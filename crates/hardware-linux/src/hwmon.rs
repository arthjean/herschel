//! `hwmon` discovery for devices the kernel already drives.
//!
//! The thermal path stays on the bound driver. Nothing here detaches a kernel
//! driver or opens a device node directly.

use std::path::{Path, PathBuf};

use nzxt_core::DeviceId;
use nzxt_core::capability::{CurveChannel, Evidenced, HwmonAttribute, HwmonCapabilities};
use nzxt_core::profile::CURVE_POINT_COUNT;

use crate::sysfs::{SysfsRoot, is_readable, is_writable, read_attribute, sorted_entries};
use crate::usb;

/// Driver name the Kraken Base binds to on Linux 6.9 and later.
pub const KRAKEN_DRIVER: &str = "kraken2023";

/// A `hwmon` instance and the USB device it belongs to.
#[derive(Debug, Clone)]
pub struct HwmonInstance {
    pub device: DeviceId,
    pub capabilities: HwmonCapabilities,
}

/// Discover every `hwmon` instance that resolves to a USB device.
///
/// Instances belonging to a CPU, GPU or NVMe sensor resolve to no USB device
/// and are skipped rather than misattributed.
pub fn discover(root: &SysfsRoot) -> Vec<HwmonInstance> {
    let mut found = Vec::new();
    for entry in sorted_entries(&root.hwmon()) {
        let Some(driver) = read_attribute(&entry.join("name")) else {
            continue;
        };
        let Some((device, _)) = usb::owning_device(&entry.join("device")) else {
            continue;
        };
        found.push(HwmonInstance {
            device,
            capabilities: read_capabilities(&entry, driver),
        });
    }
    found
}

/// Read every attribute of one `hwmon` directory.
fn read_capabilities(path: &Path, driver: String) -> HwmonCapabilities {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut attributes = Vec::new();

    for entry in sorted_entries(&resolved) {
        let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !entry.is_file() || name == "uevent" {
            continue;
        }
        // Curve points are summarised per channel instead of listed one by one.
        if name.contains("auto_point") {
            continue;
        }
        attributes.push(HwmonAttribute {
            name: name.to_string(),
            path: entry.display().to_string(),
            readable: is_readable(&entry),
            writable: is_writable(&entry),
            label: label_for(&resolved, name),
        });
    }

    HwmonCapabilities {
        curve_points: curve_channels(&resolved),
        driver,
        path: resolved.display().to_string(),
        attributes,
    }
}

/// The driver-published label of a reading, when it publishes one.
fn label_for(directory: &Path, attribute: &str) -> Evidenced<String> {
    let Some(base) = attribute.strip_suffix("_input") else {
        return Evidenced::unknown("attribute has no label", format!("{attribute} (no label)"));
    };
    let label_file = directory.join(format!("{base}_label"));
    match read_attribute(&label_file) {
        Some(label) => Evidenced::known(label, label_file.display().to_string()),
        None => Evidenced::unknown(
            "driver publishes no label for this reading",
            label_file.display().to_string(),
        ),
    }
}

/// Count the `tempN_auto_pointM_pwm` files exposed per temperature channel.
fn curve_channels(directory: &Path) -> Vec<CurveChannel> {
    let mut channels: Vec<CurveChannel> = Vec::new();
    for entry in sorted_entries(directory) {
        let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(index) = parse_curve_point(name) else {
            continue;
        };
        let writable = is_writable(&entry);
        match channels.iter_mut().find(|c| c.temp_index == index) {
            Some(channel) => {
                channel.point_count += 1;
                channel.writable &= writable;
            }
            None => channels.push(CurveChannel {
                temp_index: index,
                point_count: 1,
                writable,
            }),
        }
    }
    channels.sort_by_key(|channel| channel.temp_index);
    channels
}

