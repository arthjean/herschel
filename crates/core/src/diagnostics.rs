// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded, redacted diagnostics.
//!
//! The log is an in-memory ring buffer: it never grows without bound and never
//! reaches disk on its own. An export carries only what this module builds, so
//! it cannot pick up credentials, environment variables, unrelated files or
//! network data by accident.

use std::collections::VecDeque;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::DeviceId;
use crate::capability::{CapabilityId, CapabilityRecord};
use crate::ipc::{HardwareState, IpcError};
use crate::profile::Channel;
use crate::telemetry::Collector;

/// Bumped whenever [`DiagnosticsExport`] changes shape.
pub const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;

/// Events retained in memory. Older events are dropped first.
pub const DEFAULT_CAPACITY: usize = 512;

/// Text substituted for anything the redactor recognizes as sensitive.
pub const REDACTION_PLACEHOLDER: &str = "[redacted]";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// A state transition or typed failure worth keeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    DaemonStarted {
        version: String,
        socket_path: String,
    },
    DaemonStopping {
        reason: String,
    },
    DeviceDiscovered {
        device: DeviceId,
        sysfs_path: String,
    },
    DeviceRejected {
        device: DeviceId,
        reason: String,
    },
    CapabilityResolved {
        device: DeviceId,
        capability: CapabilityId,
        state: String,
    },
    OwnershipAcquired {
        device: DeviceId,
        lock_path: String,
    },
    OwnershipConflict {
        device: Option<DeviceId>,
        resource: String,
        detail: String,
    },
    AccessModeChanged {
        read_only: bool,
        reason: String,
    },
    ClientAccepted {
        uid: u32,
        pid: i32,
    },
    ClientRejected {
        reason: String,
    },
    ClientDisconnected {
        detail: String,
    },
    RequestRejected {
        error: IpcError,
    },
    ProfileActivated {
        name: String,
        hardware: HardwareState,
    },
    ProfileSaved {
        name: String,
    },
    ProfileDeleted {
        name: String,
    },
    ConfigLoaded {
        path: String,
    },
    ConfigRecovered {
        detail: String,
        preserved_path: String,
    },
    /// A cooling program reached the hardware, or was refused before it did.
    ProgramApplied {
        hardware: HardwareState,
        /// Kernel attributes actually written. Zero means deduplicated.
        writes: u32,
    },
    /// A lighting program reached the controller, or was refused before it did.
    ///
    /// The program is recorded as its summary, never as packet bytes: a
    /// diagnostics archive is something an operator attaches to an issue, and
    /// a wire dump would put the reverse-engineered protocol in it.
    LightingApplied {
        channel: u8,
        program: String,
        hardware: HardwareState,
        /// Reports actually sent. Zero means deduplicated or refused.
        writes: u32,
    },
    /// A frame was rendered for the panel, or refused before it was.
    DisplayApplied {
        /// Preset mode, as its stable key rather than its label.
        mode: String,
        hardware: HardwareState,
        /// Frames actually sent. Zero means deduplicated or refused.
        frames: u32,
    },
    /// A committed curve is not what the device turned out to be running.
    ///
    /// Curve points are write-only on this driver, so a curve write is accepted
    /// with nothing to compare it against. The duty the device reports at the
    /// measured liquid temperature is the one thing that can contradict it
    /// afterwards, and this is that contradiction.
    CurveDiverged {
        channel: Channel,
        /// Liquid temperature in millidegrees Celsius, as `temp1_input` reports
        /// it. An integer because this event is compared and hashed, and
        /// because it is the unit the attribute already carries.
        liquid_temperature_mc: i32,
        /// Duty the committed curve commands at that temperature.
        expected: u8,
        /// Duty the device reports it is running.
        reported: u8,
    },
    /// A committed command reached the hardware but could not be written down.
    ///
    /// The device took it, so nothing on screen is wrong. What is lost is the
    /// next start: the daemon will replay the state it last managed to record
    /// rather than this one, and this line is the only place that says so.
    SessionNotRecorded {
        detail: String,
    },
    /// A telemetry collector panicked or stopped answering.
    CollectorFailed {
        collector: Collector,
        detail: String,
    },
    CollectorRecovered {
        collector: Collector,
    },
}

