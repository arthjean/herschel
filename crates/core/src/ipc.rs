//! The typed protocol spoken over the daemon's Unix socket.
//!
//! Framing is newline-delimited JSON with a hard length ceiling applied on both
//! ends. Anything that does not parse into [`Request`] is answered with a typed
//! [`IpcError`] and produces zero hardware writes.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::DeviceId;
use crate::capability::{CapabilityId, CapabilityRecord};
use crate::profile::{Incompatibility, Profile, ValidationError};

/// Incremented on any breaking change to [`Request`] or [`Response`].
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest frame either side will read.
///
/// A capability record with both devices and every `hwmon` attribute stays
/// well under this. The ceiling exists so a peer cannot force unbounded
/// allocation in the daemon.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Environment variable that overrides the socket path, used by tests and by
/// anyone running a second daemon against a fake sysfs root.
pub const SOCKET_PATH_ENV: &str = "NZXT_CONTROL_SOCKET";

/// Socket file name inside the per-user runtime directory.
pub const SOCKET_FILE_NAME: &str = "nzxt-control.sock";

/// Environment variable that overrides the runtime directory holding the
/// socket and the per-device locks.
pub const RUNTIME_DIR_ENV: &str = "NZXT_CONTROL_RUNTIME_DIR";

/// Directory name both processes append to a base runtime directory.
const APP_DIR: &str = "nzxt-control";

/// Resolve the runtime directory from explicit inputs.
///
/// Split from the environment so the fallback order is testable without
/// mutating a process-global variable.
///
/// The last resort is the user's own cache directory, never `/tmp`: a socket
/// in a world-writable directory can be created by another user before the
/// daemon binds, and a client reaching it would take its device list, its
/// ownership state and its capability record from whoever won that race.
pub fn runtime_dir(
    override_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    match (override_dir, xdg_runtime_dir) {
        (Some(path), _) => path.to_path_buf(),
        (None, Some(path)) => path.join(APP_DIR),
        (None, None) => home
            .unwrap_or_else(|| Path::new("."))
            .join(".cache")
            .join(APP_DIR),
    }
}

/// The runtime directory this machine resolves to.
pub fn runtime_dir_from_env() -> PathBuf {
    runtime_dir(
        env_path(RUNTIME_DIR_ENV).as_deref(),
        env_path("XDG_RUNTIME_DIR").as_deref(),
        env_path("HOME").as_deref(),
    )
}

/// The socket path this machine resolves to.
///
/// The daemon binds here and the client connects here, from this one function,
/// so the two cannot drift onto different paths.
pub fn socket_path_from_env() -> PathBuf {
    match env_path(SOCKET_PATH_ENV) {
        Some(path) => path,
        None => runtime_dir_from_env().join(SOCKET_FILE_NAME),
    }
}

/// A non-empty environment variable read as a path.
fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// A request from the GPUI client to the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// `deny_unknown_fields` is not supported on internally tagged enums, so strict
// field checking lives on the payload structs (`Profile`, `TemperatureCurve`).
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Negotiate the protocol version. Sent before anything else.
    Hello { protocol_version: u32 },
    /// Current daemon, device and ownership state.
    Status,
    /// The versioned capability record for every allowlisted device.
    Capabilities,
    /// Every stored profile plus the active one.
    Profiles,
    /// Store a profile, replacing one with the same name.
    SaveProfile { profile: Profile },
    /// Make a stored profile active.
    ActivateProfile { name: String },
    /// Remove a stored profile. The safe profile is activated first when the
    /// deleted profile is the active one.
    DeleteProfile { name: String },
    /// Redacted diagnostics for an issue report.
    Diagnostics,
}

/// A response from the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol_version: u32,
        daemon_version: String,
    },
    Status(Box<DaemonStatus>),
    Capabilities(Box<CapabilityRecord>),
    Profiles {
        active: String,
        profiles: Vec<Profile>,
    },
    Activated(ActivationOutcome),
    Saved {
        name: String,
    },
    Deleted {
        name: String,
        activated_instead: Option<String>,
    },
    Diagnostics(crate::diagnostics::DiagnosticsExport),
    Error(IpcError),
}

/// What actually happened to the hardware when a profile was activated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationOutcome {
    pub name: String,
    pub hardware: HardwareState,
}

/// How much of the hardware state the daemon can vouch for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HardwareState {
    /// The device is running its own firmware program and nothing was written.
    Onboard,
    /// The daemon wrote the program and read it back.
    Confirmed,
    /// The profile was selected but not written, and `reason` says why.
    NotApplied { reason: String },
    /// A write started and could not be confirmed. No further write is sent
    /// until a readback succeeds.
    Uncertain { reason: String },
}

/// Whether the daemon may write at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AccessMode {
    ReadWrite,
    /// Writes are refused. `conflicts` lists the evidence.
    ReadOnly {
        conflicts: Vec<OwnershipConflict>,
    },
}

impl AccessMode {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadOnly { .. })
    }
}

