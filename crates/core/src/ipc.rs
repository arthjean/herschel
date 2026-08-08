// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The typed protocol spoken over the daemon's Unix socket.
//!
//! Framing is newline-delimited JSON with a hard length ceiling applied on both
//! ends. Anything that does not parse into [`Request`] is answered with a typed
//! [`IpcError`] and produces zero hardware writes.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::DeviceId;
use crate::capability::{CapabilityId, CapabilityRecord, LcdPanel};
use crate::display::{DisplayError, DisplayPreset};
use crate::lighting::{LightingCommand, LightingError, LightingProgram};
use crate::profile::{Channel, CoolingProgram, Incompatibility, Profile, ValidationError};
use crate::telemetry::{PwmMode, TelemetrySnapshot};

/// Incremented on any breaking change to [`Request`] or [`Response`].
///
/// Version 2 added the lighting command and the per-channel lighting state.
///
/// Version 3 added the panel preset and the state the daemon reports for it.
///
/// Version 4 changed the shape of that preset in both directions: a reading
/// slot gained the color its band fades to, and the wordmark color it no longer
/// draws stopped being written. A preset refuses unknown fields, so a client of
/// this version and a daemon of the last cannot exchange one either way. The
/// bump is what turns that into a refusal at the handshake, naming both
/// versions, instead of a parse failure the first time a frame is applied.
pub const PROTOCOL_VERSION: u32 = 4;

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
    /// The most recent sampling pass.
    Telemetry,
    /// Apply a cooling program directly, without saving it as a profile.
    ///
    /// This is what the Cooling screen's Apply activates. The daemon
    /// revalidates every value before the first write.
    ApplyProgram { program: CoolingProgram },
    /// Apply a lighting program to one channel of the RGB controller.
    ///
    /// The daemon revalidates the program, checks it against the topology the
    /// controller reported, and enforces the command cadence before any byte
    /// is sent.
    ApplyLighting { command: LightingCommand },
    /// Show a preset on the Kraken's panel.
    ///
    /// The preset travels, never the pixels: the daemon renders it with the
    /// same crate the client previews it with, so the two cannot drift, and a
    /// panel keeps updating after the window closes.
    ApplyDisplay { preset: DisplayPreset },
    /// Redacted diagnostics for an issue report.
    Diagnostics,
}

/// A response from the daemon.
///
/// Not `Eq`: a telemetry snapshot carries floating-point readings, and an
/// equality that silently compared them would invite exactly the wrong kind of
/// assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    Telemetry(Box<TelemetrySnapshot>),
    Applied(Box<ApplyOutcome>),
    Lit(LightingOutcome),
    Shown(Box<DisplayOutcome>),
    Diagnostics(crate::diagnostics::DiagnosticsExport),
    Error(IpcError),
}

/// The result of one lighting command.
///
/// The controller acknowledges no state: there is no report that reads a
/// channel's current color back. The daemon is the sole writer, so its record
/// of what it committed is the only evidence of what the channel is showing,
/// which is the same reasoning the write-only curve attributes already follow.
/// `Confirmed` therefore means the controller accepted the report, and nothing
/// stronger is claimed anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightingOutcome {
    pub channel: u8,
    pub program: LightingProgram,
    pub hardware: HardwareState,
    /// Reports actually sent. Zero means the request matched the committed
    /// state and was deduplicated.
    pub writes: u32,
    pub deduplicated: bool,
}

/// The result of one panel command.
///
/// The panel acknowledges no picture: there is no report that reads back what
/// it is showing. The daemon is the sole writer, so its record of the last
/// preset it committed is the only evidence, exactly as for lighting.
/// `Confirmed` means the transfer completed, and nothing stronger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayOutcome {
    pub preset: DisplayPreset,
    pub hardware: HardwareState,
    /// Frames actually sent. Zero means the request matched what the panel is
    /// already showing and was deduplicated.
    pub frames: u32,
    pub deduplicated: bool,
}

/// What the panel is currently showing, as far as the daemon knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayState {
    /// Geometry a frame must match, when the panel answered at all.
    pub panel: Option<LcdPanel>,
    /// The last preset the daemon committed, when it has committed one.
    pub committed: Option<DisplayPreset>,
    /// True while a preset that reads telemetry is being streamed.
    pub streaming: bool,
    /// Frames dropped because a transfer was still in flight when the next
    /// sample arrived. US-018 keeps at most one frame pending, so this counts
    /// what that ceiling discarded.
    pub dropped_frames: u64,
}

/// What one lighting channel is currently showing, as far as the daemon knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelState {
    pub channel: u8,
    /// Accessory names the controller reported, in slot order.
    pub accessories: Vec<String>,
    /// The last program the daemon committed, when it has committed one.
    pub committed: Option<LightingProgram>,
}

/// What actually happened to the hardware when a profile was activated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationOutcome {
    pub name: String,
    pub hardware: HardwareState,
    /// The write the activation performed, when it performed one.
    pub applied: Option<ApplyOutcome>,
}

/// The result of one cooling write, with the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    pub hardware: HardwareState,
    /// Number of kernel attributes actually written.
    ///
    /// Zero means the request matched the confirmed state and was
    /// deduplicated, which is what US-009 requires of a repeated Apply.
    pub writes: u32,
    /// True when nothing was written because the state already matched.
    pub deduplicated: bool,
    /// What the kernel reported after the write, per channel.
    pub readback: Vec<ChannelReadback>,
}

impl ApplyOutcome {
    /// The outcome of a program that touches no hardware.
    pub fn untouched(hardware: HardwareState) -> Self {
        Self {
            hardware,
            writes: 0,
            deduplicated: false,
            readback: Vec::new(),
        }
    }

    pub fn readback_for(&self, channel: Channel) -> Option<&ChannelReadback> {
        self.readback.iter().find(|entry| entry.channel == channel)
    }
}

/// What one channel reported after a write.
///
/// Every field is optional because a readback the kernel does not provide is
/// recorded as missing rather than assumed to match what was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelReadback {
    pub channel: Channel,
    pub mode: Option<PwmMode>,
    pub duty: Option<u8>,
    /// Curve points that read back identical to what was written.
    pub curve_points_confirmed: Option<u16>,
    /// Set when the readback contradicts the value that was written.
    pub mismatch: Option<String>,
}

impl ChannelReadback {
    pub fn new(channel: Channel) -> Self {
        Self {
            channel,
            mode: None,
            duty: None,
            curve_points_confirmed: None,
            mismatch: None,
        }
    }

    pub fn is_confirmed(&self) -> bool {
        self.mismatch.is_none() && (self.mode.is_some() || self.duty.is_some())
    }
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
    /// Lighting channels the controller reported, empty when it reported none.
    pub lighting: Vec<ChannelState>,
    /// The panel's geometry and what it was last told to show.
    pub display: DisplayState,
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
    #[error("{0}")]
    Lighting(#[from] LightingError),
    #[error("{0}")]
    Display(#[from] DisplayError),
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
