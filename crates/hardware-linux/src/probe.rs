// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Assemble the versioned capability record.
//!
//! The probe is read-only by construction: it reads sysfs attributes and tests
//! permissions with `access(2)`. It never opens a writable descriptor, and a
//! device outside the allowlist is recorded and then left untouched.

use std::time::{SystemTime, UNIX_EPOCH};

use nzxt_core::capability::{
    CAPABILITY_SCHEMA_VERSION, Capability, CapabilityId, CapabilityRecord, CapabilityState,
    DeviceRecord, Evidenced, HwmonCapabilities, ProbeContext, RejectedDevice, SupportState,
};
use nzxt_core::{DeviceId, KRAKEN_BASE, RGB_CONTROLLER, is_allowlisted};

use crate::hwmon::{self, HwmonInstance, curve_is_complete};
use crate::sysfs::SysfsRoot;
use crate::usb;

/// Story that must validate the RGB protocol before any RGB write.
pub const RGB_VALIDATION_STORY: &str = "US-013";
/// Story that must validate the LCD transport before any frame is sent.
pub const LCD_VALIDATION_STORY: &str = "US-016";

/// Run a complete read-only probe of the machine.
pub fn probe(root: &SysfsRoot) -> CapabilityRecord {
    let hwmon_instances = hwmon::discover(root);

    let mut devices = Vec::new();
    let mut rejected = Vec::new();

    for discovered in usb::enumerate(root) {
        if !is_allowlisted(discovered.id) {
            rejected.push(RejectedDevice {
                id: discovered.id,
                sysfs_path: discovered.path.display().to_string(),
                reason: "Device is not on the validated allowlist. Nothing was opened.".to_string(),
            });
            continue;
        }

        let hwmon = hwmon_instances
            .iter()
            .find(|instance: &&HwmonInstance| instance.device == discovered.id)
            .map(|instance| instance.capabilities.clone());

        devices.push(DeviceRecord {
            support: SupportState::Supported,
            capabilities: capabilities_for(discovered.id, hwmon.as_ref()),
            usb: usb::identity(discovered.id, &discovered.path),
            interfaces: usb::interfaces(&discovered.path),
            hwmon,
        });
    }

    CapabilityRecord {
        schema_version: CAPABILITY_SCHEMA_VERSION,
        context: ProbeContext {
            kernel_release: kernel_release(),
            probed_at_unix_ms: unix_millis(),
        },
        devices,
        rejected,
    }
}

/// Milliseconds since the Unix epoch, or 0 when the clock is before it.
pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Kernel release, from `uname(2)` rather than a sysfs path.
///
/// It describes the running kernel, so it must not follow a relocated sysfs
/// root: a fixture probe still runs on this kernel.
fn kernel_release() -> Evidenced<String> {
    let release = rustix::system::uname();
    match release.release().to_str() {
        Ok(release) if !release.is_empty() => Evidenced::known(release.to_string(), "uname(2)"),
        _ => Evidenced::unknown("kernel release is not readable", "uname(2)"),
    }
}

/// Resolve every capability of one allowlisted device.
fn capabilities_for(id: DeviceId, hwmon: Option<&HwmonCapabilities>) -> Vec<Capability> {
    match id {
        KRAKEN_BASE => kraken_capabilities(hwmon),
        RGB_CONTROLLER => rgb_capabilities(),
        _ => Vec::new(),
    }
}

