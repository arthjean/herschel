// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! What decides whether a write is permitted.
//!
//! [`crate::probe`] records what the machine exposes and opens no device node
//! doing it. This module answers the separate question the record cannot: given
//! what a device actually answered, is this product allowed to send it a
//! command? Nothing here reads sysfs, and nothing here writes to hardware. It
//! resolves evidence into a [`CapabilityState`], and a control the operator can
//! reach is exactly one that came out of here `Available` and writable.
//!
//! The two ladders are deliberately conservative and ordered the same way, from
//! the cheapest evidence to the strongest: no topology recorded at all, then a
//! device that answered nothing, then a firmware this project has never driven.
//! They part company only where the evidence genuinely differs, and each rung
//! names the evidence it is missing so a disabled control can say why.

use kori_core::capability::{
    Capability, CapabilityId, CapabilityRecord, CapabilityState, DeviceRecord, Evidenced,
    LcdTopology, RgbTopology,
};
use kori_core::{KRAKEN_BASE, RGB_CONTROLLER};

/// Swap one device's capabilities for a freshly resolved set.
///
/// Only the listed ids are replaced, so a device that carries surfaces this
/// topology says nothing about keeps them. Assigning the whole vector instead
/// works only for a device whose every capability comes from one topology,
/// which is a fact about today's allowlist rather than about this function.
fn replace_capabilities(
    device: &mut DeviceRecord,
    replaced: &[CapabilityId],
    fresh: Vec<Capability>,
) {
    device
        .capabilities
        .retain(|capability| !replaced.contains(&capability.id));
    device.capabilities.extend(fresh);
}

/// The last two rungs both write gates share: a firmware, and a validated one.
///
/// Returns the refusal when there is one, `None` when the device cleared both.
/// The heads of the two ladders genuinely differ (a channel count on one side,
/// a firmware generation on the other), so only the common tail is shared: a
/// mechanism general enough to cover both heads would hide which evidence
/// actually opened the path.
fn firmware_gate(
    firmware: &Evidenced<String>,
    validated: &[&str],
    subject: &str,
) -> Option<CapabilityState> {
    let Some(firmware) = firmware.value() else {
        return Some(CapabilityState::Unvalidated {
            reason: format!(
                "The {subject} did not report a firmware revision, so its command set \
                 cannot be matched against a validated one."
            ),
        });
    };
    (!validated.contains(&firmware.as_str())).then(|| CapabilityState::Unvalidated {
        reason: format!("Firmware {firmware} is not validated for this operation."),
    })
}

/// Resolve the LCD surface against whatever the Kraken actually answered.
///
/// The two capabilities part company on one thing only. Brightness and
/// orientation travel over the `hidraw` node; a frame additionally needs the
/// bulk interface. A machine with the `hidraw` rule installed and not the
/// `usbfs` one is a real state, and it is reported as what it is rather than
/// collapsed into one refusal.
pub(crate) fn lcd_capabilities(topology: Option<&LcdTopology>) -> Vec<Capability> {
    let control = lcd_state(topology);
    let frame = match (&control, topology) {
        (CapabilityState::Available { .. }, Some(topology)) if !topology.bulk_node.is_known() => {
            CapabilityState::Unavailable {
                reason: topology
                    .bulk_node
                    .reason()
                    .unwrap_or("The bulk interface could not be claimed.")
                    .to_string(),
            }
        }
        _ => control.clone(),
    };

    vec![
        Capability {
            id: CapabilityId::LcdFrame,
            state: frame,
        },
        Capability {
            id: CapabilityId::LcdDisplayControl,
            state: control,
        },
    ]
}

