//! Size-checked JSON payloads and optional length-delimited transport configuration.

use serde::{Serialize, de::DeserializeOwned};
use snafu::Snafu;

/// Maximum JSON payload length: 1 MiB.
///
/// The local named pipe connects UI/CLI and core on the same machine; 1 MiB fits
/// full single-response query payloads while keeping per-connection buffers small.
pub const MAX_FRAME_LEN: usize = 1024 * 1024;

/// Encodes a JSON payload without its transport header, enforcing the 04 §2 size limit.
///
/// # Errors
/// Returns the serialization source or the encoded length when the payload is too large.
pub fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, EncodeFrameError> {
    let payload = serde_json::to_vec(value).map_err(|source| EncodeFrameError::Json { source })?;
    if payload.len() > MAX_FRAME_LEN {
        return Err(EncodeFrameError::TooLarge {
            payload_len: payload.len(),
        });
    }
    Ok(payload)
}

/// Decodes a UTF-8 JSON payload using serde_json's default nesting limit.
///
/// # Errors
/// Rejects invalid UTF-8, JSON syntax, excessive nesting, and schema mismatches.
pub fn decode_json<T: DeserializeOwned>(payload: &[u8]) -> Result<T, DecodeJsonError> {
    let text = std::str::from_utf8(payload).map_err(|source| DecodeJsonError::Utf8 { source })?;
    serde_json::from_str(text).map_err(|source| DecodeJsonError::Json { source })
}

/// Constructs the shared codec for 04 §2: a stripped four-byte little-endian length prefix.
#[cfg(feature = "codec")]
#[must_use]
pub fn frame_codec() -> tokio_util::codec::LengthDelimitedCodec {
    /// The 04 §2 length field is a little-endian u32 and excludes the header itself.
    const HEADER_LEN: usize = size_of::<u32>();
    tokio_util::codec::LengthDelimitedCodec::builder()
        .length_field_length(HEADER_LEN)
        .little_endian()
        .length_adjustment(0)
        .num_skip(HEADER_LEN)
        .max_frame_length(MAX_FRAME_LEN)
        .new_codec()
}

/// JSON encoding failed before a payload could be sent.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum EncodeFrameError {
    /// The serialized payload exceeds the protocol limit.
    #[snafu(display("payload length {payload_len} exceeds {MAX_FRAME_LEN}"))]
    TooLarge {
        /// Actual JSON payload length, excluding the transport header.
        payload_len: usize,
    },
    /// JSON serialization failed.
    #[snafu(display("cannot serialize frame JSON: {source}"))]
    Json {
        /// Original serialization failure.
        source: serde_json::Error,
    },
}