/// Another process or a permission problem preventing ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipConflict {
    pub device: Option<DeviceId>,
    /// Path or resource the conflict was observed on.
    pub resource: String,
    pub detail: String,
}

/// Per-device state the client renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub id: DeviceId,
    pub present: bool,
    /// The daemon holds the exclusive per-device lock.
    pub owned: bool,
    /// Capabilities that may be written right now.
    pub writable: Vec<CapabilityId>,
    /// Operator-facing reason each unavailable capability is blocked.
    pub blocked: Vec<BlockedCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedCapability {
    pub capability: CapabilityId,
    pub reason: String,
}

/// How the on-disk configuration was resolved at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConfigState {
    /// No configuration existed; built-in defaults are active.
    Defaults,
    /// A valid configuration was loaded.
    Loaded,
    /// The configuration could not be loaded. The unreadable file was kept at
    /// `preserved_path` and `recovery_action` names what the operator can do.
    Recovered {
        detail: String,
        preserved_path: String,
        recovery_action: String,
    },
}

impl ConfigState {
    /// One actionable sentence, or `None` when nothing needs attention.
    pub fn recovery_message(&self) -> Option<String> {
        match self {
            Self::Defaults | Self::Loaded => None,
            Self::Recovered {
                detail,
                preserved_path,
                recovery_action,
            } => Some(format!(
                "Configuration could not be loaded ({detail}). Safe defaults are active. \
                 The previous file was kept at {preserved_path}. {recovery_action}"
            )),
        }
    }
}

/// Everything the client needs to decide what to enable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub daemon_version: String,
    pub protocol_version: u32,
    pub access: AccessMode,
    pub devices: Vec<DeviceStatus>,
    pub active_profile: String,
    pub config: ConfigState,
    /// Path of the Unix socket. The daemon opens no other listening endpoint.
    pub socket_path: String,
}

/// Every way a request can be refused, with enough detail to render it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum IpcError {
    #[error("Client protocol {requested} is not supported. This daemon speaks {supported}.")]
    UnsupportedProtocol { requested: u32, supported: u32 },
    #[error("Request could not be parsed: {detail}")]
    Malformed { detail: String },
    #[error("Request exceeds the {max_bytes} byte frame limit.")]
    FrameTooLarge { max_bytes: usize },
    #[error("Request rejected before the protocol handshake completed.")]
    HandshakeRequired,
    #[error("Local peer rejected: {reason}")]
    PeerRejected { reason: String },
    #[error("{0}")]
    Validation(#[from] ValidationError),
    #[error("Controls are read-only: {reason}")]
    ReadOnly { reason: String },
    #[error("Profile is incompatible with the connected hardware.")]
    Incompatible { details: Vec<Incompatibility> },
    #[error("No profile named {name}.")]
    ProfileNotFound { name: String },
    #[error("The name {name} is reserved for the built-in safe profile.")]
    ProfileNameUnavailable { name: String },
    #[error("No supported device is present.")]
    NoDevice,
    #[error("Local operation failed: {detail}")]
    Io { detail: String },
}

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

    #[test]
    fn the_runtime_directory_never_falls_back_to_a_world_writable_place() {
        let home = Path::new("/home/a");
        let resolved = runtime_dir(None, None, Some(home));
        assert!(resolved.starts_with(home), "{resolved:?}");
        assert!(!resolved.starts_with("/tmp"), "{resolved:?}");
        assert!(!resolved.starts_with("/var/tmp"), "{resolved:?}");
    }

    #[test]
    fn the_runtime_directory_prefers_the_override_then_xdg() {
        assert_eq!(
            runtime_dir(
                Some(Path::new("/explicit")),
                Some(Path::new("/run/user/1000")),
                Some(Path::new("/home/a"))
            ),
            Path::new("/explicit")
        );
        assert_eq!(
            runtime_dir(
                None,
                Some(Path::new("/run/user/1000")),
                Some(Path::new("/home/a"))
            ),
            Path::new("/run/user/1000/nzxt-control")
        );
    }

    #[test]
    fn the_socket_sits_inside_the_runtime_directory() {
        let directory = runtime_dir(None, Some(Path::new("/run/user/1000")), None);
        assert_eq!(
            directory.join(SOCKET_FILE_NAME),
            Path::new("/run/user/1000/nzxt-control/nzxt-control.sock")
        );
    }

    #[test]
    fn config_recovery_message_names_the_preserved_file() {
        let state = ConfigState::Recovered {
            detail: "expected schema 1, found 9".into(),
            preserved_path: "/home/a/.config/nzxt-control/config.toml.corrupt".into(),
            recovery_action: "Re-save a profile to replace it.".into(),
        };
        let message = state.recovery_message().unwrap();
        assert!(message.contains(".corrupt"), "{message}");
        assert!(message.contains("Safe defaults are active"), "{message}");
        assert!(ConfigState::Loaded.recovery_message().is_none());
    }
}
