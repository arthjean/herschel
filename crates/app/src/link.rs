// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The client's view of the daemon.
//!
//! The window never touches hardware. It asks the daemon what exists, what is
//! writable and what the sensors currently read, and derives every control
//! state from that answer. When the daemon is unreachable the shell stays
//! usable and read-only rather than showing values it cannot vouch for.

use std::sync::Arc;

use kori_core::DeviceId;
use kori_core::capability::{CapabilityId, CapabilityRecord, DeviceRecord};
use kori_core::ipc::{AccessMode, ChannelState, DaemonStatus};
use kori_core::profile::{Channel, Profile};
use kori_core::telemetry::{
    Collector, CollectorFailure, STALE_AFTER_MS, SafetyAlert, TelemetrySnapshot,
};

use crate::components::{ControlState, DeviceHealth};
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
                        conflicts
                            .iter()
                            .map(|conflict| conflict.detail.clone())
                            .collect::<Vec<_>>()
                            .join(" ")
                    )),
                }
            }
        }
    }

    /// State of a control that commands the hardware without naming a single
    /// capability, such as the active-profile selector.
    ///
    /// A profile carries whichever program it was saved with, so it cannot be
    /// gated on one capability id. It is gated on the three conditions US-004
    /// names instead: no daemon, no supported device, or a read-only conflict.
    pub fn write_state(&self) -> ControlState {
        let Self::Connected {
            status,
            capabilities,
            ..
        } = self
        else {
            return ControlState::disabled(
                "The background service is not running, so no control can be applied.",
            );
        };

        if let AccessMode::ReadOnly { conflicts } = &status.access {
            return ControlState::disabled(
                conflicts
                    .iter()
                    .map(|conflict| conflict.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }

        if capabilities.supported().next().is_none() {
            return ControlState::disabled("No supported NZXT device detected.");
        }

        ControlState::Enabled
    }

    /// State of a control that writes `capability` on `device`.
    ///
    /// This is the single gate every write control passes through, so a
    /// capability that is unproven, unavailable or unowned cannot present an
    /// enabled control anywhere in the interface.
    pub fn control_state(&self, device: DeviceId, capability: CapabilityId) -> ControlState {
        let Self::Connected { status, .. } = self else {
            return ControlState::disabled(
                "The background service is not running, so no control can be applied.",
            );
        };

        let Some(record) = self.device(device) else {
            return ControlState::disabled(format!("{device} is not connected."));
        };

        // The capability's own reason comes first when it has one. A daemon in
        // read-only mode reports one conflict for the whole machine, and that
        // conflict is about whatever made it read-only: showing "no writable
        // hwmon attribute" on a lighting control would name the wrong evidence
        // for the wrong device. A capability that is otherwise fine still falls
        // through to the conflict, because ownership outranks a clean record.
        match record.capability(capability) {
            Some(entry) => {
                if let Some(reason) = entry.state.blocked_reason() {
                    return ControlState::disabled(reason);
                }
            }
            None => {
                return ControlState::disabled(format!(
                    "{} is absent from the capability record for {device}.",
                    capability.label()
                ));
            }
        }

        if let AccessMode::ReadOnly { conflicts } = &status.access
            && let Some(conflict) = conflicts
                .iter()
                .find(|c| c.device.is_none_or(|d| d == device))
        {
            return ControlState::disabled(conflict.detail.clone());
        }

        ControlState::Enabled
    }

    /// State of a Cooling write control.
    ///
    /// Everything [`LinkState::control_state`] refuses, plus the two conditions
    /// only the Cooling screen can judge: telemetry that has gone stale, and a
    /// hardware state the daemon could not confirm. Both mean the readback a
    /// write would be checked against is not trustworthy, so no further write
    /// is offered until it is.
    pub fn cooling_state(
        &self,
        device: DeviceId,
        capability: CapabilityId,
        now_unix_ms: u64,
    ) -> ControlState {
        let base = self.control_state(device, capability);
        if !base.is_enabled() {
            return base;
        }

        let Some(snapshot) = self.telemetry() else {
            return ControlState::disabled(
                "No telemetry has arrived yet, so no write can be checked against a readback.",
            );
        };

        if !snapshot.kraken.present {
            return ControlState::disabled(
                "The Kraken is not reporting through its kernel driver. Controls stay read-only \
                 until it does.",
            );
        }

        let age_ms = now_unix_ms.saturating_sub(snapshot.kraken.at_unix_ms);
        if age_ms >= STALE_AFTER_MS {
            return ControlState::disabled(format!(
                "Cooling telemetry is {:.1} s old. Controls stay read-only until a fresh reading \
                 confirms the hardware state.",
                age_ms as f32 / 1000.0
            ));
        }

        ControlState::Enabled
    }

    /// State of a control that applies a whole cooling program.
    ///
    /// A program is offered only when *every* capability it writes is
    /// available, and the list comes from
    /// [`kori_core::profile::CoolingProgram::required_capabilities`], which is
    /// the same list the daemon checks in `program_incompatibilities`. A
    /// control that stays enabled here is therefore one the daemon would
    /// accept, rather than one that fails on the far side of the socket.
    ///
    /// A program that requires nothing writes nothing: the onboard fallback
    /// stays reachable whatever the hardware reports, which is what makes it a
    /// fallback.
    pub fn program_state(
        &self,
        device: DeviceId,
        required: &[CapabilityId],
        now_unix_ms: u64,
    ) -> ControlState {
        if self.status().is_none() {
            return ControlState::disabled(
                "The background service is not running, so no control can be applied.",
            );
        }
        required
            .iter()
            .map(|capability| self.cooling_state(device, *capability, now_unix_ms))
            .find(|state| !state.is_enabled())
            .unwrap_or(ControlState::Enabled)
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
mod tests {
    use super::*;
    use kori_core::capability::{
        CAPABILITY_SCHEMA_VERSION, Capability, CapabilityState, Evidenced, ProbeContext,
        SupportState, UsbIdentity,
    };
    use kori_core::ipc::{
        BlockedCapability, ConfigState, DeviceStatus, OwnershipConflict, PROTOCOL_VERSION,
    };
    use kori_core::profile::{CoolingProgram, TemperatureCurve};
    use kori_core::telemetry::{
        ChannelTelemetry, GpuTelemetry, KrakenTelemetry, PwmMode, Reading, SystemTelemetry,
        Unavailable,
    };
    use kori_core::{KRAKEN_BASE, RGB_CONTROLLER};

    fn device_record(id: DeviceId, capabilities: Vec<Capability>) -> DeviceRecord {
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

    fn telemetry(at_unix_ms: u64, present: bool) -> TelemetrySnapshot {
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

    fn connected(access: AccessMode, capabilities: Vec<Capability>) -> LinkState {
        connected_at(access, capabilities, Some(telemetry(1_000, true)))
    }

    fn connected_at(
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

    #[test]
    fn a_capability_with_its_own_reason_is_not_shadowed_by_a_machine_wide_conflict() {
        // The daemon reports one conflict for the whole machine when nothing is
        // writable. It is about hwmon, and it must not become the explanation
        // shown on a lighting control whose record already says exactly why the
        // controller is refused. US-013 requires the missing evidence to be
        // reported, not a neighbouring one.
        let link = connected(
            AccessMode::ReadOnly {
                conflicts: vec![OwnershipConflict {
                    device: None,
                    resource: "hwmon".into(),
                    detail: "No writable control attribute is available to this user.".into(),
                }],
            },
            vec![Capability {
                id: CapabilityId::RgbFixedColor,
                state: CapabilityState::Unvalidated {
                    reason: "The channel topology is not readable. permission denied on \
                             /dev/hidraw12."
                        .into(),
                },
            }],
        );

        let state = link.control_state(KRAKEN_BASE, CapabilityId::RgbFixedColor);
        let message = state.message().unwrap_or_default();
        assert!(state.is_disabled());
        assert!(message.contains("/dev/hidraw12"), "{message}");
        assert!(message.contains("US-013"), "{message}");
        assert!(!message.contains("hwmon"), "{message}");
    }

    #[test]
    fn ownership_still_outranks_a_capability_record_that_looks_fine() {
        // The other direction: a capability the record calls writable is still
        // refused while another process owns the device, because the record
        // describes the device and the conflict describes who is holding it.
        let link = connected(
            AccessMode::ReadOnly {
                conflicts: vec![OwnershipConflict {
                    device: Some(KRAKEN_BASE),
                    resource: "/dev/hidraw3".into(),
                    detail: "Another process owns this device.".into(),
                }],
            },
            vec![writable(CapabilityId::PumpDuty)],
        );

        let state = link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty);
        assert!(state.is_disabled());
        assert!(
            state
                .message()
                .unwrap_or_default()
                .contains("Another process"),
            "{:?}",
            state.message()
        );
    }

    #[test]
    fn an_unconflicted_writable_capability_stays_enabled() {
        let link = connected(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
        );
        assert!(
            link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty)
                .is_enabled()
        );
        // And a device that is not in the record at all is named as absent.
        let state = link.control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor);
        assert!(
            state
                .message()
                .unwrap_or_default()
                .contains("not connected"),
            "{:?}",
            state.message()
        );
    }

    fn writable(id: CapabilityId) -> Capability {
        Capability {
            id,
            state: CapabilityState::Available {
                writable: true,
                source: "/sys/class/hwmon/hwmon4/pwm1".into(),
            },
        }
    }

    #[test]
    fn without_a_daemon_every_control_is_disabled_with_one_actionable_message() {
        let link = LinkState::Unavailable {
            message: "The background service is not running. Start korid to enable controls."
                .into(),
        };

        let state = link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("background service"));
        assert!(link.banner().unwrap().contains("korid"));
        assert!(link.device_rows().is_empty());
        assert!(link.profiles().is_empty());
        assert!(link.telemetry().is_none());
        assert!(link.alerts().is_empty());

        // The profile selector commands the hardware without naming a single
        // capability, so it needs its own gate to satisfy the same criterion.
        let profile_selector = link.write_state();
        assert!(profile_selector.is_disabled());
        assert!(
            profile_selector
                .message()
                .unwrap()
                .contains("background service")
        );
    }

    #[test]
    fn a_read_only_conflict_also_disables_the_profile_selector() {
        let link = connected(
            AccessMode::ReadOnly {
                conflicts: vec![OwnershipConflict {
                    device: None,
                    resource: "hwmon".into(),
                    detail: "Another process owns this device.".into(),
                }],
            },
            vec![writable(CapabilityId::PumpDuty)],
        );

        let state = link.write_state();
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("Another process"));
    }

    #[test]
    fn the_profile_selector_is_enabled_only_when_a_device_can_be_written() {
        assert!(
            connected(
                AccessMode::ReadWrite,
                vec![writable(CapabilityId::PumpDuty)]
            )
            .write_state()
            .is_enabled()
        );

        let mut empty = connected(AccessMode::ReadWrite, vec![]);
        if let LinkState::Connected { capabilities, .. } = &mut empty {
            *capabilities = Arc::new(CapabilityRecord {
                devices: vec![],
                ..(**capabilities).clone()
            });
        }
        let state = empty.write_state();
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("No supported"));
    }

    #[test]
    fn a_writable_capability_enables_its_control() {
        let link = connected(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
        );
        assert!(
            link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty)
                .is_enabled()
        );
        assert_eq!(link.banner(), None);
    }

    #[test]
    fn an_unvalidated_capability_disables_its_control_and_names_the_story() {
        let link = connected(
            AccessMode::ReadWrite,
            vec![Capability {
                id: CapabilityId::LcdFrame,
                state: CapabilityState::Unvalidated {
                    reason: "The LCD transport is not proven on this firmware.".into(),
                },
            }],
        );

        let state = link.control_state(KRAKEN_BASE, CapabilityId::LcdFrame);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("US-016"));
    }

    #[test]
    fn a_read_only_conflict_disables_controls_and_shows_the_conflict() {
        let link = connected(
            AccessMode::ReadOnly {
                conflicts: vec![OwnershipConflict {
                    device: None,
                    resource: "hwmon".into(),
                    detail: "Another process owns this device.".into(),
                }],
            },
            vec![writable(CapabilityId::PumpDuty)],
        );

        let state = link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("Another process"));
        assert!(link.banner().unwrap().starts_with("Controls are read-only"));
    }

    #[test]
    fn a_capability_absent_from_the_record_disables_its_control() {
        let link = connected(AccessMode::ReadWrite, vec![]);
        let state = link.control_state(KRAKEN_BASE, CapabilityId::FanCurve);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("Fan curve"));
    }

    #[test]
    fn a_control_for_an_absent_device_is_disabled() {
        let link = connected(AccessMode::ReadWrite, vec![]);
        let state = link.control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("1e71:2021"));
    }

    #[test]
    fn stale_cooling_telemetry_disables_write_controls_and_says_how_old_it_is() {
        let link = connected(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
        );

        // Fresh: the underlying capability gate is the only one that applies.
        assert!(
            link.cooling_state(KRAKEN_BASE, CapabilityId::PumpDuty, 1_500)
                .is_enabled()
        );

        let stale = link.cooling_state(KRAKEN_BASE, CapabilityId::PumpDuty, 1_000 + STALE_AFTER_MS);
        assert!(stale.is_disabled());
        let message = stale.message().unwrap();
        assert!(message.contains("2.0 s old"), "{message}");
        assert!(message.contains("read-only"), "{message}");
    }

    #[test]
    fn an_absent_kraken_disables_cooling_controls_even_when_the_capability_is_writable() {
        let link = connected_at(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
            Some(telemetry(1_000, false)),
        );
        let state = link.cooling_state(KRAKEN_BASE, CapabilityId::PumpDuty, 1_100);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("kernel driver"));
    }

    #[test]
    fn cooling_controls_stay_disabled_until_the_first_sample_arrives() {
        let link = connected_at(
            AccessMode::ReadWrite,
            vec![writable(CapabilityId::PumpDuty)],
            None,
        );
        let state = link.cooling_state(KRAKEN_BASE, CapabilityId::PumpDuty, 1_100);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("No telemetry"));
        assert_eq!(link.kraken_age_ms(1_100), None);
    }

    /// A capability the device exposes but this user cannot write, which is
    /// what a udev rule covering only one channel produces.
    fn read_only(id: CapabilityId) -> Capability {
        Capability {
            id,
            state: CapabilityState::Available {
                writable: false,
                source: "/sys/class/hwmon/hwmon4/pwm2".into(),
            },
        }
    }

    #[test]
    fn each_cooling_channel_is_gated_on_its_own_capability() {
        // The probe resolves pwm1 and pwm2 independently, so a rule granting
        // only the pump leaves the fan read-only.
        let link = connected(
            AccessMode::ReadWrite,
            vec![
                writable(CapabilityId::PumpDuty),
                read_only(CapabilityId::FanDuty),
            ],
        );

        let pump = link.cooling_state(KRAKEN_BASE, Channel::Pump.duty_capability(), 1_100);
        assert!(pump.is_enabled(), "the pump is writable");

        let fan = link.cooling_state(KRAKEN_BASE, Channel::Fan.duty_capability(), 1_100);
        assert!(
            fan.is_disabled(),
            "a read-only fan must not present an enabled control"
        );
        assert!(fan.message().unwrap().contains("pwm2"), "{fan:?}");
    }

    #[test]
    fn a_program_is_refused_unless_every_channel_it_writes_is_writable() {
        let link = connected(
            AccessMode::ReadWrite,
            vec![
                writable(CapabilityId::PumpDuty),
                read_only(CapabilityId::FanDuty),
                writable(CapabilityId::PumpCurve),
                writable(CapabilityId::FanCurve),
            ],
        );

        // Fixed writes both duties, and one of them is refused.
        let fixed = CoolingProgram::Fixed { pump: 180, fan: 90 };
        let state = link.program_state(KRAKEN_BASE, &fixed.required_capabilities(), 1_100);
        assert!(
            state.is_disabled(),
            "Apply must not be offered for a program the daemon would refuse"
        );
        assert!(state.message().unwrap().contains("Read-only"), "{state:?}");

        // The curve program writes different capabilities, and both are open.
        let curve = CoolingProgram::Curve {
            pump: TemperatureCurve::flat(120),
            fan: TemperatureCurve::flat(120),
        };
        assert!(
            link.program_state(KRAKEN_BASE, &curve.required_capabilities(), 1_100)
                .is_enabled()
        );

        // The onboard program writes nothing, so it stays available.
        assert!(
            link.program_state(
                KRAKEN_BASE,
                &CoolingProgram::Onboard.required_capabilities(),
                1_100
            )
            .is_enabled()
        );
    }

    #[test]
    fn no_program_is_offered_without_a_daemon() {
        let link = LinkState::Unavailable {
            message: "The background service is not running.".into(),
        };
        for program in [
            CoolingProgram::Onboard,
            CoolingProgram::Fixed { pump: 180, fan: 90 },
        ] {
            let state = link.program_state(KRAKEN_BASE, &program.required_capabilities(), 1_100);
            assert!(state.is_disabled(), "{program:?}");
            assert!(state.message().unwrap().contains("background service"));
        }
    }

    #[test]
    fn a_program_is_refused_while_its_readback_is_stale() {
        let link = connected(
            AccessMode::ReadWrite,
            vec![
                writable(CapabilityId::PumpDuty),
                writable(CapabilityId::FanDuty),
            ],
        );
        let fixed = CoolingProgram::Fixed { pump: 180, fan: 90 };

        assert!(
            link.program_state(KRAKEN_BASE, &fixed.required_capabilities(), 1_500)
                .is_enabled()
        );
        let stale = link.program_state(
            KRAKEN_BASE,
            &fixed.required_capabilities(),
            1_000 + STALE_AFTER_MS,
        );
        assert!(stale.is_disabled());
        assert!(stale.message().unwrap().contains("2.0 s old"), "{stale:?}");
    }

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
