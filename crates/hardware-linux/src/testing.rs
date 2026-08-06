// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

// This module only ever builds fixtures for tests. Failing to build one is a
// broken test, not a runtime condition, so it panics loudly and immediately.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A fake sysfs tree mirroring the layout of the development machine.
//!
//! The probe reads real files through real permission checks, so a fixture is
//! the only way to prove its behavior on absent attributes, unbound drivers
//! and read-only nodes without owning four different devices.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::sysfs::SysfsRoot;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Directory names used by the fixture, matching the development machine.
const KRAKEN_BUS_ID: &str = "1-9";
const RGB_BUS_ID: &str = "1-12";
const UNRELATED_BUS_ID: &str = "2-1";
const KRAKEN_HID_ID: &str = "0003:1E71:300E.000B";

/// Serial numbers of the fixture devices.
///
/// Deliberately synthetic. A serial identifies one physical unit, and the
/// product treats it as a secret everywhere else, so publishing a real one in
/// a test fixture would undo the redaction the fixture exists to prove.
pub const KRAKEN_FIXTURE_SERIAL: &str = "F1XTURE0000000000000000000KRAKEN";
pub const RGB_FIXTURE_SERIAL: &str = "F1XTURE0000000000000000000000RGB";

/// A temporary sysfs tree removed when the value is dropped.
pub struct FakeSysfs {
    base: PathBuf,
}

