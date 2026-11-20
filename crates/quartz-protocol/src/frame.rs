//! Length-prefixed frames, independent of transport and JSON schemas.

use serde::{Serialize, de::DeserializeOwned};
use snafu::Snafu;

/// Maximum JSON payload length: 256 KiB, as specified in docs/design/04-ipc.md §2.
pub const MAX_FRAME_LEN: usize = 256 * 1024;
/// A frame starts with a little-endian u32 byte length (04 §2).
const HEADER_LEN: usize = size_of::<u32>();

/// Raw payload bytes; JSON validation happens only in [`Frame::decode_json`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    payload: Vec<u8>,
}

impl Frame {
    /// Wraps raw bytes, including empty or invalid JSON payloads.
    #[must_use]
    pub fn new(payload: Vec<u8>) -> Self {
        Self { payload }
    }

    /// Borrows the payload without its length prefix.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Serializes JSON into a frame, checking its encoded byte length.
    ///
    /// # Errors
    /// Returns a JSON serialization error or an oversized-payload error.
    pub fn from_json<T: Serialize>(value: &T) -> Result<Self, EncodeFrameError> {
        let payload =
            serde_json::to_vec(value).map_err(|source| EncodeFrameError::Json { source })?;
        Self::check_len(payload.len())?;
        Ok(Self::new(payload))
    }

    /// Produces the little-endian header followed by the unchanged payload.
    ///
    /// # Errors
    /// Rejects payloads larger than [`MAX_FRAME_LEN`].
    pub fn encode(&self) -> Result<Vec<u8>, EncodeFrameError> {
        Self::check_len(self.payload.len())?;
        let declared_len = u32::try_from(self.payload.len())
            .map_err(|source| EncodeFrameError::Length { source })?;
        let mut bytes = Vec::with_capacity(HEADER_LEN + self.payload.len());
        bytes.extend_from_slice(&declared_len.to_le_bytes());
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// Parses UTF-8 JSON with serde_json's default nesting limit enabled.
    ///
    /// # Errors
    /// Rejects invalid UTF-8, invalid JSON, excessive nesting, and schema mismatches.
    pub fn decode_json<T: DeserializeOwned>(&self) -> Result<T, DecodeJsonError> {
        let text = std::str::from_utf8(&self.payload)
            .map_err(|source| DecodeJsonError::Utf8 { source })?;
        serde_json::from_str(text).map_err(|source| DecodeJsonError::Json { source })
    }

    fn check_len(payload_len: usize) -> Result<(), EncodeFrameError> {
        if payload_len > MAX_FRAME_LEN {
            return Err(EncodeFrameError::TooLarge { payload_len });
        }
        Ok(())
    }
}

/// Encoding failed before a frame could be sent.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum EncodeFrameError {
    /// The serialized payload exceeds the protocol limit.
    #[snafu(display("payload length {payload_len} exceeds {MAX_FRAME_LEN}"))]
    TooLarge {
        /// Actual byte length before framing.
        payload_len: usize,
    },
    /// JSON serialization failed.
    #[snafu(display("cannot serialize frame JSON: {source}"))]
    Json {
        /// Underlying serialization failure.
        source: serde_json::Error,
    },
    /// The length cannot be represented in the wire header.
    #[snafu(display("cannot encode frame length: {source}"))]
    Length {
        /// Underlying checked conversion failure.
        source: std::num::TryFromIntError,
    },
}

/// Framing failed; the connection must close and this decoder cannot be reused.
#[derive(Debug, Clone, PartialEq, Eq, Snafu)]
#[snafu(module)]
pub enum DecodeFrameError {
    /// A header declared more bytes than the protocol allows.
    #[snafu(display("declared frame length {declared_len} exceeds {MAX_FRAME_LEN}"))]
    TooLarge {
        /// Untrusted length read from the wire, before any payload allocation.
        declared_len: u32,
    },
    /// A prior header error permanently disabled this decoder.
    #[snafu(display("decoder closed after frame length {declared_len}"))]
    Poisoned {
        /// Length responsible for closing the decoder.
        declared_len: u32,
    },
    /// The local platform cannot represent the declared byte length.
    #[snafu(display("cannot represent frame length {declared_len}"))]
    Length {
        /// Length read from the wire.
        declared_len: u32,
        /// Underlying checked conversion failure.
        source: std::num::TryFromIntError,
    },
}