/// Payload decoding failed independently of the frame boundary.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum DecodeJsonError {
    /// Payload bytes are not UTF-8.
    #[snafu(display("invalid UTF-8 frame: {source}"))]
    Utf8 {
        /// Original UTF-8 validation failure.
        source: std::str::Utf8Error,
    },
    /// JSON syntax, nesting depth, or schema is invalid.
    #[snafu(display("invalid frame JSON: {source}"))]
    Json {
        /// Original JSON validation failure.
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_validation_is_separate_and_depth_limited() {
        assert!(matches!(
            decode_json::<serde_json::Value>(&[0xff]),
            Err(DecodeJsonError::Utf8 { .. })
        ));
        assert!(decode_json::<serde_json::Value>(&[]).is_err());
        let deep = format!("{}0{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(matches!(
            decode_json::<serde_json::Value>(deep.as_bytes()),
            Err(DecodeJsonError::Json { .. })
        ));
        assert!(matches!(
            encode_json(&"x".repeat(MAX_FRAME_LEN)),
            Err(EncodeFrameError::TooLarge { .. })
        ));
        let value = serde_json::json!({"hello":"世界"});
        assert_eq!(
            decode_json::<serde_json::Value>(&encode_json(&value).expect("JSON bytes"))
                .expect("JSON value"),
            value
        );
    }

    #[cfg(feature = "codec")]
    mod codec {
        use super::*;
        use tokio_util::{
            bytes::BytesMut,
            codec::{Decoder, Encoder, LengthDelimitedCodecError},
        };

        fn wire(payload: &[u8]) -> BytesMut {
            let mut buffer = BytesMut::new();
            frame_codec()
                .encode(payload, &mut buffer)
                .expect("encode frame");
            buffer
        }
        #[test]
        fn exact_limit_and_zero_length_frames_are_accepted() {
            for payload in [vec![b' '; MAX_FRAME_LEN], Vec::new()] {
                let mut buffer = wire(&payload);
                assert_eq!(
                    frame_codec()
                        .decode(&mut buffer)
                        .expect("decode")
                        .expect("complete frame")
                        .as_ref(),
                    payload.as_slice()
                );
                assert!(buffer.is_empty());
            }
        }
        #[test]
        fn oversized_header_is_rejected_before_payload_reservation() {
            let declared = u32::try_from(MAX_FRAME_LEN + 1).expect("length fits u32");
            let mut buffer = BytesMut::from(declared.to_le_bytes().as_slice());
            let capacity = buffer.capacity();
            let error = frame_codec()
                .decode(&mut buffer)
                .expect_err("reject oversize header");
            assert!(
                error
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<LengthDelimitedCodecError>())
                    .is_some()
            );
            assert_eq!(buffer.capacity(), capacity);
            assert!(buffer.capacity() < usize::try_from(declared).expect("u32 fits usize"));
        }
        #[test]
        fn header_can_arrive_in_separate_feeds_and_payload_byte_by_byte() {
            let payload = b"{\"hello\":true}";
            let bytes = wire(payload);
            let mut codec = frame_codec();
            let mut buffer = BytesMut::new();
            for byte in &bytes[..bytes.len() - 1] {
                buffer.extend_from_slice(std::slice::from_ref(byte));
                assert!(codec.decode(&mut buffer).expect("partial frame").is_none());
            }
            buffer.extend_from_slice(&bytes[bytes.len() - 1..]);
            assert_eq!(
                codec
                    .decode(&mut buffer)
                    .expect("decode")
                    .expect("complete frame")
                    .as_ref(),
                payload
            );
        }
        #[test]
        fn several_frames_and_truncated_eof_keep_their_boundaries() {
            let mut buffer = wire(b"{}");
            buffer.extend_from_slice(&wire(b"[]"));
            buffer.extend_from_slice(&[2, 0]);
            let mut codec = frame_codec();
            assert_eq!(
                codec
                    .decode(&mut buffer)
                    .expect("first")
                    .expect("frame")
                    .as_ref(),
                b"{}"
            );
            assert_eq!(
                codec
                    .decode(&mut buffer)
                    .expect("second")
                    .expect("frame")
                    .as_ref(),
                b"[]"
            );
            let error = codec
                .decode_eof(&mut buffer)
                .expect_err("partial header at EOF");
            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert!(
                error
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<LengthDelimitedCodecError>())
                    .is_none()
            );
            let mut payload_tail = wire(b"{}");
            payload_tail.truncate(payload_tail.len() - 1);
            assert_eq!(
                frame_codec()
                    .decode_eof(&mut payload_tail)
                    .expect_err("partial payload at EOF")
                    .kind(),
                std::io::ErrorKind::Other
            );
        }
        #[test]
        fn encoder_rejects_oversize_and_writes_little_endian_header() {
            let mut buffer = BytesMut::new();
            assert!(
                frame_codec()
                    .encode(vec![0; MAX_FRAME_LEN + 1].as_slice(), &mut buffer)
                    .is_err()
            );
            assert!(buffer.is_empty());
            assert_eq!(wire(b"{}").as_ref(), b"\x02\x00\x00\x00{}");
            let payload = vec![0; 256];
            assert_eq!(&wire(&payload)[..4], &[0, 1, 0, 0]);
        }
    }
}