impl FakeSysfs {
    pub fn new(name: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "nzxt-fake-sysfs-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sys/bus/usb/devices")).unwrap();
        fs::create_dir_all(base.join("sys/class/hwmon")).unwrap();
        Self { base }
    }

    pub fn root(&self) -> SysfsRoot {
        SysfsRoot::new(self.base.join("sys"))
    }

    /// Path of the fake sysfs root, for `NZXT_SYSFS_ROOT`.
    pub fn root_path(&self) -> PathBuf {
        self.base.join("sys")
    }

    fn devices(&self) -> PathBuf {
        self.base.join("sys/bus/usb/devices")
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{contents}\n")).unwrap();
    }

    fn write_mode(path: &Path, contents: &str, mode: u32) {
        Self::write(path, contents);
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A directory with no identifiers, like a root hub entry.
    pub fn add_bare_directory(&self, relative: &str) -> PathBuf {
        let path = self.base.join("sys").join(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn add_interface(&self, device: &Path, number: u8, class: u8, driver: Option<&str>) {
        let name = device.file_name().unwrap().to_string_lossy().into_owned();
        let interface = device.join(format!("{name}:1.{number}"));
        fs::create_dir_all(&interface).unwrap();
        Self::write(
            &interface.join("bInterfaceNumber"),
            &format!("{number:02x}"),
        );
        Self::write(&interface.join("bInterfaceClass"), &format!("{class:02x}"));
        Self::write(&interface.join("bInterfaceSubClass"), "00");
        Self::write(&interface.join("bInterfaceProtocol"), "00");
        if let Some(driver) = driver {
            let driver_dir = self.base.join("sys/bus/usb/drivers").join(driver);
            fs::create_dir_all(&driver_dir).unwrap();
            symlink(&driver_dir, interface.join("driver")).unwrap();
        }
    }

    /// The Kraken Base: a vendor interface with no driver plus a HID interface
    /// bound to `usbhid`.
    pub fn add_kraken(&self) -> PathBuf {
        let device = self.devices().join(KRAKEN_BUS_ID);
        fs::create_dir_all(&device).unwrap();
        Self::write(&device.join("idVendor"), "1e71");
        Self::write(&device.join("idProduct"), "300e");
        Self::write(&device.join("manufacturer"), "NZXT Inc.");
        Self::write(&device.join("product"), "NZXT Kraken Base");
        Self::write(&device.join("serial"), KRAKEN_FIXTURE_SERIAL);
        Self::write(&device.join("bcdDevice"), "0200");
        self.add_interface(&device, 0, 0xff, None);
        self.add_interface(&device, 1, 0x03, Some("usbhid"));
        device
    }

    pub fn add_rgb_controller(&self) -> PathBuf {
        let device = self.devices().join(RGB_BUS_ID);
        fs::create_dir_all(&device).unwrap();
        Self::write(&device.join("idVendor"), "1e71");
        Self::write(&device.join("idProduct"), "2021");
        Self::write(&device.join("manufacturer"), "NZXT, Inc.");
        Self::write(&device.join("product"), "NZXT RGB Controller");
        Self::write(&device.join("serial"), RGB_FIXTURE_SERIAL);
        Self::write(&device.join("bcdDevice"), "0105");
        self.add_interface(&device, 0, 0x03, Some("usbhid"));
        device
    }

    /// A device outside the allowlist, which the probe must leave alone.
    pub fn add_unrelated_device(&self) -> PathBuf {
        let device = self.devices().join(UNRELATED_BUS_ID);
        fs::create_dir_all(&device).unwrap();
        Self::write(&device.join("idVendor"), "046d");
        Self::write(&device.join("idProduct"), "c52b");
        Self::write(&device.join("product"), "USB Receiver");
        self.add_interface(&device, 0, 0x03, Some("usbhid"));
        device
    }

    /// The `kraken2023` hwmon instance, hanging off the Kraken HID interface.
    ///
    /// Permissions mirror an unprivileged session: readings are readable, the
    /// PWM nodes are not writable and the curve points are neither.
    pub fn add_kraken_hwmon(&self) -> PathBuf {
        let hid = self
            .devices()
            .join(KRAKEN_BUS_ID)
            .join(format!("{KRAKEN_BUS_ID}:1.1"))
            .join(KRAKEN_HID_ID);
        let hwmon = hid.join("hwmon/hwmon4");
        fs::create_dir_all(&hwmon).unwrap();

        Self::write_mode(&hwmon.join("name"), "kraken2023", 0o444);
        Self::write_mode(&hwmon.join("temp1_input"), "27900", 0o444);
        Self::write_mode(&hwmon.join("temp1_label"), "Coolant temp", 0o444);
        Self::write_mode(&hwmon.join("fan1_input"), "2970", 0o444);
        Self::write_mode(&hwmon.join("fan1_label"), "Pump speed", 0o444);
        Self::write_mode(&hwmon.join("fan2_input"), "1764", 0o444);
        Self::write_mode(&hwmon.join("fan2_label"), "Fan speed", 0o444);
        for channel in 1..=2 {
            Self::write_mode(&hwmon.join(format!("pwm{channel}")), "255", 0o444);
            Self::write_mode(&hwmon.join(format!("pwm{channel}_enable")), "0", 0o444);
            for point in 1..=40 {
                // Write-only on the real driver (`--w-------`), so an
                // unprivileged user has neither read nor write until a udev
                // rule grants one.
                Self::write_mode(
                    &hwmon.join(format!("temp{channel}_auto_point{point}_pwm")),
                    "0",
                    0o000,
                );
            }
        }

        symlink(&hid, hwmon.join("device")).unwrap();
        symlink(&hwmon, self.base.join("sys/class/hwmon/hwmon4")).unwrap();
        hwmon
    }

    /// A hwmon instance from another driver, which must not be attributed to
    /// an NZXT device.
    ///
    /// Deliberately not a CPU driver: the CPU temperature collector recognizes
    /// a fixed set of driver names, and a fixture that accidentally matched
    /// one would make an "no CPU sensor here" test pass for the wrong reason.
    pub fn add_unrelated_hwmon(&self) -> PathBuf {
        let hwmon = self.base.join("sys/devices/platform/nvme0/hwmon/hwmon5");
        fs::create_dir_all(&hwmon).unwrap();
        Self::write_mode(&hwmon.join("name"), "nvme", 0o444);
        Self::write_mode(&hwmon.join("temp1_input"), "42000", 0o444);
        symlink(
            hwmon.parent().unwrap().parent().unwrap(),
            hwmon.join("device"),
        )
        .unwrap();
        symlink(&hwmon, self.base.join("sys/class/hwmon/hwmon5")).unwrap();
        hwmon
    }

    /// The `k10temp` instance the CPU temperature collector reads.
    pub fn add_cpu_hwmon(&self) -> PathBuf {
        let hwmon = self.base.join("sys/devices/platform/k10temp/hwmon/hwmon6");
        fs::create_dir_all(&hwmon).unwrap();
        Self::write_mode(&hwmon.join("name"), "k10temp", 0o444);
        Self::write_mode(&hwmon.join("temp1_label"), "Tctl", 0o444);
        Self::write_mode(&hwmon.join("temp1_input"), "46750", 0o444);
        symlink(
            hwmon.parent().unwrap().parent().unwrap(),
            hwmon.join("device"),
        )
        .unwrap();
        let link = self.base.join("sys/class/hwmon/hwmon6");
        let _ = fs::remove_file(&link);
        symlink(&hwmon, link).unwrap();
        hwmon
    }

    /// A `/proc` tree with the two files the system collectors read.
    ///
    /// Counters start at zero so a test controls every delta explicitly.
    pub fn add_proc(&self) -> PathBuf {
        let proc_root = self.base.join("proc");
        fs::create_dir_all(&proc_root).unwrap();
        self.set_proc_stat(&proc_root, 0, 0);
        self.set_proc_meminfo(
            &proc_root,
            "MemTotal:       31979068 kB\nMemFree:         1110612 kB\nMemAvailable:   21489412 kB\n",
        );
        proc_root
    }

    /// Rewrite `/proc/stat` with cumulative busy and idle tick counters.
    pub fn set_proc_stat(&self, proc_root: &Path, busy: u64, idle: u64) {
        fs::write(
            proc_root.join("stat"),
            format!("cpu  {busy} 0 0 {idle} 0 0 0 0 0 0\ncpu0 {busy} 0 0 {idle} 0 0 0 0 0 0\n"),
        )
        .unwrap();
    }

    pub fn set_proc_meminfo(&self, proc_root: &Path, contents: &str) {
        fs::write(proc_root.join("meminfo"), contents).unwrap();
    }

    /// Grant write permission on a hwmon attribute, simulating a udev rule.
    pub fn grant_write(&self, hwmon: &Path, attribute: &str) {
        let path = hwmon.join(attribute);
        let mode = if attribute.contains("auto_point") {
            0o200
        } else {
            0o644
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Remove an attribute to simulate a device that does not expose it.
    pub fn remove_attribute(&self, device: &Path, attribute: &str) {
        let _ = fs::remove_file(device.join(attribute));
    }

    /// Rewrite a reading the driver publishes read-only.
    ///
    /// The fixture mirrors the kernel's permissions, so a test that needs a
    /// stalled tachometer or a hot coolant lifts the mode for the write and
    /// puts it back. Production code still cannot write these files.
    pub fn set_reading(&self, hwmon: &Path, attribute: &str, value: &str) {
        let path = hwmon.join(attribute);
        let previous = fs::metadata(&path)
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0o444);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, format!("{value}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(previous)).unwrap();
    }

    /// Read an attribute the fixture deliberately keeps unreadable.
    ///
    /// The curve points mirror the kernel's `--w-------` mode, so production
    /// code genuinely cannot read them back. A test still has to check what
    /// was written, so it lifts the mode for the read and puts it back.
    pub fn written_value(&self, hwmon: &Path, attribute: &str) -> Option<String> {
        let path = hwmon.join(attribute);
        let previous = fs::metadata(&path).ok()?.permissions().mode() & 0o777;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).ok()?;
        let contents = fs::read_to_string(&path).ok();
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(previous));
        contents.map(|text| text.trim().to_string())
    }

    /// The forty curve points currently stored for one kernel channel.
    pub fn written_curve(&self, hwmon: &Path, channel_index: u8) -> Vec<u8> {
        (1..=40)
            .filter_map(|point| {
                self.written_value(hwmon, &format!("temp{channel_index}_auto_point{point}_pwm"))
            })
            .filter_map(|value| value.parse().ok())
            .collect()
    }
}

impl Drop for FakeSysfs {
    fn drop(&mut self) {
        // Curve points are mode 000; restore access so the tree can be removed.
        if let Ok(entries) = walk(&self.base) {
            for path in entries {
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));
            }
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn walk(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            // `file_type` does not follow symlinks, so the hwmon links in the
            // fixture cannot turn this walk into a cycle.
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }
    Ok(files)
}

/// True when the process can bypass permission bits, which makes any
/// `access(2)` assertion meaningless.
pub fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}