fn lcd_state(topology: Option<&LcdTopology>) -> CapabilityState {
    let unvalidated = |reason: String| CapabilityState::Unvalidated { reason };

    let Some(topology) = topology else {
        return unvalidated("The panel transport is not recorded yet.".to_string());
    };

    let Some(node) = topology.hid_node.value() else {
        return CapabilityState::Unavailable {
            reason: topology
                .hid_node
                .reason()
                .unwrap_or("The device exposes no usable node.")
                .to_string(),
        };
    };

    if !topology.answered() {
        return unvalidated(format!(
            "The panel did not answer, so this unit may carry no display. {}",
            topology
                .display
                .reason()
                .unwrap_or("No reason was recorded.")
        ));
    }

    // The generation check sits before the shared tail: a 1.x Kraken would be
    // sent a transfer sequence written for a firmware it is not, which is a
    // different refusal from "this exact revision was never driven".
    if let Some(firmware) = topology.firmware.value()
        && crate::hid::firmware_major(firmware) != Some(crate::lcd::SUPPORTED_FIRMWARE_MAJOR)
    {
        return unvalidated(format!(
            "Firmware {firmware} is not the {}.x generation this transfer sequence \
             was written for.",
            crate::lcd::SUPPORTED_FIRMWARE_MAJOR
        ));
    }
    if let Some(refusal) =
        firmware_gate(&topology.firmware, crate::lcd::VALIDATED_FIRMWARE, "panel")
    {
        return refusal;
    }

    CapabilityState::Available {
        writable: true,
        source: node.clone(),
    }
}

/// Fold a live Kraken answer into a record and re-resolve its capabilities.
pub fn attach_lcd_topology(record: &mut CapabilityRecord, topology: LcdTopology) {
    let Some(device) = record
        .devices
        .iter_mut()
        .find(|device| device.usb.id == KRAKEN_BASE)
    else {
        return;
    };
    replace_capabilities(
        device,
        &[CapabilityId::LcdFrame, CapabilityId::LcdDisplayControl],
        lcd_capabilities(Some(&topology)),
    );
    device.lcd = Some(topology);
}

/// Resolve the RGB surface against whatever the controller actually answered.
///
/// Only a controller that answered a channel list *and* reports a firmware the
/// write probe exercised becomes writable.
pub(crate) fn rgb_capabilities(topology: Option<&RgbTopology>) -> Vec<Capability> {
    let state = rgb_state(topology);
    [CapabilityId::RgbFixedColor, CapabilityId::RgbEffects]
        .into_iter()
        .map(|id| Capability {
            id,
            state: state.clone(),
        })
        .collect()
}

fn rgb_state(topology: Option<&RgbTopology>) -> CapabilityState {
    let unvalidated = |reason: String| CapabilityState::Unvalidated { reason };

    let Some(topology) = topology else {
        return unvalidated(
            "The channel topology and packet format are not recorded yet.".to_string(),
        );
    };

    let Some(node) = topology.node.value() else {
        return CapabilityState::Unavailable {
            reason: topology
                .node
                .reason()
                .unwrap_or("The controller exposes no usable node.")
                .to_string(),
        };
    };

    if let Some(reason) = topology.channels.reason() {
        // The reason already carries its own punctuation, so it is joined
        // rather than wrapped: this string is shown verbatim on a control.
        return unvalidated(format!("The channel topology is not readable. {reason}"));
    }
    if topology.channel_count().unwrap_or(0) == 0 {
        return unvalidated("The controller reported zero lighting channels.".to_string());
    }

    if let Some(refusal) = firmware_gate(
        &topology.firmware,
        crate::rgb::VALIDATED_FIRMWARE,
        "controller",
    ) {
        return refusal;
    }

    CapabilityState::Available {
        writable: true,
        source: node.clone(),
    }
}

