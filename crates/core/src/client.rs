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
    Disconnected(#[from] FrameError),
    #[error("{0}")]
    Refused(#[from] IpcError),
    #[error("the daemon answered a {actual} where a {expected} was expected")]
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
                "The background service is not running. Start nzxt-controld to enable controls."
                    .to_string()
            }
            other => other.to_string(),
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
        let stream = UnixStream::connect(path).map_err(|source| ClientError::Unreachable {
            path: path.display().to_string(),
            source,
        })?;
        // A hung daemon must not freeze the caller's thread forever.
        let timeout = Some(Duration::from_secs(5));
        let _ = stream.set_read_timeout(timeout);
        let _ = stream.set_write_timeout(timeout);

        let write_half = stream
            .try_clone()
            .map_err(|source| ClientError::Unreachable {
                path: path.display().to_string(),
                source,
            })?;

        let mut client = Self {
            reader: BufReader::new(stream),
            writer: BufWriter::new(write_half),
            daemon_version: String::new(),
        };

        match client.request(Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        })? {
            Response::Hello { daemon_version, .. } => {
                client.daemon_version = daemon_version;
                Ok(client)
            }
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "hello",
                actual: "other response",
            }),
        }
    }

    pub fn daemon_version(&self) -> &str {
        &self.daemon_version
    }

    /// Send one request and read its response.
    pub fn request(&mut self, request: Request) -> Result<Response, ClientError> {
        write_frame(&mut self.writer, &request)?;
        Ok(read_frame(&mut self.reader)?)
    }

    /// Current daemon status.
    pub fn status(&mut self) -> Result<DaemonStatus, ClientError> {
        match self.request(Request::Status)? {
            Response::Status(status) => Ok(*status),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "status",
                actual: "other response",
            }),
        }
    }

    /// The versioned capability record.
    pub fn capabilities(&mut self) -> Result<crate::capability::CapabilityRecord, ClientError> {
        match self.request(Request::Capabilities)? {
            Response::Capabilities(record) => Ok(*record),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "capabilities",
                actual: "other response",
            }),
        }
    }

    /// The most recent sampling pass.
    pub fn telemetry(&mut self) -> Result<crate::telemetry::TelemetrySnapshot, ClientError> {
        match self.request(Request::Telemetry)? {
            Response::Telemetry(snapshot) => Ok(*snapshot),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "telemetry",
                actual: "other response",
            }),
        }
    }

    /// Apply a cooling program and read what the hardware confirmed.
    pub fn apply(
        &mut self,
        program: crate::profile::CoolingProgram,
    ) -> Result<crate::ipc::ApplyOutcome, ClientError> {
        match self.request(Request::ApplyProgram { program })? {
            Response::Applied(outcome) => Ok(*outcome),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "applied",
                actual: "other response",
            }),
        }
    }

    /// Apply a lighting program to one channel.
    pub fn apply_lighting(
        &mut self,
        command: crate::lighting::LightingCommand,
    ) -> Result<crate::ipc::LightingOutcome, ClientError> {
        match self.request(Request::ApplyLighting { command })? {
            Response::Lit(outcome) => Ok(outcome),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "lit",
                actual: "other response",
            }),
        }
    }

    /// Show a preset on the Kraken's panel.
    pub fn apply_display(
        &mut self,
        preset: crate::display::DisplayPreset,
    ) -> Result<crate::ipc::DisplayOutcome, ClientError> {
        match self.request(Request::ApplyDisplay { preset })? {
            Response::Shown(outcome) => Ok(*outcome),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "shown",
                actual: "other response",
            }),
        }
    }

    /// Every stored profile plus the active one.
    pub fn profiles(&mut self) -> Result<(String, Vec<crate::profile::Profile>), ClientError> {
        match self.request(Request::Profiles)? {
            Response::Profiles { active, profiles } => Ok((active, profiles)),
            Response::Error(error) => Err(ClientError::Refused(error)),
            _ => Err(ClientError::Unexpected {
                expected: "profiles",
                actual: "other response",
            }),
        }
    }
}
