// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The typed protocol spoken over the daemon's Unix socket.
//!
//! This file is the vocabulary: what either side may say and what every answer
//! carries. Anything that does not parse into [`Request`] is answered with a
//! typed [`IpcError`] and produces zero hardware writes.
//!
//! Two jobs done underneath it live in their own modules. `frame` is the
//! transport, newline-delimited JSON with a hard length ceiling on both ends,
//! and it knows nothing about what it carries. `paths` is where the socket
//! sits on this machine, which is filesystem layout rather than protocol. Both
//! are re-exported here, so every caller still reaches them through
//! `kori_core::ipc`.

use serde::{Deserialize, Serialize};

mod frame;
mod paths;

pub use frame::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
pub use paths::{
    RUNTIME_DIR_ENV, SOCKET_FILE_NAME, SOCKET_PATH_ENV, runtime_dir, runtime_dir_from_env,
    socket_path_from_env,
};

use crate::DeviceId;
use crate::capability::{CapabilityId, CapabilityRecord, LcdPanel};
use crate::display::{DisplayError, DisplayPreset};
use crate::lighting::{LightingCommand, LightingError, LightingProgram};
use crate::profile::{Channel, CoolingProgram, Incompatibility, Profile, ValidationError};
use crate::telemetry::{PwmMode, TelemetrySnapshot};

/// Incremented on any breaking change to [`Request`] or [`Response`].
///
/// Negotiated by [`Request::Hello`] before anything else is sent, so a mismatch
/// is a refusal naming both versions rather than a frame one side misreads.
/// What each version changed, and which quiet disagreement the bump exists to
/// prevent, is recorded in `docs/schema-history.md`.
pub const PROTOCOL_VERSION: u32 = 8;

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

impl Response {
    /// The name this variant travels under, for an error that has to say what
    /// arrived instead of what was expected.
    ///
    /// The strings are the wire tags, so a mismatch reported to an operator
    /// names something they can find in a frame rather than a label invented
    /// for the message.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Status(_) => "status",
            Self::Capabilities(_) => "capabilities",
            Self::Profiles { .. } => "profiles",
            Self::Activated(_) => "activated",
            Self::Saved { .. } => "saved",
            Self::Deleted { .. } => "deleted",
            Self::Telemetry(_) => "telemetry",
            Self::Applied(_) => "applied",
            Self::Lit(_) => "lit",
            Self::Shown(_) => "shown",
            Self::Diagnostics(_) => "diagnostics",
            Self::Error(_) => "error",
        }
    }
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
    /// True when the picture was already on the glass and no frame went out.
    ///
    /// This is about the picture alone. A command can be deduplicated here and
    /// still have changed the panel, because the brightness is not a picture:
    /// see `brightness_sent`.
    pub deduplicated: bool,
    /// True when the display-control report went out for this command.
    ///
    /// The brightness is a panel setting rather than a pixel, so it survives
    /// the frame comparison. Without this field a preset whose only edit was
    /// the brightness would be reported as "nothing was sent" by a client that
    /// had just changed what the operator is looking at.
    pub brightness_sent: bool,
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
    /// Why the stream stopped, once a transfer failed.
    ///
    /// [`DisplayState::streaming`] cannot say this on its own: it is equally
    /// false for a preset that reads no telemetry and for a panel that was
    /// never written. The daemon stops a faulted stream until an explicit
    /// recoverable state arrives, and a screen that cannot see the fault cannot
    /// offer one.
    pub faulted: Option<String>,
    /// Frames dropped because a transfer was still in flight when the next
    /// sample arrived. The streamer keeps at most one frame pending, so this
    /// counts what that ceiling discarded.
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
    /// deduplicated, which is what a repeated Apply must do.
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
    /// The thermal program the daemon last committed, when it committed one.
    ///
    /// A duty can be read back from the driver, but a curve cannot: the device
    /// publishes no attribute that returns the shape it was given. The daemon's
    /// own record is therefore the only place a curve exists, and a client that
    /// could not see it would open on a plot the machine is not running and
    /// make the operator draw theirs again.
    pub cooling: Option<CoolingProgram>,
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

