// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The client side of the local socket.
//!
//! Lives next to the protocol so the GPUI process and the integration tests
//! speak it through the same code, including the frame ceiling.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::ipc::{
    DaemonStatus, FrameError, IpcError, PROTOCOL_VERSION, Request, Response, read_frame,
    write_frame,
};

/// Why talking to the daemon failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the daemon is not running, or its socket is not reachable at {path}: {source}")]
    Unreachable {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the connection to the daemon was lost: {0}")]
    Disconnected(FrameError),
    #[error("{0}")]
    Refused(IpcError),
    #[error("the daemon answered {actual} where {expected} was expected")]
    Unexpected {
        expected: &'static str,
        actual: &'static str,
    },
}

impl ClientError {
    /// One sentence a control surface can display verbatim.
    pub fn operator_message(&self) -> String {
        match self {
            Self::Unreachable { .. } => {
                "The background service is not running. Start korid to enable controls.".to_string()
            }
            other => other.to_string(),
        }
    }

    /// A framing failure on the way out.
    ///
    /// A request refused for its size never left this process, so it is a typed
    /// refusal and not a lost connection. Reporting it as a disconnection would
    /// describe something that did not happen and send the operator looking for
    /// a daemon that is still there.
    fn from_write(error: FrameError) -> Self {
        match error {
            FrameError::TooLarge { max_bytes } => {
                Self::Refused(IpcError::FrameTooLarge { max_bytes })
            }
            other => Self::Disconnected(other),
        }
    }
}

/// A connected client that has completed the version handshake.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    daemon_version: String,
}

impl Client {
    /// Connect and handshake.
    pub fn connect(path: &Path) -> Result<Self, ClientError> {
        let unreachable = |source| ClientError::Unreachable {
            path: path.display().to_string(),
            source,
        };

        let stream = UnixStream::connect(path).map_err(unreachable)?;
        // A hung daemon must not freeze the caller's thread forever. The
        // failure is reported rather than discarded: a connection whose
        // timeouts were refused does not carry that guarantee, and a caller
        // handed one anyway would wait on it without end.
        let timeout = Some(Duration::from_secs(5));
        stream.set_read_timeout(timeout).map_err(unreachable)?;
        stream.set_write_timeout(timeout).map_err(unreachable)?;

        let write_half = stream.try_clone().map_err(unreachable)?;
        let mut reader = BufReader::new(stream);
        let mut writer = BufWriter::new(write_half);

        // The handshake runs on the streams themselves, before a `Client`
        // exists, so the type is never built around a daemon version nobody
        // has read yet.
        let hello = Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut writer, &hello).map_err(ClientError::from_write)?;
        let daemon_version = match read_frame(&mut reader).map_err(ClientError::Disconnected)? {
            Response::Hello { daemon_version, .. } => daemon_version,
            Response::Error(error) => return Err(ClientError::Refused(error)),
            other => {
                return Err(ClientError::Unexpected {
                    expected: "hello",
                    actual: other.kind(),
                });
            }
        };