fn kraken_capabilities(hwmon: Option<&HwmonCapabilities>) -> Vec<Capability> {
    let Some(hwmon) = hwmon else {
        let reason = format!(
            "No {} hwmon instance is bound to this device.",
            hwmon::KRAKEN_DRIVER
        );
        return [
            CapabilityId::LiquidTemperature,
            CapabilityId::PumpSpeed,
            CapabilityId::FanSpeed,
            CapabilityId::PumpDuty,
            CapabilityId::FanDuty,
            CapabilityId::PumpCurve,
            CapabilityId::FanCurve,
        ]
        .into_iter()
        .map(|id| Capability {
            id,
            state: CapabilityState::Unavailable {
                reason: reason.clone(),
            },
        })
        .chain(lcd_capabilities())
        .collect();
    };

    let reading = |id: CapabilityId, attribute: &str| Capability {
        id,
        state: match hwmon.attribute(attribute) {
            Some(entry) if entry.readable => CapabilityState::Available {
                // A reading is never writable, whatever the file mode says.
                writable: false,
                source: entry.path.clone(),
            },
            Some(entry) => CapabilityState::Unavailable {
                reason: format!("{} is present but not readable.", entry.path),
            },
            None => CapabilityState::Unavailable {
                reason: format!("{attribute} is absent from {}.", hwmon.path),
            },
        },
    };

    let control = |id: CapabilityId, attribute: &str| Capability {
        id,
        state: match hwmon.attribute(attribute) {
            Some(entry) => CapabilityState::Available {
                writable: entry.writable,
                source: entry.path.clone(),
            },
            None => CapabilityState::Unavailable {
                reason: format!("{attribute} is absent from {}.", hwmon.path),
            },
        },
    };

    let curve = |id: CapabilityId, temp_index: u8| Capability {
        id,
        state: match hwmon
            .curve_points
            .iter()
            .find(|channel| channel.temp_index == temp_index)
        {
            Some(channel) if curve_is_complete(channel) => CapabilityState::Available {
                writable: channel.writable,
                source: hwmon::attribute_path(hwmon, &format!("temp{temp_index}_auto_point*_pwm"))
                    .display()
                    .to_string(),
            },
            Some(channel) => CapabilityState::Unavailable {
                reason: format!(
                    "temp{temp_index} exposes {} curve points, not the 40 the ABI requires.",
                    channel.point_count
                ),
            },
            None => CapabilityState::Unavailable {
                reason: format!("temp{temp_index} exposes no curve points."),
            },
        },
    };

    vec![
        reading(CapabilityId::LiquidTemperature, "temp1_input"),
        reading(CapabilityId::PumpSpeed, "fan1_input"),
        reading(CapabilityId::FanSpeed, "fan2_input"),
        control(CapabilityId::PumpDuty, "pwm1"),
        control(CapabilityId::FanDuty, "pwm2"),
        curve(CapabilityId::PumpCurve, 1),
        curve(CapabilityId::FanCurve, 2),
    ]
    .into_iter()
    .chain(lcd_capabilities())
    .collect()
}

/// The LCD surface stays unvalidated until its transport story lands.
fn lcd_capabilities() -> impl Iterator<Item = Capability> {
    [CapabilityId::LcdFrame, CapabilityId::LcdDisplayControl]
        .into_iter()
        .map(|id| Capability {
            id,
            state: CapabilityState::Unvalidated {
                required_story: LCD_VALIDATION_STORY.to_string(),
                reason: "The LCD transport is not proven on this firmware.".to_string(),
            },
        })
}