impl EventKind {
    pub fn severity(&self) -> Severity {
        match self {
            Self::DeviceRejected { .. }
            | Self::OwnershipConflict { .. }
            | Self::ClientRejected { .. }
            | Self::RequestRejected { .. }
            | Self::CollectorFailed { .. }
            | Self::ConfigRecovered { .. }
            // The hardware is running something the next start will not put
            // back, which the operator only learns from here.
            | Self::SessionNotRecorded { .. }
            // The device is not running the program this process believes it
            // committed. That is a control failure, not a notice.
            | Self::CurveDiverged { .. } => Severity::Warning,
            Self::ProgramApplied { hardware, .. }
            | Self::LightingApplied { hardware, .. }
            | Self::DisplayApplied { hardware, .. } => match hardware {
                HardwareState::Uncertain { .. } => Severity::Error,
                HardwareState::NotApplied { .. } => Severity::Warning,
                HardwareState::Confirmed | HardwareState::Onboard => Severity::Info,
            },
            Self::AccessModeChanged { read_only, .. } => {
                if *read_only {
                    Severity::Warning
                } else {
                    Severity::Info
                }
            }
            // Listed one by one rather than defaulted. A wildcard here would
            // hand every event added later the mildest severity there is,
            // which is how a new failure arrives in the log as a notice.
            Self::DaemonStarted { .. }
            | Self::DaemonStopping { .. }
            | Self::DeviceDiscovered { .. }
            | Self::CapabilityResolved { .. }
            | Self::OwnershipAcquired { .. }
            | Self::ClientAccepted { .. }
            | Self::ClientDisconnected { .. }
            | Self::ProfileActivated { .. }
            | Self::ProfileSaved { .. }
            | Self::ProfileDeleted { .. }
            | Self::ConfigLoaded { .. }
            | Self::CollectorRecovered { .. } => Severity::Info,
        }
    }
}

/// One timestamped entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticEvent {
    pub at_unix_ms: u64,
    pub severity: Severity,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Replaces known secrets wherever they appear in free text.
///
/// USB serial numbers are the only secrets this product handles, and they are
/// redacted by default rather than on request.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a value that must never appear in an export.
    ///
    /// Very short values are ignored: redacting them would corrupt unrelated
    /// text without protecting anything.
    pub fn add_secret(&mut self, secret: impl Into<String>) {
        let secret = secret.into();
        if secret.len() >= 4 && !self.secrets.contains(&secret) {
            self.secrets.push(secret);
        }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut output = text.to_string();
        for secret in &self.secrets {
            if output.contains(secret.as_str()) {
                output = output.replace(secret.as_str(), REDACTION_PLACEHOLDER);
            }
        }
        output
    }

    /// A copy of `value` with every registered secret replaced.
    ///
    /// `None` when the value could not be taken apart and put back together.
    /// A caller must treat that as a refusal rather than fall back to the
    /// original: handing back the unswept value would export exactly the text
    /// this type exists to remove, and that is the one direction a redactor
    /// must never fail in.
    fn redacted<T: Serialize + DeserializeOwned>(&self, value: &T) -> Option<T> {
        let mut json = serde_json::to_value(value).ok()?;
        self.redact_json(&mut json);
        serde_json::from_value(json).ok()
    }

    fn redact_json(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                let redacted = self.redact(text);
                if &redacted != text {
                    *text = redacted;
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.redact_json(item);
                }
            }
            serde_json::Value::Object(map) => {
                for (_, item) in map.iter_mut() {
                    self.redact_json(item);
                }
            }
            _ => {}
        }
    }
}

/// Bounded in-memory event log.
#[derive(Debug, Clone)]
pub struct DiagnosticsLog {
    events: VecDeque<DiagnosticEvent>,
    capacity: usize,
    redactor: Redactor,
}

impl Default for DiagnosticsLog {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl DiagnosticsLog {
    pub fn with_capacity(capacity: usize) -> Self {
        // One bound, decided once. The allocation is only a hint and is held
        // down to the default so a large capacity does not reserve for events
        // that may never arrive.
        let capacity = capacity.max(1);
        Self {
            events: VecDeque::with_capacity(capacity.min(DEFAULT_CAPACITY)),
            capacity,
            redactor: Redactor::new(),
        }
    }

    /// Register a secret to strip from every future export.
    pub fn add_secret(&mut self, secret: impl Into<String>) {
        self.redactor.add_secret(secret);
    }

