// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The client's view of the daemon.
//!
//! The window never touches hardware. It asks the daemon what exists, what is
//! writable and what the sensors currently read, and derives every control
//! state from that answer. When the daemon is unreachable the shell stays
//! usable and read-only rather than showing values it cannot vouch for.

use std::sync::Arc;

pub mod gate;

use kori_core::DeviceId;
use kori_core::capability::{CapabilityRecord, DeviceRecord};
use kori_core::ipc::{AccessMode, ChannelState, DaemonStatus};
use kori_core::profile::{Channel, Profile};
use kori_core::telemetry::{Collector, CollectorFailure, SafetyAlert, TelemetrySnapshot};

use crate::components::DeviceHealth;
use crate::theme::META_SEPARATOR;

/// What the client knows about the daemon right now.
///
/// Cheap to clone: the worker publishes one of these per cycle and the view
/// takes a copy, so nothing is read out from under a repaint.
#[derive(Debug, Clone)]
pub enum LinkState {
    /// Connected, with the state the daemon reported.
    Connected {
        status: Arc<DaemonStatus>,
        capabilities: Arc<CapabilityRecord>,
        profiles: Arc<[Profile]>,
        telemetry: Option<Arc<TelemetrySnapshot>>,
    },
    /// Not connected. `message` is shown verbatim to the operator.
    Unavailable { message: String },
}

impl LinkState {
    /// The state shown before the worker's first cycle completes.
    pub fn connecting() -> Self {
        Self::Unavailable {
            message: "Connecting to the background service...".to_string(),
        }
    }