/// The RGB protocol stays unvalidated until its probe story lands.
fn rgb_capabilities() -> Vec<Capability> {
    [CapabilityId::RgbFixedColor, CapabilityId::RgbEffects]
        .into_iter()
        .map(|id| Capability {
            id,
            state: CapabilityState::Unvalidated {
                required_story: RGB_VALIDATION_STORY.to_string(),
                reason: "The channel topology and packet format are not recorded yet.".to_string(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeSysfs, running_as_root};

    fn fixture(name: &str) -> FakeSysfs {
        let fake = FakeSysfs::new(name);
        fake.add_kraken();
        fake.add_kraken_hwmon();
        fake.add_rgb_controller();
        fake.add_unrelated_device();
        fake.add_unrelated_hwmon();
        fake
    }

    #[test]
    fn probe_records_identity_interfaces_and_hwmon_for_both_devices() {
        let fake = fixture("probe-identity");
        let record = probe(&fake.root());

        assert_eq!(record.schema_version, CAPABILITY_SCHEMA_VERSION);
        assert_eq!(record.devices.len(), 2);

        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert_eq!(
            kraken.usb.product.value().map(String::as_str),
            Some("NZXT Kraken Base")
        );
        assert_eq!(
            kraken.usb.firmware.value().map(String::as_str),
            Some("0200")
        );
        assert!(kraken.usb.serial.is_known());
        assert_eq!(kraken.interfaces.len(), 2);
        let hwmon = kraken.hwmon.as_ref().unwrap();
        assert_eq!(hwmon.driver, hwmon::KRAKEN_DRIVER);
        assert_eq!(hwmon.curve_points.len(), 2);

        let rgb = record.device(RGB_CONTROLLER).unwrap();
        assert_eq!(rgb.interfaces.len(), 1);
        assert!(rgb.hwmon.is_none());
    }

    #[test]
    fn unknown_devices_are_reported_and_never_opened() {
        let fake = fixture("probe-rejects");
        let record = probe(&fake.root());

        assert_eq!(record.rejected.len(), 1);
        assert_eq!(record.rejected[0].id.to_string(), "046d:c52b");
        assert!(record.device(DeviceId::new(0x046d, 0xc52b)).is_none());
        assert!(record.supported().count() == 2);
    }

    #[test]
    fn readings_are_available_and_never_writable() {
        let fake = fixture("probe-readings");
        let record = probe(&fake.root());
        let kraken = record.device(KRAKEN_BASE).unwrap();

        for id in [
            CapabilityId::LiquidTemperature,
            CapabilityId::PumpSpeed,
            CapabilityId::FanSpeed,
        ] {
            let state = &kraken.capability(id).unwrap().state;
            assert!(state.is_readable(), "{id:?} should be readable");
            assert!(!state.is_writable(), "{id:?} must never be writable");
        }
    }

    #[test]
    fn control_capabilities_follow_filesystem_permissions() {
        if running_as_root() {
            return;
        }
        let fake = fixture("probe-permissions");
        let hwmon_path = fake.root_path().join("class/hwmon/hwmon4");

        let before = probe(&fake.root());
        let kraken = before.device(KRAKEN_BASE).unwrap();
        assert!(!kraken.can_write(CapabilityId::PumpDuty));
        assert!(!kraken.can_write(CapabilityId::PumpCurve));
        let reason = kraken
            .capability(CapabilityId::PumpDuty)
            .unwrap()
            .state
            .blocked_reason()
            .unwrap();
        assert!(reason.contains("Read-only"), "{reason}");

        let hwmon = std::fs::canonicalize(&hwmon_path).unwrap();
        fake.grant_write(&hwmon, "pwm1");
        for point in 1..=40 {
            fake.grant_write(&hwmon, &format!("temp1_auto_point{point}_pwm"));
        }

        let after = probe(&fake.root());
        let kraken = after.device(KRAKEN_BASE).unwrap();
        assert!(kraken.can_write(CapabilityId::PumpDuty));
        assert!(kraken.can_write(CapabilityId::PumpCurve));
        assert!(!kraken.can_write(CapabilityId::FanDuty));
        assert!(!kraken.can_write(CapabilityId::FanCurve));
    }

    #[test]
    fn missing_hwmon_marks_every_thermal_capability_unavailable() {
        let fake = FakeSysfs::new("probe-no-hwmon");
        fake.add_kraken();

        let record = probe(&fake.root());
        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert!(kraken.hwmon.is_none());

        let state = &kraken.capability(CapabilityId::PumpDuty).unwrap().state;
        assert!(matches!(state, CapabilityState::Unavailable { .. }));
        let reason = state.blocked_reason().unwrap();
        assert!(reason.contains("kraken2023"), "{reason}");
    }

    #[test]
    fn unvalidated_surfaces_name_the_story_that_unblocks_them() {
        let fake = fixture("probe-unvalidated");
        let record = probe(&fake.root());

        let lcd = &record
            .device(KRAKEN_BASE)
            .unwrap()
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state;
        assert!(lcd.blocked_reason().unwrap().contains(LCD_VALIDATION_STORY));

        let rgb = &record
            .device(RGB_CONTROLLER)
            .unwrap()
            .capability(CapabilityId::RgbFixedColor)
            .unwrap()
            .state;
        assert!(rgb.blocked_reason().unwrap().contains(RGB_VALIDATION_STORY));
        assert!(
            !record
                .device(RGB_CONTROLLER)
                .unwrap()
                .can_write(CapabilityId::RgbFixedColor)
        );
    }

    #[test]
    fn absent_attributes_stay_unknown_with_their_source() {
        let fake = FakeSysfs::new("probe-unknown");
        let device = fake.add_kraken();
        fake.remove_attribute(&device, "serial");
        fake.remove_attribute(&device, "bcdDevice");

        let record = probe(&fake.root());
        let kraken = record.device(KRAKEN_BASE).unwrap();
        assert!(!kraken.usb.serial.is_known());
        assert!(!kraken.usb.firmware.is_known());

        let json = serde_json::to_string(&kraken.usb).unwrap();
        assert!(json.contains("\"state\":\"unknown\""), "{json}");
    }

    #[test]
    fn an_empty_machine_produces_an_empty_but_valid_record() {
        let fake = FakeSysfs::new("probe-empty");
        let record = probe(&fake.root());

        assert_eq!(record.schema_version, CAPABILITY_SCHEMA_VERSION);
        assert!(record.devices.is_empty());
        assert!(record.rejected.is_empty());
    }
}
