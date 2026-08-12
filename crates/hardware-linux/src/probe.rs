// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Assemble the versioned capability record.
//!
//! The probe is read-only by construction: it reads sysfs attributes and tests
//! permissions with `access(2)`. It never opens a writable descriptor, and a
//! device outside the allowlist is recorded and then left untouched.
//!
//! What this pass records and what a control is *allowed* to do with it are two
//! questions, and only the first is answered here. The second lives in
//! [`crate::gates`], because it needs answers this pass cannot obtain: it opens
//! no device node, so it can see that a controller exists without knowing what
//! it contains.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use kori_core::capability::{
    CAPABILITY_SCHEMA_VERSION, Capability, CapabilityId, CapabilityRecord, CapabilityState,
    DeviceRecord, Evidenced, HwmonCapabilities, ProbeContext, RejectedDevice, SupportState,
};
use kori_core::{DeviceId, KRAKEN_BASE, RGB_CONTROLLER, is_allowlisted};

use crate::gates::{lcd_capabilities, rgb_capabilities};
use crate::hwmon::{self, HwmonInstance, curve_is_complete};
use crate::sysfs::SysfsRoot;
use crate::usb;

/// The sysfs path of an allowlisted device that is actually present.
///
/// `None` covers both "not on this machine" and "present but not supported",
/// which are the same answer to every caller that is about to open a node: there
/// is nothing here to talk to. Written once here rather than at each call site,
/// because a site that forgot the support filter would happily resolve a node on
/// a device the record says nothing is proven about.
pub fn device_path(record: &CapabilityRecord, id: DeviceId) -> Option<PathBuf> {
    record
        .device(id)
        .filter(|device| device.is_supported())
        .map(|device| PathBuf::from(&device.usb.sysfs_path))
}

/// The `hidraw` node `usbhid` created for a present allowlisted device.
pub fn hidraw_node(record: &CapabilityRecord, id: DeviceId) -> Option<PathBuf> {
    usb::hidraw_node(&device_path(record, id)?)
}

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
            .map(hwmon::read_capabilities);

        devices.push(DeviceRecord {
            support: SupportState::Supported,
            capabilities: capabilities_for(discovered.id, hwmon.as_ref()),
            usb: usb::identity(discovered.id, &discovered.path),
            interfaces: usb::interfaces(&discovered.path),
            hwmon,
            // Filled by `attach_rgb_topology` / `attach_lcd_topology` once the
            // device has answered. The sysfs pass never opens a device node,
            // so it cannot know.
            rgb: None,
            lcd: None,
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
///
/// The surfaces that depend on a device answering (`rgb`, `lcd`) start from no
/// topology at all, because this pass opens no device node. The daemon folds in
/// what the device actually said with [`crate::gates::attach_rgb_topology`] and
/// [`crate::gates::attach_lcd_topology`].
fn capabilities_for(id: DeviceId, hwmon: Option<&HwmonCapabilities>) -> Vec<Capability> {
    match id {
        KRAKEN_BASE => kraken_capabilities(hwmon)
            .into_iter()
            .chain(lcd_capabilities(None))
            .collect(),
        RGB_CONTROLLER => rgb_capabilities(None),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeSysfs, running_as_root};

    #[test]
    fn probe_records_identity_interfaces_and_hwmon_for_both_devices() {
        let fake = FakeSysfs::development_machine("probe-identity");
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
        let fake = FakeSysfs::development_machine("probe-rejects");
        let record = probe(&fake.root());

        assert_eq!(record.rejected.len(), 1);
        assert_eq!(record.rejected[0].id.to_string(), "046d:c52b");
        assert!(record.device(DeviceId::new(0x046d, 0xc52b)).is_none());
        assert!(record.supported().count() == 2);
    }

    #[test]
    fn readings_are_available_and_never_writable() {
        let fake = FakeSysfs::development_machine("probe-readings");
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
        let fake = FakeSysfs::development_machine("probe-permissions");
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
    fn endpoints_are_recorded_for_the_interfaces_that_publish_them() {
        let fake = FakeSysfs::development_machine("probe-endpoints");
        let record = probe(&fake.root());

        let rgb = record.device(RGB_CONTROLLER).unwrap();
        let endpoints = &rgb.interfaces[0].endpoints;
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].address, 0x02);
        assert_eq!(endpoints[0].direction, "out");
        assert_eq!(endpoints[0].transfer, "Interrupt");
        assert_eq!(endpoints[0].max_packet_size, 64);
        assert_eq!(endpoints[1].address, 0x81);
        assert_eq!(endpoints[1].direction, "in");
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
