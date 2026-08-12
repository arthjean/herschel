// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! `hwmon` discovery for devices the kernel already drives.
//!
//! The thermal path stays on the bound driver. Nothing here detaches a kernel
//! driver or opens a device node directly.

use std::path::{Path, PathBuf};

use kori_core::capability::{CurveChannel, Evidenced, HwmonAttribute, HwmonCapabilities};
use kori_core::profile::{CURVE_POINT_COUNT, Channel, TemperatureCurve};
use kori_core::telemetry::{ChannelTelemetry, KrakenTelemetry, PwmMode, Reading, Unavailable};
use kori_core::{DeviceId, KRAKEN_BASE};

use crate::sysfs::{
    SysfsRoot, is_readable, is_writable, read_attribute, read_parsed, reading, sorted_entries,
};
use crate::usb;

/// Driver name the Kraken Base binds to on Linux 6.9 and later.
pub const KRAKEN_DRIVER: &str = "kraken2023";

/// A `hwmon` instance and the USB device it belongs to.
///
/// Identity only. Reading an instance's attributes costs one `access(2)` per
/// file and there are more than eighty of them per channel, so it is a separate
/// step: the daemon locates the Kraken's directory on every reconnect and needs
/// none of that, while the capability record needs all of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwmonInstance {
    pub device: DeviceId,
    pub driver: String,
    /// Canonical directory of the instance, not the `class/hwmon` symlink.
    pub path: PathBuf,
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
            driver,
            path: std::fs::canonicalize(&entry).unwrap_or(entry),
        });
    }
    found
}