    pub fn record(&mut self, at_unix_ms: u64, kind: EventKind) {
        let event = DiagnosticEvent {
            at_unix_ms,
            severity: kind.severity(),
            kind,
        };
        while self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn events(&self) -> impl Iterator<Item = &DiagnosticEvent> {
        self.events.iter()
    }

    /// Build an export with every secret removed from all of it.
    ///
    /// Two removals run, and they are not the same removal. `redact_serials`
    /// blanks the field that holds a serial whether or not anything registered
    /// it as a secret. The sweep then replaces registered secrets wherever they
    /// ended up, which is what reaches a serial that landed in a device path,
    /// an evidence source or a free-text detail. Running only the first would
    /// miss those; running only the second would keep a serial nobody had
    /// registered.
    pub fn export(
        &self,
        generated_at_unix_ms: u64,
        daemon_version: impl Into<String>,
        capabilities: Option<CapabilityRecord>,
    ) -> DiagnosticsExport {
        let capabilities = capabilities.map(|mut record| {
            record.redact_serials();
            record
        });

        let export = DiagnosticsExport {
            schema_version: DIAGNOSTICS_SCHEMA_VERSION,
            generated_at_unix_ms,
            daemon_version: daemon_version.into(),
            capabilities,
            events: self.events.iter().cloned().collect(),
        };

        // One sweep over the whole document. Sweeping the events alone, as this
        // did, left the capability record to a single blanked field and shipped
        // anything that had reached the rest of it.
        let swept = self.redactor.redacted(&export);
        match swept {
            Some(swept) => swept,
            // Nothing that could not be swept leaves. An export with no events
            // is a poor bug report and says so by being empty; an export
            // carrying a serial is a disclosure, and only one of the two can be
            // taken back.
            None => DiagnosticsExport {
                capabilities: None,
                events: Vec::new(),
                ..export
            },
        }
    }
}

/// The complete payload of an export action.
///
/// Every field is built from values this crate owns. Nothing walks the home
/// directory, reads the environment or touches the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsExport {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    pub daemon_version: String,
    pub capabilities: Option<CapabilityRecord>,
    pub events: Vec<DiagnosticEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KRAKEN_BASE;
    use crate::capability::{
        CAPABILITY_SCHEMA_VERSION, CapabilityRecord, DeviceRecord, Evidenced, ProbeContext,
        SupportState, UsbIdentity,
    };

    /// Synthetic: a serial identifies one physical unit, so a real one has no
    /// place in a test that exists to prove serials never escape.
    const SERIAL: &str = "F1XTURE0000000000000000000KRAKEN";

    fn record_with_serial() -> CapabilityRecord {
        CapabilityRecord {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            context: ProbeContext {
                kernel_release: Evidenced::known("7.1.6".into(), "uname"),
                probed_at_unix_ms: 10,
            },
            devices: vec![DeviceRecord {
                support: SupportState::Supported,
                usb: UsbIdentity {
                    id: KRAKEN_BASE,
                    manufacturer: Evidenced::known("NZXT Inc.".into(), "sysfs"),
                    product: Evidenced::known("NZXT Kraken Base".into(), "sysfs"),
                    serial: Evidenced::known(SERIAL.into(), "sysfs"),
                    firmware: Evidenced::known("0200".into(), "sysfs"),
                    sysfs_path: "/sys/bus/usb/devices/1-9".into(),
                },
                interfaces: vec![],
                hwmon: None,
                rgb: None,
                lcd: None,
                capabilities: vec![],
            }],
            rejected: vec![],
        }
    }

    #[test]
    fn log_is_bounded_and_drops_oldest_first() {
        let mut log = DiagnosticsLog::with_capacity(3);
        for i in 0..10 {
            log.record(
                i,
                EventKind::ProfileSaved {
                    name: format!("p{i}"),
                },
            );
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.events().next().unwrap().at_unix_ms, 7);
    }

    #[test]
    fn export_redacts_serials_from_the_capability_record() {
        let log = DiagnosticsLog::default();
        let export = log.export(1, "0.1.0", Some(record_with_serial()));
        let json = serde_json::to_string(&export).unwrap();
        assert!(!json.contains(SERIAL), "{json}");
    }

    #[test]
    fn export_redacts_secrets_from_free_text_events() {
        let mut log = DiagnosticsLog::default();
        log.add_secret(SERIAL);
        log.record(
            1,
            EventKind::OwnershipConflict {
                device: Some(KRAKEN_BASE),
                resource: format!("/dev/serial/by-id/usb-NZXT_{SERIAL}"),
                detail: format!("held by another process for {SERIAL}"),
            },
        );

        let export = log.export(2, "0.1.0", None);
        let json = serde_json::to_string(&export).unwrap();
        assert!(!json.contains(SERIAL), "{json}");
        assert!(json.contains(REDACTION_PLACEHOLDER), "{json}");
    }