    pub fn status(&self) -> Option<&DaemonStatus> {
        match self {
            Self::Connected { status, .. } => Some(status),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn capabilities(&self) -> Option<&CapabilityRecord> {
        match self {
            Self::Connected { capabilities, .. } => Some(capabilities),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn profiles(&self) -> &[Profile] {
        match self {
            Self::Connected { profiles, .. } => profiles,
            Self::Unavailable { .. } => &[],
        }
    }

    pub fn telemetry(&self) -> Option<&TelemetrySnapshot> {
        match self {
            Self::Connected { telemetry, .. } => telemetry.as_deref(),
            Self::Unavailable { .. } => None,
        }
    }

    /// Conditions the Cooling screen must surface immediately.
    pub fn alerts(&self) -> &[SafetyAlert] {
        match self.telemetry() {
            Some(snapshot) => &snapshot.alerts,
            None => &[],
        }
    }

    pub fn failed_collectors(&self) -> &[CollectorFailure] {
        match self.telemetry() {
            Some(snapshot) => &snapshot.failed,
            None => &[],
        }
    }

    /// The reported failure of one collector, when it has one.
    pub fn collector_failure(&self, collector: Collector) -> Option<&CollectorFailure> {
        self.failed_collectors()
            .iter()
            .find(|failure| failure.collector == collector)
    }

    /// Lighting channels the controller reported, empty when it reported none.
    ///
    /// Empty is the normal state until the controller answers: the screen shows
    /// the reason the capability record carries rather than an invented channel.
    pub fn lighting_channels(&self) -> &[ChannelState] {
        match self.status() {
            Some(status) => &status.lighting,
            None => &[],
        }
    }

    pub fn active_profile(&self) -> Option<&str> {
        self.status().map(|status| status.active_profile.as_str())
    }

    pub fn device(&self, id: DeviceId) -> Option<&DeviceRecord> {
        self.capabilities()?.device(id)
    }

    /// Age of the Kraken readings, in milliseconds.
    pub fn kraken_age_ms(&self, now_unix_ms: u64) -> Option<u64> {
        let snapshot = self.telemetry()?;
        Some(now_unix_ms.saturating_sub(snapshot.kraken.at_unix_ms))
    }

    /// The one sentence a destination shows when something needs attention.
    ///
    /// Ordered by urgency: no daemon, then recovered configuration, then
    /// read-only ownership, then nothing.
    pub fn banner(&self) -> Option<String> {
        match self {
            Self::Unavailable { message } => Some(message.clone()),
            Self::Connected { status, .. } => {
                if let Some(message) = status.config.recovery_message() {
                    return Some(message);
                }
                match &status.access {
                    AccessMode::ReadWrite => None,
                    AccessMode::ReadOnly { conflicts } => Some(format!(
                        "Controls are read-only. {}",
                        conflict_detail(conflicts)
                    )),
                }
            }
        }
    }

    /// Alerts affecting one channel, plus every alert that names no channel.
    pub fn channel_alerts(&self, channel: Channel) -> Vec<&SafetyAlert> {
        self.alerts()
            .iter()
            .filter(|alert| alert.channel().is_none_or(|affected| affected == channel))
            .collect()
    }

    /// Rows for the device list, one per allowlisted device.
    pub fn device_rows(&self) -> Vec<DeviceSummary> {
        let Self::Connected {
            status,
            capabilities,
            ..
        } = self
        else {
            return Vec::new();
        };

        capabilities
            .supported()
            .map(|record| {
                let reported = status.devices.iter().find(|d| d.id == record.id());
                let owned = reported.is_some_and(|device| device.owned);
                let writable = reported.is_some_and(|device| !device.writable.is_empty());
                let health = match (owned, writable) {
                    (false, _) => DeviceHealth::Unavailable,
                    (true, false) => DeviceHealth::ReadOnly,
                    (true, true) => DeviceHealth::Ready,
                };

                DeviceSummary {
                    id: record.id(),
                    name: record
                        .usb
                        .product
                        .value()
                        .cloned()
                        .unwrap_or_else(|| "Unnamed device".to_string()),
                    firmware: record.usb.firmware.value().cloned(),
                    driver: record
                        .hwmon
                        .as_ref()
                        .map(|hwmon| hwmon.driver.clone())
                        .unwrap_or_else(|| "no kernel driver".to_string()),
                    health,
                }
            })
            .collect()
    }
}

/// Every ownership conflict as one sentence, in the order the daemon reported.
///
/// One reading of the list rather than one per caller: the banner, the
/// capability gate and the profile gate all quote the same conflicts, and three
/// copies of the join is three places for one of them to start dropping a
/// conflict the operator needs.
fn conflict_detail(conflicts: &[kori_core::ipc::OwnershipConflict]) -> String {
    conflicts
        .iter()
        .map(|conflict| conflict.detail.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One device as the shell renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSummary {
    pub id: DeviceId,
    pub name: String,
    pub firmware: Option<String>,
    pub driver: String,
    pub health: DeviceHealth,
}

impl DeviceSummary {
    /// The secondary line: firmware and kernel binding, or their absence.
    ///
    /// Two fragments joined by the metadata separator rather than one sentence.
    /// The sentence form read as "bound to no kernel driver" whenever nothing
    /// was bound, which states the opposite of what it means.
    pub fn detail(&self) -> String {
        let firmware = match &self.firmware {
            Some(firmware) => format!("Firmware {firmware}"),
            None => "Firmware unknown".to_string(),
        };
        format!("{firmware} {META_SEPARATOR} {}", self.driver)
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    //! Builders both this module's tests and the gate's are written
    //! against. One machine described once: a Kraken that answered, the
    //! capabilities it reported, and telemetry of a chosen age.

    use super::*;
    use kori_core::KRAKEN_BASE;
    use kori_core::capability::{
        CAPABILITY_SCHEMA_VERSION, Capability, CapabilityId, CapabilityState, Evidenced,
        ProbeContext, SupportState, UsbIdentity,
    };
    use kori_core::ipc::{BlockedCapability, ConfigState, DeviceStatus, PROTOCOL_VERSION};
    use kori_core::telemetry::{
        ChannelTelemetry, GpuTelemetry, KrakenTelemetry, PwmMode, Reading, SystemTelemetry,
        Unavailable,
    };

    pub(crate) fn device_record(id: DeviceId, capabilities: Vec<Capability>) -> DeviceRecord {
        DeviceRecord {
            support: SupportState::Supported,
            usb: UsbIdentity {
                id,
                manufacturer: Evidenced::known("NZXT Inc.".into(), "sysfs"),
                product: Evidenced::known("NZXT Kraken Base".into(), "sysfs"),
                serial: Evidenced::unknown("redacted", "policy"),
                firmware: Evidenced::known("0200".into(), "sysfs"),
                sysfs_path: "/sys/bus/usb/devices/1-9".into(),
            },
            interfaces: vec![],
            hwmon: None,
            rgb: None,
            lcd: None,
            capabilities,
        }
    }

    pub(crate) fn telemetry(at_unix_ms: u64, present: bool) -> TelemetrySnapshot {
        let channel = |channel| ChannelTelemetry {
            channel,
            rpm: Reading::valid(2_970),
            duty: Reading::valid(255),
            mode: Reading::valid(PwmMode::FullSpeed),
        };
        TelemetrySnapshot {
            sequence: 1,
            at_unix_ms,
            interval_ms: 1_000,
            kraken: KrakenTelemetry {
                at_unix_ms,
                present,
                liquid_temperature_c: Reading::valid(29.8),
                pump: channel(Channel::Pump),
                fan: channel(Channel::Fan),
            },
            system: SystemTelemetry::unavailable(at_unix_ms, Unavailable::absent("not sampled")),
            gpu: GpuTelemetry::unavailable(at_unix_ms, Unavailable::absent("no NVML")),
            alerts: Vec::new(),
            failed: Vec::new(),
        }
    }

    pub(crate) fn connected(access: AccessMode, capabilities: Vec<Capability>) -> LinkState {
        connected_at(access, capabilities, Some(telemetry(1_000, true)))
    }

    pub(crate) fn connected_at(
        access: AccessMode,
        capabilities: Vec<Capability>,
        snapshot: Option<TelemetrySnapshot>,
    ) -> LinkState {
        let record = device_record(KRAKEN_BASE, capabilities.clone());
        LinkState::Connected {
            status: Arc::new(DaemonStatus {
                daemon_version: "0.1.0".into(),
                protocol_version: PROTOCOL_VERSION,
                access,
                devices: vec![DeviceStatus {
                    id: KRAKEN_BASE,
                    present: true,
                    owned: true,
                    writable: capabilities
                        .iter()
                        .filter(|c| c.state.is_writable())
                        .map(|c| c.id)
                        .collect(),
                    blocked: capabilities
                        .iter()
                        .filter_map(|c| {
                            c.state.blocked_reason().map(|reason| BlockedCapability {
                                capability: c.id,
                                reason,
                            })
                        })
                        .collect(),
                }],
                active_profile: "Onboard safe".into(),
                config: ConfigState::Loaded,
                cooling: None,
                lighting: Vec::new(),
                display: kori_core::ipc::DisplayState {
                    panel: None,
                    committed: None,
                    streaming: false,
                    faulted: None,
                    dropped_frames: 0,
                },
                socket_path: "/run/user/1000/kori/kori.sock".into(),
            }),
            capabilities: Arc::new(CapabilityRecord {
                schema_version: CAPABILITY_SCHEMA_VERSION,
                context: ProbeContext {
                    kernel_release: Evidenced::known("7.1.6".into(), "uname(2)"),
                    probed_at_unix_ms: 1,
                },
                devices: vec![record],
                rejected: vec![],
            }),
            profiles: vec![Profile::safe()].into(),
            telemetry: snapshot.map(Arc::new),
        }
    }

    pub(crate) fn writable(id: CapabilityId) -> Capability {
        Capability {
            id,
            state: CapabilityState::Available {
                writable: true,
                source: "/sys/class/hwmon/hwmon4/pwm1".into(),
            },
        }
    }

    /// A capability the device exposes but this user cannot write, which is
    /// what a udev rule covering only one channel produces.
    pub(crate) fn read_only(id: CapabilityId) -> Capability {
        Capability {
            id,
            state: CapabilityState::Available {
                writable: false,
                source: "/sys/class/hwmon/hwmon4/pwm2".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;
    use kori_core::KRAKEN_BASE;
    use kori_core::capability::{Capability, CapabilityId, CapabilityState};
    use kori_core::ipc::{ConfigState, DaemonStatus};
    use kori_core::profile::Channel;
    use kori_core::telemetry::{CollectorFailure, SafetyAlert};
    #[test]
    fn alerts_are_routed_to_the_channel_they_name() {
        let mut link = connected(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
        );
        if let LinkState::Connected { telemetry, .. } = &mut link {
            let mut snapshot = (**telemetry.as_ref().unwrap()).clone();
            snapshot.alerts = vec![
                SafetyAlert::ChannelStalled {
                    channel: Channel::Pump,
                    commanded_duty: 180,
                    samples: 3,
                    rpm: 0,
                },
                SafetyAlert::LiquidCritical {
                    temperature_c: 61.0,
                    threshold_c: 60.0,
                },
            ];
            *telemetry = Some(Arc::new(snapshot));
        }

        assert_eq!(link.channel_alerts(Channel::Pump).len(), 2);
        assert_eq!(
            link.channel_alerts(Channel::Fan).len(),
            1,
            "an alert naming no channel affects both"
        );
    }

    #[test]
    fn a_recovered_configuration_takes_priority_in_the_banner() {
        let mut link = connected(AccessMode::ReadWrite, vec![]);
        if let LinkState::Connected { status, .. } = &mut link {
            *status = Arc::new(DaemonStatus {
                config: ConfigState::Recovered {
                    detail: "expected schema 1, found 9".into(),
                    preserved_path: "/home/a/.config/kori/config.toml.corrupt.1".into(),
                    recovery_action: "Save a profile to write a fresh configuration.".into(),
                },
                ..(**status).clone()
            });
        }
        assert!(link.banner().unwrap().contains("Safe defaults are active"));
    }

    #[test]
    fn device_rows_report_health_from_ownership_and_writability() {
        let ready = connected(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
        );
        assert_eq!(ready.device_rows()[0].health, DeviceHealth::Ready);

        let read_only = connected(
            AccessMode::ReadWrite,
            vec![Capability {
                id: CapabilityId::PumpDuty,
                state: CapabilityState::Available {
                    writable: false,
                    source: "/sys/class/hwmon/hwmon4/pwm1".into(),
                },
            }],
        );
        assert_eq!(read_only.device_rows()[0].health, DeviceHealth::ReadOnly);
    }

    #[test]
    fn a_failed_collector_is_reported_by_name() {
        let mut link = connected(AccessMode::ReadWrite, vec![]);
        if let LinkState::Connected { telemetry, .. } = &mut link {
            let mut snapshot = (**telemetry.as_ref().unwrap()).clone();
            snapshot.failed = vec![CollectorFailure {
                collector: Collector::Gpu,
                detail: "The GPU collector panicked and was isolated.".into(),
            }];
            *telemetry = Some(Arc::new(snapshot));
        }
        assert!(link.collector_failure(Collector::Gpu).is_some());
        assert!(link.collector_failure(Collector::Cpu).is_none());
    }

    #[test]
    fn a_device_row_details_firmware_and_kernel_binding() {
        let summary = DeviceSummary {
            id: KRAKEN_BASE,
            name: "NZXT Kraken Base".into(),
            firmware: Some("0200".into()),
            driver: "kraken2023".into(),
            health: DeviceHealth::ReadOnly,
        };
        assert_eq!(summary.detail(), "Firmware 0200 \u{00b7} kraken2023");

        let unknown = DeviceSummary {
            firmware: None,
            ..summary
        };
        assert_eq!(unknown.detail(), "Firmware unknown \u{00b7} kraken2023");

        let unbound = DeviceSummary {
            driver: "no kernel driver".into(),
            ..unknown
        };
        assert_eq!(
            unbound.detail(),
            "Firmware unknown \u{00b7} no kernel driver"
        );
    }
}