        Ok(Self {
            reader,
            writer,
            daemon_version,
        })
    }

    pub fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    /// Send one request and read its response.
    pub fn request(&mut self, request: Request) -> Result<Response, ClientError> {
        write_frame(&mut self.writer, &request).map_err(ClientError::from_write)?;
        read_frame(&mut self.reader).map_err(ClientError::Disconnected)
    }

    /// Send one request and take the single response shape it accepts.
    ///
    /// A typed refusal comes back as one. Anything else names both what was
    /// expected and what arrived: an error that can only say "other response"
    /// is one an operator cannot act on and a maintainer cannot trace.
    fn expect<T>(
        &mut self,
        request: Request,
        expected: &'static str,
        take: impl FnOnce(Response) -> Option<T>,
    ) -> Result<T, ClientError> {
        match self.request(request)? {
            Response::Error(error) => Err(ClientError::Refused(error)),
            other => {
                let actual = other.kind();
                take(other).ok_or(ClientError::Unexpected { expected, actual })
            }
        }
    }

    /// Current daemon status.
    pub fn status(&mut self) -> Result<DaemonStatus, ClientError> {
        self.expect(Request::Status, "status", |response| match response {
            Response::Status(status) => Some(*status),
            _ => None,
        })
    }

    /// The versioned capability record.
    pub fn capabilities(&mut self) -> Result<crate::capability::CapabilityRecord, ClientError> {
        self.expect(
            Request::Capabilities,
            "capabilities",
            |response| match response {
                Response::Capabilities(record) => Some(*record),
                _ => None,
            },
        )
    }

    /// The most recent sampling pass.
    pub fn telemetry(&mut self) -> Result<crate::telemetry::TelemetrySnapshot, ClientError> {
        self.expect(Request::Telemetry, "telemetry", |response| match response {
            Response::Telemetry(snapshot) => Some(*snapshot),
            _ => None,
        })
    }

    /// Apply a cooling program and read what the hardware confirmed.
    pub fn apply(
        &mut self,
        program: crate::profile::CoolingProgram,
    ) -> Result<crate::ipc::ApplyOutcome, ClientError> {
        self.expect(
            Request::ApplyProgram { program },
            "applied",
            |response| match response {
                Response::Applied(outcome) => Some(*outcome),
                _ => None,
            },
        )
    }

    /// Apply a lighting program to one channel.
    pub fn apply_lighting(
        &mut self,
        command: crate::lighting::LightingCommand,
    ) -> Result<crate::ipc::LightingOutcome, ClientError> {
        self.expect(
            Request::ApplyLighting { command },
            "lit",
            |response| match response {
                Response::Lit(outcome) => Some(outcome),
                _ => None,
            },
        )
    }

    /// Show a preset on the Kraken's panel.
    pub fn apply_display(
        &mut self,
        preset: crate::display::DisplayPreset,
    ) -> Result<crate::ipc::DisplayOutcome, ClientError> {
        self.expect(
            Request::ApplyDisplay { preset },
            "shown",
            |response| match response {
                Response::Shown(outcome) => Some(*outcome),
                _ => None,
            },
        )
    }

    /// Every stored profile plus the active one.
    pub fn profiles(&mut self) -> Result<(String, Vec<crate::profile::Profile>), ClientError> {
        self.expect(Request::Profiles, "profiles", |response| match response {
            Response::Profiles { active, profiles } => Some((active, profiles)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message an operator reads has to name what actually arrived. The
    /// field used to be the constant "other response" on every site, which
    /// produced a sentence that was both ungrammatical and empty.
    #[test]
    fn an_unexpected_answer_names_what_arrived_and_what_was_wanted() {
        let error = ClientError::Unexpected {
            expected: "status",
            actual: Response::Saved {
                name: "Silent".into(),
            }
            .kind(),
        };
        let message = error.to_string();
        assert!(message.contains("saved"), "{message}");
        assert!(message.contains("status"), "{message}");
        assert_eq!(error.operator_message(), message);
    }

    /// A request refused for its size never reached the socket. Calling that a
    /// lost connection would send the operator to restart a daemon that is
    /// running and would have answered.
    #[test]
    fn an_oversized_request_is_a_refusal_rather_than_a_disconnection() {
        let refused = ClientError::from_write(FrameError::TooLarge {
            max_bytes: crate::ipc::MAX_FRAME_BYTES,
        });
        match refused {
            ClientError::Refused(IpcError::FrameTooLarge { max_bytes }) => {
                assert_eq!(max_bytes, crate::ipc::MAX_FRAME_BYTES);
            }
            other => panic!("expected a typed refusal, got {other:?}"),
        }

        // A genuine transport failure still reads as a lost connection.
        let lost = ClientError::from_write(FrameError::Closed);
        assert!(matches!(
            lost,
            ClientError::Disconnected(FrameError::Closed)
        ));
    }

    /// The one message the client rewrites, because a raw connection error
    /// tells an operator nothing about what to do next.
    #[test]
    fn an_unreachable_daemon_is_explained_in_operator_language() {
        let error = ClientError::Unreachable {
            path: "/run/user/1000/kori/kori.sock".into(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        let message = error.operator_message();
        assert!(message.contains("korid"), "{message}");
        assert!(!message.contains("/run/user"), "{message}");
    }
}