/// Read every attribute of one `hwmon` instance.
pub fn read_capabilities(instance: &HwmonInstance) -> HwmonCapabilities {
    let resolved = instance.path.clone();
    let mut attributes = Vec::new();

    for entry in sorted_entries(&resolved) {
        let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !entry.is_file() || name == "uevent" {
            continue;
        }
        // Curve points are summarized per channel instead of listed one by one.
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
        driver: instance.driver.clone(),
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

/// Kernel index of a cooling channel.
///
/// `nzxt-kraken3` puts the pump on channel 1 and the fan on channel 2, for
/// `fanN_input`, `pwmN`, `pwmN_enable` and `tempN_auto_point*_pwm` alike.
pub fn channel_index(channel: Channel) -> u8 {
    match channel {
        Channel::Pump => 1,
        Channel::Fan => 2,
    }
}

/// Attribute names of one channel, in one place.
pub fn rpm_attribute(channel: Channel) -> String {
    format!("fan{}_input", channel_index(channel))
}

pub fn duty_attribute(channel: Channel) -> String {
    format!("pwm{}", channel_index(channel))
}

pub fn mode_attribute(channel: Channel) -> String {
    format!("pwm{}_enable", channel_index(channel))
}

/// The `tempN_auto_pointM_pwm` file for one curve point.
///
/// Points are one-based in the ABI while `index` is zero-based, matching
/// [`kori_core::profile::TemperatureCurve::points`].
pub fn curve_point_attribute(channel: Channel, index: usize) -> String {
    format!("temp{}_auto_point{}_pwm", channel_index(channel), index + 1)
}

/// Attribute holding the coolant temperature, in millidegrees Celsius.
pub const LIQUID_TEMPERATURE_ATTRIBUTE: &str = "temp1_input";

/// A bound `kraken2023` instance, opened for reading.
///
/// Holding the path rather than a descriptor is deliberate: sysfs attributes
/// are read per sample, and an open descriptor would keep a stale value alive
/// across a device reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KrakenHwmon {
    path: PathBuf,
}

impl KrakenHwmon {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Find the `kraken2023` instance belonging to the allowlisted Kraken.
    ///
    /// An instance from another driver, or one that resolves to a device
    /// outside the allowlist, is never returned. Nothing but the directory is
    /// read: this runs again on every reconnect, and the attribute sweep the
    /// capability record needs would be several hundred syscalls thrown away.
    pub fn locate(root: &SysfsRoot) -> Option<Self> {
        discover(root)
            .into_iter()
            .find(|instance| instance.device == KRAKEN_BASE && instance.driver == KRAKEN_DRIVER)
            .map(|instance| Self::new(instance.path))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn attribute(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Coolant temperature in degrees Celsius.
    pub fn liquid_temperature_c(&self) -> Reading<f32> {
        match read_parsed::<i32>(&self.attribute(LIQUID_TEMPERATURE_ATTRIBUTE)) {
            Ok(millidegrees) => Reading::valid(millidegrees as f32 / 1000.0),
            Err(cause) => Reading::unavailable(cause),
        }
    }

    pub fn rpm(&self, channel: Channel) -> Reading<u32> {
        reading(&self.attribute(&rpm_attribute(channel)))
    }

    pub fn duty(&self, channel: Channel) -> Reading<u8> {
        reading(&self.attribute(&duty_attribute(channel)))
    }

    pub fn mode(&self, channel: Channel) -> Reading<PwmMode> {
        let path = self.attribute(&mode_attribute(channel));
        match read_parsed::<u8>(&path) {
            Ok(value) => match PwmMode::from_kernel(value) {
                Some(mode) => Reading::valid(mode),
                None => Reading::unavailable(Unavailable::unparsable(format!(
                    "{} reports control mode {value}, which this build does not recognize.",
                    path.display()
                ))),
            },
            Err(cause) => Reading::unavailable(cause),
        }
    }

    /// Read the complete onboard curve of one channel.
    ///
    /// All forty points or none: a partially readable curve cannot be used as
    /// a last known-good snapshot, so it is reported as unavailable instead.
    pub fn curve(&self, channel: Channel) -> Result<TemperatureCurve, Unavailable> {
        let mut points = [0u8; CURVE_POINT_COUNT];
        for (index, point) in points.iter_mut().enumerate() {
            *point = read_parsed::<u8>(&self.attribute(&curve_point_attribute(channel, index)))?;
        }
        // The array is sized by the ABI's own constant, so the curve is built
        // through the door that cannot refuse rather than through the one that
        // would hand back a `Result` nothing here could act on.
        Ok(TemperatureCurve::from_points(points))
    }

    /// Everything one channel reports in a single pass.
    pub fn channel_telemetry(&self, channel: Channel) -> ChannelTelemetry {
        ChannelTelemetry {
            channel,
            rpm: self.rpm(channel),
            duty: self.duty(channel),
            mode: self.mode(channel),
        }
    }

    /// One complete Kraken sample.
    pub fn telemetry(&self, at_unix_ms: u64) -> KrakenTelemetry {
        KrakenTelemetry {
            at_unix_ms,
            present: true,
            liquid_temperature_c: self.liquid_temperature_c(),
            pump: self.channel_telemetry(Channel::Pump),
            fan: self.channel_telemetry(Channel::Fan),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeSysfs, running_as_root};
    use kori_core::KRAKEN_BASE;

    #[test]
    fn discovery_attributes_hwmon_to_the_owning_usb_device() {
        let fake = FakeSysfs::new("hwmon-discover");
        fake.add_kraken();
        fake.add_kraken_hwmon();
        fake.add_unrelated_hwmon();

        let found = discover(&fake.root());
        assert_eq!(found.len(), 1, "only the USB-backed instance resolves");
        assert_eq!(found[0].device, KRAKEN_BASE);
        assert_eq!(found[0].driver, KRAKEN_DRIVER);
    }

    #[test]
    fn locating_the_kraken_reads_no_attribute_of_any_instance() {
        // The daemon calls this on every reconnect. It resolves a directory and
        // must not pay for the attribute sweep the capability record needs, so
        // this asserts the shape rather than the cost: the located path is the
        // instance's own, reached without a `HwmonCapabilities` in sight.
        let fake = FakeSysfs::new("hwmon-locate");
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();
        fake.add_unrelated_hwmon();

        let located = KrakenHwmon::locate(&fake.root()).expect("the fixture exposes kraken2023");
        assert_eq!(
            located.path(),
            std::fs::canonicalize(&hwmon).unwrap().as_path()
        );

        // An instance from another driver on the same bus is never returned.
        let empty = FakeSysfs::new("hwmon-locate-empty");
        empty.add_unrelated_hwmon();
        assert_eq!(KrakenHwmon::locate(&empty.root()), None);
    }

    #[test]
    fn readings_carry_the_driver_label() {
        let fake = FakeSysfs::new("hwmon-labels");
        fake.add_kraken();
        fake.add_kraken_hwmon();

        let found = discover(&fake.root());
        let capabilities = &read_capabilities(&found[0]);
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
        let channels = &read_capabilities(&found[0]).curve_points;
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

        let before = read_capabilities(&discover(&fake.root())[0]);
        let pwm1 = before.attribute("pwm1").unwrap();
        assert!(pwm1.readable);
        assert!(!pwm1.writable, "no udev rule means no write access");
        assert!(!before.curve_points[0].writable);

        fake.grant_write(&hwmon, "pwm1");
        for point in 1..=40 {
            fake.grant_write(&hwmon, &format!("temp1_auto_point{point}_pwm"));
        }

        let after = read_capabilities(&discover(&fake.root())[0]);
        assert!(after.attribute("pwm1").unwrap().writable);
        assert!(after.curve_points[0].writable);
        assert!(!after.curve_points[1].writable);
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