/// Extract the temperature channel index from `tempN_auto_pointM_pwm`.
fn parse_curve_point(name: &str) -> Option<u8> {
    let rest = name.strip_prefix("temp")?;
    let (index, rest) = rest.split_once("_auto_point")?;
    let point = rest.strip_suffix("_pwm")?;
    point.parse::<u16>().ok()?;
    index.parse().ok()
}

/// Whether a channel exposes the complete kernel curve ABI.
pub fn curve_is_complete(channel: &CurveChannel) -> bool {
    channel.point_count as usize == CURVE_POINT_COUNT
}

/// Path of a `hwmon` attribute, for a capability source.
pub fn attribute_path(capabilities: &HwmonCapabilities, name: &str) -> PathBuf {
    Path::new(&capabilities.path).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeSysfs, running_as_root};
    use nzxt_core::KRAKEN_BASE;

    #[test]
    fn discovery_attributes_hwmon_to_the_owning_usb_device() {
        let fake = FakeSysfs::new("hwmon-discover");
        fake.add_kraken();
        fake.add_kraken_hwmon();
        fake.add_unrelated_hwmon();

        let found = discover(&fake.root());
        assert_eq!(found.len(), 1, "only the USB-backed instance resolves");
        assert_eq!(found[0].device, KRAKEN_BASE);
        assert_eq!(found[0].capabilities.driver, KRAKEN_DRIVER);
    }

    #[test]
    fn readings_carry_the_driver_label() {
        let fake = FakeSysfs::new("hwmon-labels");
        fake.add_kraken();
        fake.add_kraken_hwmon();

        let found = discover(&fake.root());
        let capabilities = &found[0].capabilities;
        assert_eq!(
            capabilities
                .attribute("temp1_input")
                .unwrap()
                .label
                .value()
                .map(String::as_str),
            Some("Coolant temp")
        );
        assert_eq!(
            capabilities
                .attribute("fan1_input")
                .unwrap()
                .label
                .value()
                .map(String::as_str),
            Some("Pump speed")
        );
        assert_eq!(
            capabilities
                .attribute("fan2_input")
                .unwrap()
                .label
                .value()
                .map(String::as_str),
            Some("Fan speed")
        );
    }

    #[test]
    fn both_curve_channels_expose_forty_points() {
        let fake = FakeSysfs::new("hwmon-curves");
        fake.add_kraken();
        fake.add_kraken_hwmon();

        let found = discover(&fake.root());
        let channels = &found[0].capabilities.curve_points;
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].temp_index, 1);
        assert_eq!(channels[1].temp_index, 2);
        assert!(channels.iter().all(curve_is_complete));
    }

    #[test]
    fn permissions_are_reported_from_the_filesystem() {
        if running_as_root() {
            return; // access(2) ignores permission bits for root.
        }
        let fake = FakeSysfs::new("hwmon-perms");
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();

        let before = discover(&fake.root());
        let pwm1 = before[0].capabilities.attribute("pwm1").unwrap();
        assert!(pwm1.readable);
        assert!(!pwm1.writable, "no udev rule means no write access");
        assert!(!before[0].capabilities.curve_points[0].writable);

        fake.grant_write(&hwmon, "pwm1");
        for point in 1..=40 {
            fake.grant_write(&hwmon, &format!("temp1_auto_point{point}_pwm"));
        }

        let after = discover(&fake.root());
        assert!(after[0].capabilities.attribute("pwm1").unwrap().writable);
        assert!(after[0].capabilities.curve_points[0].writable);
        assert!(!after[0].capabilities.curve_points[1].writable);
    }

    #[test]
    fn curve_point_names_are_parsed_strictly() {
        assert_eq!(parse_curve_point("temp1_auto_point40_pwm"), Some(1));
        assert_eq!(parse_curve_point("temp2_auto_point1_pwm"), Some(2));
        assert_eq!(parse_curve_point("temp1_input"), None);
        assert_eq!(parse_curve_point("pwm1"), None);
        assert_eq!(parse_curve_point("temp1_auto_pointX_pwm"), None);
    }
}
