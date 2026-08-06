// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The client's view of the daemon.
//!
//! The window never touches hardware. It asks the daemon what exists and what
//! is writable, and derives every control state from that answer. When the
//! daemon is unreachable the shell stays usable and read-only rather than
//! showing values it cannot vouch for.

use std::path::Path;

use nzxt_core::DeviceId;
use nzxt_core::capability::{CapabilityId, CapabilityRecord, DeviceRecord};
use nzxt_core::client::{Client, ClientError};
use nzxt_core::ipc::{AccessMode, DaemonStatus, Request, Response};
use nzxt_core::profile::Profile;

use crate::components::{ControlState, DeviceHealth};

/// What the client knows about the daemon right now.
pub enum LinkState {
    /// Connected, with the state the daemon reported.
    Connected {
        status: Box<DaemonStatus>,
        capabilities: Box<CapabilityRecord>,
        profiles: Vec<Profile>,
    },
    /// Not connected. `message` is shown verbatim to the operator.
    Unavailable { message: String },
}

impl LinkState {
    /// Connect to `socket` and fetch everything the shell renders.
    ///
    /// One round of requests at startup. Live streaming belongs to the
    /// telemetry stories, and adding it here would mean inventing values.
    pub fn connect(socket: &Path) -> Self {
        match Self::try_connect(socket) {
            Ok(state) => state,
            Err(error) => Self::Unavailable {
                message: error.operator_message(),
            },
        }
    }

    fn try_connect(socket: &Path) -> Result<Self, ClientError> {
        let mut client = Client::connect(socket)?;
        let status = client.status()?;
        let capabilities = client.capabilities()?;
        let profiles = match client.request(Request::Profiles)? {
            Response::Profiles { profiles, .. } => profiles,
            Response::Error(error) => return Err(ClientError::Refused(error)),
            _ => Vec::new(),
        };

        Ok(Self::Connected {
            status: Box::new(status),
            capabilities: Box::new(capabilities),
            profiles,
        })
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

    pub fn active_profile(&self) -> Option<&str> {
        self.status().map(|status| status.active_profile.as_str())
    }

    pub fn device(&self, id: DeviceId) -> Option<&DeviceRecord> {
        self.capabilities()?.device(id)
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

        if let AccessMode::ReadOnly { conflicts } = &status.access
            && let Some(conflict) = conflicts
                .iter()
                .find(|c| c.device.is_none_or(|d| d == device))
        {
            return ControlState::disabled(conflict.detail.clone());
        }

        let Some(record) = self.device(device) else {
            return ControlState::disabled(format!("{device} is not connected."));
        };

        match record.capability(capability) {
            Some(entry) => match entry.state.blocked_reason() {
                None => ControlState::Enabled,
                Some(reason) => ControlState::disabled(reason),
            },
            None => ControlState::disabled(format!(
                "{} is absent from the capability record for {device}.",
                capability.label()
            )),
        }
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
    pub fn detail(&self) -> String {
        let firmware = self
            .firmware
            .clone()
            .unwrap_or_else(|| "firmware unknown".to_string());
        format!("Firmware {firmware}, bound to {}", self.driver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::capability::{
        CAPABILITY_SCHEMA_VERSION, Capability, CapabilityState, Evidenced, ProbeContext,
        SupportState, UsbIdentity,
    };
    use nzxt_core::ipc::{
        BlockedCapability, ConfigState, DeviceStatus, OwnershipConflict, PROTOCOL_VERSION,
    };
    use nzxt_core::{KRAKEN_BASE, RGB_CONTROLLER};

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
            capabilities,
        }
    }

    fn connected(access: AccessMode, capabilities: Vec<Capability>) -> LinkState {
        let record = device_record(KRAKEN_BASE, capabilities.clone());
        LinkState::Connected {
            status: Box::new(DaemonStatus {
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
                socket_path: "/run/user/1000/nzxt-control/nzxt-control.sock".into(),
            }),
            capabilities: Box::new(CapabilityRecord {
                schema_version: CAPABILITY_SCHEMA_VERSION,
                context: ProbeContext {
                    kernel_release: Evidenced::known("7.1.6".into(), "uname(2)"),
                    probed_at_unix_ms: 1,
                },
                devices: vec![record],
                rejected: vec![],
            }),
            profiles: vec![Profile::safe()],
        }
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
            message:
                "The background service is not running. Start nzxt-controld to enable controls."
                    .into(),
        };

        let state = link.control_state(KRAKEN_BASE, CapabilityId::PumpDuty);
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("background service"));
        assert!(link.banner().unwrap().contains("nzxt-controld"));
        assert!(link.device_rows().is_empty());
        assert!(link.profiles().is_empty());

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
            capabilities.devices.clear();
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
                    required_story: "US-016".into(),
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
    fn a_recovered_configuration_takes_priority_in_the_banner() {
        let mut link = connected(AccessMode::ReadWrite, vec![]);
        if let LinkState::Connected { status, .. } = &mut link {
            status.config = ConfigState::Recovered {
                detail: "expected schema 1, found 9".into(),
                preserved_path: "/home/a/.config/nzxt-control/config.toml.corrupt.1".into(),
                recovery_action: "Save a profile to write a fresh configuration.".into(),
            };
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
    fn a_device_row_details_firmware_and_kernel_binding() {
        let summary = DeviceSummary {
            id: KRAKEN_BASE,
            name: "NZXT Kraken Base".into(),
            firmware: Some("0200".into()),
            driver: "kraken2023".into(),
            health: DeviceHealth::ReadOnly,
        };
        assert_eq!(summary.detail(), "Firmware 0200, bound to kraken2023");

        let unknown = DeviceSummary {
            firmware: None,
            ..summary
        };
        assert!(unknown.detail().contains("firmware unknown"));
    }
}
