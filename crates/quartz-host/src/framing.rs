//! Native Messaging length-prefixed stdio framing (05 §2.2).

use std::io::{self, Read, Write};

use quartz_protocol::BROWSER_MAX_FRAME_LEN;
use snafu::{ResultExt, Snafu};

/// Length of the Native Messaging little-endian `u32` byte-count header.
pub(crate) const FRAME_HEADER_LEN: usize = size_of::<u32>();

/// Maximum message size permitted from a browser by the Chrome Native Messaging spec: 1 MiB.
pub(crate) const NATIVE_MAX_FRAME_LEN: usize = 1024 * 1024;

/// Maximum JSON payload length accepted by the Quartz browser protocol: 256 KiB (05 §2.2).
pub(crate) const HOST_MAX_FRAME_LEN: usize = BROWSER_MAX_FRAME_LEN;

/// Failure reading a Native Messaging frame from stdin.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum ReadNativeFrameError {
    /// Declared payload length exceeds the 256 KiB browser protocol limit.
    #[snafu(display("native message length {declared_len} exceeds {HOST_MAX_FRAME_LEN} bytes"))]
    FrameTooLarge {
        /// Declared length read from the four-byte header.
        declared_len: usize,
    },
    /// Stream ended mid-header after reading at least one byte.
    #[snafu(display("native message header was truncated at EOF"))]
    TruncatedHeader,
    /// Stream ended before reading the declared payload bytes.
    #[snafu(display("native message payload was truncated at EOF"))]
    TruncatedPayload {
        /// Original I/O failure.
        source: io::Error,
    },
    /// Underlying read failure.
    #[snafu(display("could not read native message"))]
    Io {
        /// Original I/O failure.
        source: io::Error,
    },
}

/// Failure encoding or writing a Native Messaging frame to stdout.
#[derive(Debug, Snafu)]
#[snafu(module)]
pub(crate) enum WriteNativeFrameError {
    /// Outgoing payload exceeds the Native Messaging frame ceiling.
    #[snafu(display("outgoing frame length {payload_len} exceeds {NATIVE_MAX_FRAME_LEN} bytes"))]
    PayloadTooLarge {
        /// Actual payload length in bytes.
        payload_len: usize,
    },
    /// Underlying write or flush failure.
    #[snafu(display("could not write native message"))]
    Io {
        /// Original I/O failure.
        source: io::Error,
    },
}

/// Reads one length-prefixed Native Messaging payload, checking length before allocation.
///
/// Returns `Ok(None)` on a clean EOF at a frame boundary.
///
/// # Errors
/// Rejects frames exceeding [`HOST_MAX_FRAME_LEN`], truncated headers/payloads, and I/O failures.
pub(crate) fn read_native_frame<R: Read>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>, ReadNativeFrameError> {
    let mut header = [0u8; FRAME_HEADER_LEN];
    let mut filled = 0;
    while filled < FRAME_HEADER_LEN {
        match reader.read(&mut header[filled..]) {
            Ok(0) => {
                return if filled == 0 {
                    Ok(None)
                } else {
                    Err(ReadNativeFrameError::TruncatedHeader)
                };
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => return Err(ReadNativeFrameError::Io { source }),
        }
    }
    let declared_len = usize::try_from(u32::from_le_bytes(header)).map_err(|_| {
        ReadNativeFrameError::FrameTooLarge {
            declared_len: usize::MAX,
        }
    })?;
    if declared_len > HOST_MAX_FRAME_LEN {
        return Err(ReadNativeFrameError::FrameTooLarge { declared_len });
    }
    let mut payload = vec![0u8; declared_len];
    reader.read_exact(&mut payload).map_err(|source| {
        if source.kind() == io::ErrorKind::UnexpectedEof {
            ReadNativeFrameError::TruncatedPayload { source }
        } else {
            ReadNativeFrameError::Io { source }
        }
    })?;
    Ok(Some(payload))
}

/// Encodes a payload with its four-byte little-endian Native Messaging header.
///
/// # Errors
/// Rejects payloads exceeding [`NATIVE_MAX_FRAME_LEN`].
pub(crate) fn encode_native_frame(payload: &[u8]) -> Result<Vec<u8>, WriteNativeFrameError> {
    if payload.len() > NATIVE_MAX_FRAME_LEN {
        return Err(WriteNativeFrameError::PayloadTooLarge {
            payload_len: payload.len(),
        });
    }
    let header = u32::try_from(payload.len())
        .map_err(|_| WriteNativeFrameError::PayloadTooLarge {
            payload_len: payload.len(),
        })?
        .to_le_bytes();
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Writes and flushes one length-prefixed Native Messaging frame.
///
/// # Errors
/// Rejects oversized payloads or underlying I/O errors.
pub(crate) fn write_native_frame<W: Write>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), WriteNativeFrameError> {
    let frame = encode_native_frame(payload)?;
    writer
        .write_all(&frame)
        .context(write_native_frame_error::IoSnafu)?;
    writer.flush().context(write_native_frame_error::IoSnafu)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn native_framing_round_trips_multiple_messages_and_eof() {
        let mut wire = Vec::new();
        let first = br#"{"t":"req","id":1}"#;
        let second = "hello 世界 😀".as_bytes();
        write_native_frame(&mut wire, first).expect("write first");
        write_native_frame(&mut wire, second).expect("write second");

        let mut cursor = Cursor::new(wire);
        assert_eq!(
            read_native_frame(&mut cursor).expect("read first"),
            Some(first.to_vec())
        );
        assert_eq!(
            read_native_frame(&mut cursor).expect("read second"),
            Some(second.to_vec())
        );
        assert_eq!(read_native_frame(&mut cursor).expect("clean eof"), None);
    }

    #[test]
    fn native_framing_accepts_exact_256_kib_and_rejects_oversize_before_reading_payload() {
        let exact = vec![b'a'; HOST_MAX_FRAME_LEN];
        let framed = encode_native_frame(&exact).expect("encode 256 KiB");
        assert_eq!(
            &framed[..FRAME_HEADER_LEN],
            &u32::try_from(HOST_MAX_FRAME_LEN)
                .expect("256 KiB fits u32")
                .to_le_bytes()
        );
        let mut cursor = Cursor::new(framed);
        assert_eq!(
            read_native_frame(&mut cursor).expect("read 256 KiB"),
            Some(exact)
        );

        for declared in [HOST_MAX_FRAME_LEN + 1, NATIVE_MAX_FRAME_LEN + 1] {
            let header = u32::try_from(declared)
                .expect("test length fits u32")
                .to_le_bytes();
            let mut cursor = Cursor::new(header);
            let error = read_native_frame(&mut cursor).expect_err("reject oversize header");
            assert!(
                matches!(
                    error,
                    ReadNativeFrameError::FrameTooLarge { declared_len } if declared_len == declared
                ),
                "unexpected error for {declared}: {error:?}"
            );
        }
    }

    #[test]
    fn native_framing_rejects_truncated_headers_and_payloads() {
        let mut partial_header = Cursor::new([1u8, 0]);
        assert!(matches!(
            read_native_frame(&mut partial_header),
            Err(ReadNativeFrameError::TruncatedHeader)
        ));

        let mut partial_payload = Cursor::new(vec![4, 0, 0, 0, b'{', b'}']);
        assert!(matches!(
            read_native_frame(&mut partial_payload),
            Err(ReadNativeFrameError::TruncatedPayload { .. })
        ));
    }
}