#[cfg(test)]
mod tests {
    use super::*;
    /// something the client did not ask for, so it has to be the tag the frame
    /// actually carries. The two are separate hand-written mirrors of the same
    /// variant list, and only this makes them agree.
    #[test]
    fn every_response_kind_is_the_tag_it_travels_under() {
        let status = DaemonStatus {
            daemon_version: "0.1.0".into(),
            protocol_version: PROTOCOL_VERSION,
            access: AccessMode::ReadWrite,
            devices: Vec::new(),
            active_profile: "Onboard safe".into(),
            config: ConfigState::Defaults,
            cooling: None,
            lighting: Vec::new(),
            display: DisplayState {
                panel: None,
                committed: None,
                streaming: false,
                faulted: None,
                dropped_frames: 0,
            },
            socket_path: "/run/user/1000/kori/kori.sock".into(),
        };
        let record = CapabilityRecord {
            schema_version: crate::capability::CAPABILITY_SCHEMA_VERSION,
            context: crate::capability::ProbeContext {
                kernel_release: crate::capability::Evidenced::unknown("not read", "test"),
                probed_at_unix_ms: 0,
            },
            devices: Vec::new(),
            rejected: Vec::new(),
        };
        let preset = DisplayPreset::default_infographic();

        let responses = vec![
            Response::Hello {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "0.1.0".into(),
            },
            Response::Status(Box::new(status)),
            Response::Capabilities(Box::new(record)),
            Response::Profiles {
                active: "Onboard safe".into(),
                profiles: Vec::new(),
            },
            Response::Activated(ActivationOutcome {
                name: "Onboard safe".into(),
                hardware: HardwareState::Onboard,
                applied: None,
            }),
            Response::Saved {
                name: "Silent".into(),
            },
            Response::Deleted {
                name: "Silent".into(),
                activated_instead: None,
            },
            Response::Telemetry(Box::new(TelemetrySnapshot::unavailable(
                0,
                crate::telemetry::Unavailable::absent("no hwmon"),
            ))),
            Response::Applied(Box::new(ApplyOutcome::untouched(HardwareState::Onboard))),
            Response::Lit(LightingOutcome {
                channel: 1,
                program: LightingProgram::Off,
                hardware: HardwareState::Confirmed,
                writes: 1,
                deduplicated: false,
            }),
            Response::Shown(Box::new(DisplayOutcome {
                preset,
                hardware: HardwareState::Confirmed,
                frames: 1,
                deduplicated: false,
                brightness_sent: true,
            })),
            Response::Diagnostics(
                crate::diagnostics::DiagnosticsLog::default().export(0, "0.1.0", None),
            ),
            Response::Error(IpcError::NoDevice),
        ];

        let mut seen: Vec<&'static str> = Vec::with_capacity(responses.len());
        for response in &responses {
            let encoded = serde_json::to_value(response).unwrap();
            assert_eq!(
                encoded.get("response").and_then(serde_json::Value::as_str),
                Some(response.kind()),
                "{} does not travel under its own kind",
                response.kind()
            );
            assert!(
                !seen.contains(&response.kind()),
                "{} is claimed by two variants",
                response.kind()
            );
            seen.push(response.kind());
        }
    }

    #[test]
    fn config_recovery_message_names_the_preserved_file() {
        let state = ConfigState::Recovered {
            detail: "expected schema 1, found 9".into(),
            preserved_path: "/home/a/.config/kori/config.toml.corrupt".into(),
            recovery_action: "Re-save a profile to replace it.".into(),
        };
        let message = state.recovery_message().unwrap();
        assert!(message.contains(".corrupt"), "{message}");
        assert!(message.contains("Safe defaults are active"), "{message}");
        assert!(ConfigState::Loaded.recovery_message().is_none());
    }
}