    #[test]
    fn export_contains_only_declared_fields() {
        let mut log = DiagnosticsLog::default();
        log.record(
            1,
            EventKind::DaemonStarted {
                version: "0.1.0".into(),
                socket_path: "/run/user/1000/kori.sock".into(),
            },
        );
        let export = log.export(2, "0.1.0", None);
        let value = serde_json::to_value(&export).unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "capabilities",
                "daemon_version",
                "events",
                "generated_at_unix_ms",
                "schema_version"
            ]
        );
    }

    /// The export used to run two different removals over two disjoint halves
    /// of itself: the registered-secret sweep reached the events, and the
    /// capability record got only its `serial` field blanked. A serial that had
    /// reached any other string of the record therefore shipped.
    #[test]
    fn the_sweep_reaches_the_capability_record_and_not_only_the_events() {
        let mut log = DiagnosticsLog::default();
        log.add_secret(SERIAL);

        let mut record = record_with_serial();
        let device = &mut record.devices[0];
        device.usb.sysfs_path = format!("/sys/bus/usb/devices/usb-NZXT_{SERIAL}");
        device.usb.product = Evidenced::known(
            format!("NZXT Kraken Base {SERIAL}"),
            format!("/dev/serial/by-id/{SERIAL}"),
        );

        let export = log.export(1, "0.1.0", Some(record));
        let json = serde_json::to_string(&export).unwrap();
        assert!(!json.contains(SERIAL), "{json}");
        assert!(json.contains(REDACTION_PLACEHOLDER), "{json}");
    }

    /// A value that refuses to be rebuilt from a redacted string is the only
    /// way the sweep can fail. Nothing in a `DiagnosticsExport` behaves this
    /// way today; the point is that the mechanism fails closed before something
    /// one day does, because the export used to hand back the original.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(try_from = "String", into = "String")]
    struct Digits(String);

    impl TryFrom<String> for Digits {
        type Error = String;

        fn try_from(value: String) -> Result<Self, Self::Error> {
            if value.chars().all(|character| character.is_ascii_digit()) {
                Ok(Self(value))
            } else {
                Err(format!("{value} is not made of digits"))
            }
        }
    }

    impl From<Digits> for String {
        fn from(value: Digits) -> Self {
            value.0
        }
    }

    #[test]
    fn a_value_that_cannot_be_swept_is_refused_rather_than_returned_intact() {
        let mut redactor = Redactor::new();
        redactor.add_secret("12345678");

        assert_eq!(
            redactor.redacted(&Digits("12345678".into())),
            None,
            "a value that cannot be put back together must not come back \
             carrying the secret"
        );

        // A value with nothing to remove still comes back unchanged.
        let clean = Digits("0000".into());
        assert_eq!(redactor.redacted(&clean), Some(clean));
    }

    /// Severity decides what an operator is shown first, so the events that
    /// report a control failure must not read as notices.
    #[test]
    fn a_control_failure_is_never_recorded_as_a_notice() {
        for kind in [
            EventKind::CurveDiverged {
                channel: Channel::Pump,
                liquid_temperature_mc: 31_200,
                expected: 180,
                reported: 90,
            },
            EventKind::SessionNotRecorded {
                detail: "the configuration directory is read-only".into(),
            },
            EventKind::CollectorFailed {
                collector: Collector::Gpu,
                detail: "the collector panicked".into(),
            },
        ] {
            assert_eq!(kind.severity(), Severity::Warning, "{kind:?}");
        }

        assert_eq!(
            EventKind::ProgramApplied {
                hardware: HardwareState::Uncertain {
                    reason: "readback failed".into()
                },
                writes: 1,
            }
            .severity(),
            Severity::Error
        );
        assert_eq!(
            EventKind::ProfileSaved {
                name: "Silent".into()
            }
            .severity(),
            Severity::Info
        );
    }

    #[test]
    fn short_values_are_not_registered_as_secrets() {
        let mut redactor = Redactor::new();
        redactor.add_secret("ab");
        assert_eq!(redactor.redact("a table"), "a table");
    }

    #[test]
    fn rejected_requests_are_recorded_as_warnings() {
        let mut log = DiagnosticsLog::default();
        log.record(
            1,
            EventKind::RequestRejected {
                error: IpcError::NoDevice,
            },
        );
        assert_eq!(log.events().next().unwrap().severity, Severity::Warning);
    }
}