/// Fold a live controller answer into a record and re-resolve its capabilities.
///
/// Separate from [`crate::probe::probe`] because the sysfs pass opens no device
/// node: it can see that a controller exists but not what it contains. The
/// daemon owns the device, so it is the daemon that asks and then calls this.
pub fn attach_rgb_topology(record: &mut CapabilityRecord, topology: RgbTopology) {
    let Some(device) = record
        .devices
        .iter_mut()
        .find(|device| device.usb.id == RGB_CONTROLLER)
    else {
        return;
    };
    replace_capabilities(
        device,
        &[CapabilityId::RgbFixedColor, CapabilityId::RgbEffects],
        rgb_capabilities(Some(&topology)),
    );
    device.rgb = Some(topology);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::probe;
    use crate::testing::FakeSysfs;
    use kori_core::capability::{LcdDisplaySettings, RgbChannel};

    /// A topology as a healthy controller would report it.
    fn answered_topology(firmware: &str, channels: u8) -> RgbTopology {
        RgbTopology {
            node: Evidenced::known("/dev/hidraw12".into(), "sysfs"),
            firmware: Evidenced::known(firmware.into(), "report 0x11 0x01"),
            channels: Evidenced::known(
                (1..=channels)
                    .map(|index| RgbChannel {
                        index,
                        accessories: Vec::new(),
                        led_count: Evidenced::unknown("not reported", "report 0x21 0x03"),
                    })
                    .collect(),
                "report 0x21 0x03",
            ),
        }
    }

    /// A Kraken topology as an answering unit with a panel reports it.
    fn answered_lcd(firmware: &str) -> LcdTopology {
        LcdTopology {
            hid_node: Evidenced::known("/dev/hidraw10".into(), "sysfs"),
            bulk_node: Evidenced::known("/dev/bus/usb/001/004 interface 0".into(), "usbfs"),
            firmware: Evidenced::known(firmware.into(), "report 0x11 0x01"),
            panel: Evidenced::known(crate::lcd::candidate_panel(), "candidate"),
            display: Evidenced::known(
                LcdDisplaySettings {
                    brightness_percent: 60,
                    quarter_turns: 0,
                },
                "report 0x31 0x01",
            ),
        }
    }

    #[test]
    fn unvalidated_surfaces_state_why_they_are_blocked() {
        let fake = FakeSysfs::development_machine("gates-unvalidated");
        let record = probe(&fake.root());

        let lcd = &record
            .device(KRAKEN_BASE)
            .unwrap()
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state;
        assert!(matches!(lcd, CapabilityState::Unvalidated { .. }));
        assert!(!lcd.blocked_reason().unwrap().is_empty());

        let rgb = &record
            .device(RGB_CONTROLLER)
            .unwrap()
            .capability(CapabilityId::RgbFixedColor)
            .unwrap()
            .state;
        assert!(matches!(rgb, CapabilityState::Unvalidated { .. }));
        assert!(!rgb.blocked_reason().unwrap().is_empty());
        assert!(
            !record
                .device(RGB_CONTROLLER)
                .unwrap()
                .can_write(CapabilityId::RgbFixedColor)
        );
    }

    #[test]
    fn an_unanswered_controller_leaves_rgb_unvalidated_and_names_the_reason() {
        let fake = FakeSysfs::development_machine("gates-rgb-silent");
        let mut record = probe(&fake.root());
        attach_rgb_topology(
            &mut record,
            RgbTopology::unavailable(
                "permission denied on /dev/hidraw12. Check the installed udev rule.",
                "/dev/hidraw12",
            ),
        );

        let rgb = record.device(RGB_CONTROLLER).unwrap();
        assert!(!rgb.can_write(CapabilityId::RgbFixedColor));
        assert!(!rgb.can_write(CapabilityId::RgbEffects));
        let reason = rgb
            .capability(CapabilityId::RgbFixedColor)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("udev"), "{reason}");
    }

    #[test]
    fn an_unvalidated_firmware_is_named_and_stays_read_only() {
        let fake = FakeSysfs::development_machine("gates-rgb-firmware");
        let mut record = probe(&fake.root());
        attach_rgb_topology(&mut record, answered_topology("9.9.9", 3));

        let rgb = record.device(RGB_CONTROLLER).unwrap();
        // The topology was recorded even though the command set was not
        // validated: evidence and permission are separate questions.
        assert_eq!(
            rgb.rgb
                .as_ref()
                .and_then(|topology| topology.channel_count()),
            Some(3)
        );
        assert!(!rgb.can_write(CapabilityId::RgbFixedColor));
        let reason = rgb
            .capability(CapabilityId::RgbFixedColor)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("9.9.9"), "{reason}");
    }

    #[test]
    fn a_controller_reporting_no_channel_is_never_writable() {
        let fake = FakeSysfs::development_machine("gates-rgb-empty");
        let mut record = probe(&fake.root());
        attach_rgb_topology(&mut record, answered_topology("1.0.0", 0));

        let rgb = record.device(RGB_CONTROLLER).unwrap();
        assert!(!rgb.can_write(CapabilityId::RgbFixedColor));
        let reason = rgb
            .capability(CapabilityId::RgbFixedColor)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("zero lighting channels"), "{reason}");
    }

    #[test]
    fn a_validated_firmware_is_the_only_thing_that_opens_the_write_path() {
        let fake = FakeSysfs::development_machine("gates-rgb-validated");

        for firmware in crate::rgb::VALIDATED_FIRMWARE {
            let mut record = probe(&fake.root());
            attach_rgb_topology(&mut record, answered_topology(firmware, 3));
            let rgb = record.device(RGB_CONTROLLER).unwrap();
            assert!(
                rgb.can_write(CapabilityId::RgbFixedColor),
                "{firmware} is recorded as validated but did not open the write path"
            );
            assert!(rgb.can_write(CapabilityId::RgbEffects));
        }

        // And nothing else does, whatever else the controller reports.
        let mut record = probe(&fake.root());
        attach_rgb_topology(&mut record, answered_topology("0.0.0", 3));
        assert!(
            !record
                .device(RGB_CONTROLLER)
                .unwrap()
                .can_write(CapabilityId::RgbFixedColor)
        );
    }

    #[test]
    fn a_kraken_that_never_answered_the_display_report_stays_read_only() {
        let fake = FakeSysfs::development_machine("gates-lcd-silent");
        let mut record = probe(&fake.root());
        let mut topology = answered_lcd("2.0.4");
        topology.display = Evidenced::unknown(
            "the controller sent no display settings answer within 2000 ms",
            "report 0x31 0x01",
        );
        attach_lcd_topology(&mut record, topology);

        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert!(!kraken.can_write(CapabilityId::LcdFrame));
        assert!(!kraken.can_write(CapabilityId::LcdDisplayControl));
        let reason = kraken
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("may carry no display"), "{reason}");
    }

    #[test]
    fn a_firmware_from_another_generation_is_named_and_refused() {
        let fake = FakeSysfs::development_machine("gates-lcd-generation");
        let mut record = probe(&fake.root());
        attach_lcd_topology(&mut record, answered_lcd("1.4.0"));

        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert!(!kraken.can_write(CapabilityId::LcdFrame));
        let reason = kraken
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("1.4.0"), "{reason}");
        assert!(reason.contains("2.x"), "{reason}");
    }

    #[test]
    fn an_unvalidated_firmware_of_the_right_generation_is_still_refused() {
        let fake = FakeSysfs::development_machine("gates-lcd-unvalidated");
        let mut record = probe(&fake.root());
        attach_lcd_topology(&mut record, answered_lcd("2.9.9"));

        let kraken = record.device(KRAKEN_BASE).unwrap();
        // The topology was recorded even though the sequence was never run on
        // this revision: evidence and permission are separate questions.
        assert!(kraken.lcd.as_ref().is_some_and(|lcd| lcd.answered()));
        assert!(!kraken.can_write(CapabilityId::LcdFrame));
        let reason = kraken
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("2.9.9"), "{reason}");
        assert!(reason.contains("not validated"), "{reason}");
    }

    #[test]
    fn a_validated_firmware_is_the_only_thing_that_opens_the_frame_path() {
        let fake = FakeSysfs::development_machine("gates-lcd-validated");

        for firmware in crate::lcd::VALIDATED_FIRMWARE {
            let mut record = probe(&fake.root());
            attach_lcd_topology(&mut record, answered_lcd(firmware));
            let kraken = record.device(KRAKEN_BASE).unwrap();
            assert!(
                kraken.can_write(CapabilityId::LcdFrame),
                "{firmware} is recorded as validated but did not open the frame path"
            );
            assert!(kraken.can_write(CapabilityId::LcdDisplayControl));
        }

        // And an unrecorded revision does not, which is the state every Kraken
        // outside the probed one stays in.
        let mut record = probe(&fake.root());
        attach_lcd_topology(&mut record, answered_lcd("2.0.4"));
        assert_eq!(
            record
                .device(KRAKEN_BASE)
                .unwrap()
                .can_write(CapabilityId::LcdFrame),
            crate::lcd::is_validated_firmware("2.0.4")
        );
    }

    #[test]
    fn a_missing_bulk_node_disables_frames_without_disabling_brightness() {
        let fake = FakeSysfs::development_machine("gates-lcd-bulk");
        let mut record = probe(&fake.root());
        let mut topology = answered_lcd("2.0.4");
        topology.bulk_node = Evidenced::unknown(
            "permission denied on /dev/bus/usb/001/004. Check the installed udev rule.",
            "usbfs",
        );
        attach_lcd_topology(&mut record, topology);

        let kraken = record.device(KRAKEN_BASE).unwrap();
        let frame = &kraken.capability(CapabilityId::LcdFrame).unwrap().state;
        let control = &kraken
            .capability(CapabilityId::LcdDisplayControl)
            .unwrap()
            .state;
        // Both are refused here because the firmware is unvalidated too, so the
        // assertion is about which reason each carries rather than about the
        // permission alone.
        assert!(!frame.is_writable());
        assert!(!control.is_writable());
        assert!(
            frame.blocked_reason().unwrap().contains("2.0.4"),
            "{frame:?}"
        );
        assert!(control.blocked_reason().unwrap().contains("2.0.4"));
    }

    #[test]
    fn a_claimed_bulk_interface_is_what_separates_a_frame_from_a_brightness() {
        // The one state where the two LCD capabilities differ: a validated
        // panel whose framebuffer interface could not be claimed. Brightness
        // travels over the hidraw node and still works; the frame does not, and
        // says so in the words the claim itself used.
        let fake = FakeSysfs::development_machine("gates-lcd-bulk-split");
        let Some(validated) = crate::lcd::VALIDATED_FIRMWARE.first() else {
            return;
        };

        let mut record = probe(&fake.root());
        let mut topology = answered_lcd(validated);
        topology.bulk_node = Evidenced::unknown(
            "permission denied on /dev/bus/usb/001/004. Check the installed udev rule.",
            "usbfs",
        );
        attach_lcd_topology(&mut record, topology);

        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert!(
            kraken.can_write(CapabilityId::LcdDisplayControl),
            "brightness does not need the bulk interface"
        );
        assert!(!kraken.can_write(CapabilityId::LcdFrame));
        let reason = kraken
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("/dev/bus/usb/001/004"), "{reason}");
        assert!(reason.contains("udev"), "{reason}");
    }

    #[test]
    fn the_rgb_controller_never_gains_an_lcd_topology() {
        let fake = FakeSysfs::development_machine("gates-lcd-scope");
        let mut record = probe(&fake.root());
        attach_lcd_topology(&mut record, answered_lcd("2.0.4"));
        assert!(record.device(RGB_CONTROLLER).unwrap().lcd.is_none());
        // And attaching it twice leaves exactly one of each capability.
        attach_lcd_topology(&mut record, answered_lcd("2.0.4"));
        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert_eq!(
            kraken
                .capabilities
                .iter()
                .filter(|c| c.id == CapabilityId::LcdFrame)
                .count(),
            1
        );
    }

    #[test]
    fn the_kraken_never_gains_an_rgb_topology() {
        let fake = FakeSysfs::development_machine("gates-rgb-scope");
        let mut record = probe(&fake.root());
        attach_rgb_topology(&mut record, answered_topology("1.0.0", 3));
        assert!(record.device(KRAKEN_BASE).unwrap().rgb.is_none());
    }
}
