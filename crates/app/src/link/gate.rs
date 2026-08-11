// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The single gate every write control passes through.
//!
//! A capability that is unproven, unavailable or unowned cannot present an
//! enabled control anywhere in the interface, because every control asks here
//! first. Separated from the readers next door on purpose: what the daemon
//! reported is a fact, and whether a control may act on it is a decision, and
//! this is the one place that decision is made.
//!
//! Each answer carries the reason in operator language rather than a bare
//! `false`. A refusal the operator cannot read is a control that is broken as
//! far as they can tell.

use kori_core::DeviceId;
use kori_core::capability::CapabilityId;
use kori_core::ipc::AccessMode;
use kori_core::telemetry::STALE_AFTER_MS;

use crate::components::ControlState;

use super::{LinkState, conflict_detail};

impl LinkState {
    /// State of a control that commands the hardware without naming a single
    /// capability, such as the active-profile selector.
    ///
    /// A profile carries whichever program it was saved with, so it cannot be
    /// gated on one capability id. It is gated on three conditions instead: no
    /// daemon, no supported device, or a read-only conflict.
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
            return ControlState::disabled(conflict_detail(conflicts));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::fixture::*;
    use kori_core::capability::CapabilityRecord;
    use kori_core::capability::{Capability, CapabilityState};
    use kori_core::ipc::OwnershipConflict;
    use kori_core::profile::{Channel, CoolingProgram, TemperatureCurve};
    use kori_core::{KRAKEN_BASE, RGB_CONTROLLER};
    use std::sync::Arc;

    #[test]
    fn a_capability_with_its_own_reason_is_not_shadowed_by_a_machine_wide_conflict() {
        // The daemon reports one conflict for the whole machine when nothing is
        // writable. It is about hwmon, and it must not become the explanation
        // shown on a lighting control whose record already says exactly why the
        // controller is refused. The missing evidence has to be the one
        // reported, not a neighboring one.
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
    fn an_unvalidated_capability_disables_its_control_and_states_the_reason() {
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
        assert!(
            state
                .message()
                .unwrap()
                .contains("not proven on this firmware")
        );
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
}