/// Payload decoding failed independently of the frame boundary.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub enum DecodeJsonError {
    /// Payload bytes are not UTF-8.
    #[snafu(display("invalid UTF-8 frame: {source}"))]
    Utf8 {
        /// Underlying UTF-8 validation failure.
        source: std::str::Utf8Error,
    },
    /// JSON syntax, nesting depth, or the requested schema is invalid.
    #[snafu(display("invalid frame JSON: {source}"))]
    Json {
        /// Underlying JSON validation failure.
        source: serde_json::Error,
    },
}

#[derive(Debug, Default)]
enum DecodeState {
    #[default]
    Header,
    Payload {
        expected_len: usize,
    },
    Poisoned {
        declared_len: u32,
    },
}

/// Incremental frame parser with a permanent poisoned state after invalid headers.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    state: DecodeState,
    header: [u8; HEADER_LEN],
    header_len: usize,
    payload: Vec<u8>,
}

impl FrameDecoder {
    /// Creates an empty decoder without allocating a payload buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes bytes through at most one complete frame, leaving subsequent bytes in `chunk`.
    ///
    /// Call repeatedly while the chunk is nonempty. `None` means more input is needed.
    /// Successfully returned frames remain available even if a later header is invalid.
    ///
    /// # Errors
    /// An oversized or unrepresentable header poisons the decoder before allocating its payload.
    /// Every subsequent call returns [`DecodeFrameError::Poisoned`].
    pub fn feed(&mut self, chunk: &mut &[u8]) -> Result<Option<Frame>, DecodeFrameError> {
        loop {
            match self.state {
                DecodeState::Poisoned { declared_len } => {
                    return Err(DecodeFrameError::Poisoned { declared_len });
                }
                DecodeState::Header => {
                    let take = (HEADER_LEN - self.header_len).min(chunk.len());
                    self.header[self.header_len..self.header_len + take]
                        .copy_from_slice(&chunk[..take]);
                    self.header_len += take;
                    *chunk = &chunk[take..];
                    if self.header_len < HEADER_LEN {
                        return Ok(None);
                    }
                    let declared_len = u32::from_le_bytes(self.header);
                    // Poison first: every header failure leaves a permanently closed decoder.
                    self.state = DecodeState::Poisoned { declared_len };
                    let expected_len = usize::try_from(declared_len).map_err(|source| {
                        DecodeFrameError::Length {
                            declared_len,
                            source,
                        }
                    })?;
                    if expected_len > MAX_FRAME_LEN {
                        return Err(DecodeFrameError::TooLarge { declared_len });
                    }
                    self.payload.reserve_exact(expected_len);
                    self.state = DecodeState::Payload { expected_len };
                }
                DecodeState::Payload { expected_len } => {
                    let take = (expected_len - self.payload.len()).min(chunk.len());
                    self.payload.extend_from_slice(&chunk[..take]);
                    *chunk = &chunk[take..];
                    if self.payload.len() < expected_len {
                        return Ok(None);
                    }
                    self.state = DecodeState::Header;
                    self.header_len = 0;
                    return Ok(Some(Frame::new(std::mem::take(&mut self.payload))));
                }
            }
        }
    }

