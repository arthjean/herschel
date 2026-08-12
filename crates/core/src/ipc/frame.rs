// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Newline-delimited JSON framing, with a hard ceiling on both ends.
//!
//! The transport, kept apart from the vocabulary it carries. Nothing here knows
//! what a [`Request`] means; it moves bytes and refuses the two ways a peer can
//! make that unsafe, which is an endless line and a frame that is not JSON.
//!
//! [`Request`]: super::Request

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use super::IpcError;

/// Largest frame either side will read.
///
/// A capability record with both devices and every `hwmon` attribute stays
/// well under this. The ceiling exists so a peer cannot force unbounded
/// allocation in the daemon.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// A framing failure, distinct from a protocol-level [`IpcError`].
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("connection closed")]
    Closed,
    #[error("frame exceeds {max_bytes} bytes")]
    TooLarge { max_bytes: usize },
    #[error("frame is not valid JSON: {0}")]
    Decode(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl FrameError {
    /// The typed protocol error a daemon answers with, when it can answer.
    pub fn as_ipc_error(&self) -> Option<IpcError> {
        match self {
            Self::Closed => None,
            Self::TooLarge { max_bytes } => Some(IpcError::FrameTooLarge {
                max_bytes: *max_bytes,
            }),
            Self::Decode(detail) => Some(IpcError::Malformed {
                detail: detail.clone(),
            }),
            Self::Io(error) => Some(IpcError::Io {
                detail: error.to_string(),
            }),
        }
    }
}

/// Write one newline-delimited JSON frame.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), FrameError> {
    let mut encoded = serde_json::to_vec(value).map_err(|e| FrameError::Decode(e.to_string()))?;
    if encoded.len() + 1 > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            max_bytes: MAX_FRAME_BYTES,
        });
    }
    encoded.push(b'\n');
    writer.write_all(&encoded)?;
    writer.flush()?;
    Ok(())
}

/// Read one newline-delimited JSON frame, refusing anything oversized.
///
/// The length ceiling is enforced while reading, not after, so an endless line
/// cannot exhaust memory before it is rejected.
pub fn read_frame<R: BufRead, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T, FrameError> {
    let mut buffer = Vec::new();
    let mut limited = std::io::Read::take(&mut *reader, MAX_FRAME_BYTES as u64);
    let read = limited.read_until(b'\n', &mut buffer)?;
    if read == 0 {
        return Err(FrameError::Closed);
    }
    if buffer.last() != Some(&b'\n') {
        // Either the ceiling was hit mid-line, or the peer vanished mid-frame.
        // Both leave the stream unusable, so the caller must close it.
        return if read >= MAX_FRAME_BYTES {
            Err(FrameError::TooLarge {
                max_bytes: MAX_FRAME_BYTES,
            })
        } else {
            Err(FrameError::Closed)
        };
    }
    buffer.pop();
    serde_json::from_slice(&buffer).map_err(|e| FrameError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{IpcError, PROTOCOL_VERSION, Request};
    use std::io::BufReader;

    #[test]
    fn frames_round_trip() {
        let mut buffer = Vec::new();
        let request = Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut buffer, &request).unwrap();
        assert_eq!(buffer.last(), Some(&b'\n'));

        let mut reader = BufReader::new(buffer.as_slice());
        let decoded: Request = read_frame(&mut reader).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn unknown_request_variant_is_a_decode_error() {
        let mut reader = BufReader::new(&b"{\"request\":\"format_disk\"}\n"[..]);
        let result: Result<Request, _> = read_frame(&mut reader);
        assert!(matches!(result, Err(FrameError::Decode(_))));
    }

    #[test]
    fn unknown_payload_field_is_rejected_instead_of_ignored() {
        let frame = br#"{"request":"save_profile","profile":{"name":"x","program":{"mode":"onboard"},"device":null,"force_detach":true}}
"#;
        let mut reader = BufReader::new(&frame[..]);
        let result: Result<Request, _> = read_frame(&mut reader);
        assert!(matches!(result, Err(FrameError::Decode(_))), "{result:?}");
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let mut reader = BufReader::new(&b"{\"request\":\"activate_profile\"}\n"[..]);
        let result: Result<Request, _> = read_frame(&mut reader);
        assert!(matches!(result, Err(FrameError::Decode(_))), "{result:?}");
    }

    #[test]
    fn wrongly_typed_field_is_rejected() {
        let mut reader =
            BufReader::new(&b"{\"request\":\"hello\",\"protocol_version\":\"one\"}\n"[..]);
        let result: Result<Request, _> = read_frame(&mut reader);
        assert!(matches!(result, Err(FrameError::Decode(_))), "{result:?}");
    }

    #[test]
    fn oversized_frame_is_refused_without_buffering_it_all() {
        let mut line = vec![b'x'; MAX_FRAME_BYTES + 4096];
        line.push(b'\n');
        let mut reader = BufReader::new(line.as_slice());
        let result: Result<Request, _> = read_frame(&mut reader);
        match result {
            Err(FrameError::TooLarge { max_bytes }) => assert_eq!(max_bytes, MAX_FRAME_BYTES),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn closed_connection_is_distinguished_from_a_bad_frame() {
        let mut reader = BufReader::new(&b""[..]);
        let result: Result<Request, _> = read_frame(&mut reader);
        assert!(matches!(result, Err(FrameError::Closed)));
        assert!(result.unwrap_err().as_ipc_error().is_none());
    }

    #[test]
    fn truncated_json_is_a_typed_malformed_error() {
        let mut reader = BufReader::new(&b"{\"request\":\n"[..]);
        let result: Result<Request, _> = read_frame(&mut reader);
        let error = result.unwrap_err().as_ipc_error().unwrap();
        assert!(matches!(error, IpcError::Malformed { .. }));
    }
}