    /// Reports an incomplete header or payload to log before dropping a closed connection.
    ///
    /// Dropping the decoder discards any partial frame. A rejected header is an error,
    /// rather than a partial frame.
    #[must_use]
    pub fn has_partial_frame(&self) -> bool {
        match self.state {
            DecodeState::Header => self.header_len != 0,
            DecodeState::Payload { .. } => true,
            DecodeState::Poisoned { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_checked_before_payload_allocation_and_poison_is_permanent() {
        let declared_len = u32::try_from(MAX_FRAME_LEN + 1).expect("limit fits u32");
        let mut bytes = declared_len.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"ignored payload");
        let mut decoder = FrameDecoder::new();
        let mut chunk = bytes.as_slice();
        assert_eq!(
            decoder.feed(&mut chunk),
            Err(DecodeFrameError::TooLarge { declared_len })
        );
        assert_eq!(decoder.payload.capacity(), 0);
        assert_eq!(chunk, b"ignored payload");
        assert!(matches!(decoder.state, DecodeState::Poisoned { .. }));
        assert_eq!(
            decoder.feed(&mut chunk),
            Err(DecodeFrameError::Poisoned { declared_len })
        );
        assert!(!decoder.has_partial_frame());
    }

    #[test]
    fn exact_limit_and_empty_frames_round_trip() {
        for payload in [vec![b' '; MAX_FRAME_LEN], Vec::new(), b"{}".to_vec()] {
            let frame = Frame::new(payload);
            let wire = frame.encode().expect("within limit");
            let mut chunk = wire.as_slice();
            let mut decoder = FrameDecoder::new();
            assert_eq!(decoder.feed(&mut chunk).expect("valid header"), Some(frame));
            assert!(chunk.is_empty());
            assert!(!decoder.has_partial_frame());
        }
        assert!(matches!(
            Frame::new(vec![0; MAX_FRAME_LEN + 1]).encode(),
            Err(EncodeFrameError::TooLarge { .. })
        ));
        assert!(matches!(
            Frame::from_json(&"x".repeat(MAX_FRAME_LEN)),
            Err(EncodeFrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn split_header_and_single_byte_payload_reads() {
        let frame = Frame::from_json(&serde_json::json!({"hello": "世界"})).expect("serialize");
        let wire = frame.encode().expect("encode");
        let mut decoder = FrameDecoder::new();
        assert_eq!(decoder.feed(&mut &wire[..1]).expect("first byte"), None);
        assert!(decoder.has_partial_frame());
        assert_eq!(
            decoder
                .feed(&mut &wire[1..HEADER_LEN])
                .expect("rest of header"),
            None
        );
        assert!(decoder.has_partial_frame());
        for byte in &wire[HEADER_LEN..wire.len() - 1] {
            assert_eq!(
                decoder
                    .feed(&mut std::slice::from_ref(byte))
                    .expect("payload byte"),
                None
            );
        }
        assert_eq!(
            decoder
                .feed(&mut &wire[wire.len() - 1..])
                .expect("last byte"),
            Some(frame)
        );
        assert!(!decoder.has_partial_frame());
    }

    #[test]
    fn several_frames_and_a_truncated_tail() {
        let frames = [
            Frame::new(Vec::new()),
            Frame::new(b"{}".to_vec()),
            Frame::new(b"[]".to_vec()),
        ];
        let mut wire = Vec::new();
        for frame in &frames {
            wire.extend(frame.encode().expect("encode"));
        }
        wire.extend_from_slice(&[2, 0]);
        let mut decoder = FrameDecoder::new();
        let mut chunk = wire.as_slice();
        for frame in frames {
            assert_eq!(decoder.feed(&mut chunk).expect("decode"), Some(frame));
        }
        assert_eq!(decoder.feed(&mut chunk).expect("partial header"), None);
        assert!(decoder.has_partial_frame());
    }

    #[test]
    fn json_validation_is_separate_and_depth_limited() {
        assert!(matches!(
            Frame::new(vec![0xff]).decode_json::<serde_json::Value>(),
            Err(DecodeJsonError::Utf8 { .. })
        ));
        assert!(
            Frame::new(Vec::new())
                .decode_json::<serde_json::Value>()
                .is_err()
        );
        let deep = format!("{}0{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(matches!(
            Frame::new(deep.into_bytes()).decode_json::<serde_json::Value>(),
            Err(DecodeJsonError::Json { .. })
        ));
        let truncated = Frame::new(b"{}".to_vec()).encode().expect("encode");
        let mut decoder = FrameDecoder::new();
        assert!(
            decoder
                .feed(&mut &truncated[..truncated.len() - 1])
                .expect("partial payload")
                .is_none()
        );
        assert!(decoder.has_partial_frame());
    }
}
