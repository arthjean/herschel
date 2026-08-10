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
const RGB_HID_ID: &str = "0003:1E71:2021.000D";

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
            "herschel-fake-sysfs-{name}-{}-{unique}",
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

    /// Path of the fake sysfs root, for `HERSCHEL_SYSFS_ROOT`.
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

    fn add_interface(&self, device: &Path, number: u8, class: u8, driver: Option<&str>) -> PathBuf {
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
        interface
    }

    /// One endpoint directory, in the layout and formats sysfs uses.
    pub fn add_endpoint(
        &self,
        interface: &Path,
        address: u8,
        direction: &str,
        transfer: &str,
        max_packet: u16,
    ) {
        let endpoint = interface.join(format!("ep_{address:02x}"));
        fs::create_dir_all(&endpoint).unwrap();
        Self::write(
            &endpoint.join("bEndpointAddress"),
            &format!("{address:02x}"),
        );
        Self::write(&endpoint.join("direction"), direction);
        Self::write(&endpoint.join("type"), transfer);
        Self::write(
            &endpoint.join("wMaxPacketSize"),
            &format!("{max_packet:04x}"),
        );
        Self::write(&endpoint.join("bInterval"), "01");
    }

    /// The Kraken Base, exactly as the development machine publishes it.
    ///
    /// Interface 0 is vendor class with no driver and one bulk OUT endpoint of
    /// 512 bytes, which is the LCD framebuffer path. Interface 1 is HID, bound
    /// to `usbhid`, carries the interrupt pair `kraken2023` drives and
    /// publishes a `hidraw` node beside it: the kernel driver starts with
    /// `HID_CONNECT_HIDRAW`, so user space and the thermal driver share that
    /// interface by design.
    pub fn add_kraken(&self) -> PathBuf {
        let device = self.devices().join(KRAKEN_BUS_ID);
        fs::create_dir_all(&device).unwrap();
        Self::write(&device.join("idVendor"), "1e71");
        Self::write(&device.join("idProduct"), "300e");
        Self::write(&device.join("manufacturer"), "NZXT Inc.");
        Self::write(&device.join("product"), "NZXT Kraken Base");
        Self::write(&device.join("serial"), KRAKEN_FIXTURE_SERIAL);
        Self::write(&device.join("bcdDevice"), "0200");
        Self::write(&device.join("busnum"), "1");
        Self::write(&device.join("devnum"), "4");
        let vendor = self.add_interface(&device, 0, 0xff, None);
        self.add_endpoint(&vendor, 0x02, "out", "Bulk", 512);
        let hid = self.add_interface(&device, 1, 0x03, Some("usbhid"));
        self.add_endpoint(&hid, 0x01, "out", "Interrupt", 64);
        self.add_endpoint(&hid, 0x81, "in", "Interrupt", 64);
        fs::create_dir_all(hid.join(KRAKEN_HID_ID).join("hidraw/hidraw10")).unwrap();
        device
    }

    /// Remove the Kraken's `hidraw` node, as if the kernel had not created one.
    pub fn remove_kraken_hidraw(&self) {
        let interface = self
            .devices()
            .join(KRAKEN_BUS_ID)
            .join(format!("{KRAKEN_BUS_ID}:1.1"));
        let _ = fs::remove_dir_all(interface.join(KRAKEN_HID_ID).join("hidraw"));
    }

    /// The RGB Controller: one HID interface, an interrupt pair and a `hidraw`
    /// node, exactly as the development machine exposes it.
    ///
    /// The node itself is a plain file under the fixture: the fixture cannot
    /// create a character device, and every caller resolves the path before it
    /// opens anything, so the path is what the tests need to be real.
    pub fn add_rgb_controller(&self) -> PathBuf {
        let device = self.devices().join(RGB_BUS_ID);
        fs::create_dir_all(&device).unwrap();
        Self::write(&device.join("idVendor"), "1e71");
        Self::write(&device.join("idProduct"), "2021");
        Self::write(&device.join("manufacturer"), "NZXT, Inc.");
        Self::write(&device.join("product"), "NZXT RGB Controller");
        Self::write(&device.join("serial"), RGB_FIXTURE_SERIAL);
        Self::write(&device.join("bcdDevice"), "0105");
        let interface = self.add_interface(&device, 0, 0x03, Some("usbhid"));
        self.add_endpoint(&interface, 0x02, "out", "Interrupt", 64);
        self.add_endpoint(&interface, 0x81, "in", "Interrupt", 64);
        fs::create_dir_all(interface.join(RGB_HID_ID).join("hidraw/hidraw12")).unwrap();
        device
    }

    /// Remove the `hidraw` node, as if the kernel had not created one.
    pub fn remove_rgb_hidraw(&self) {
        let interface = self
            .devices()
            .join(RGB_BUS_ID)
            .join(format!("{RGB_BUS_ID}:1.0"));
        let _ = fs::remove_dir_all(interface.join(RGB_HID_ID).join("hidraw"));
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

/// A controller that answers the reports the real one answers.
///
/// It exists so the command path can be proven end to end without a physical
/// device: the daemon, its integration tests and the write probe all drive the
/// same encoding, ownership and cadence code, and this stands in for the only
/// part a test cannot own.
#[derive(Debug, Default)]
pub struct FakeController {
    firmware: String,
    /// Accessory identifiers per channel, in channel order.
    channels: Vec<Vec<u8>>,
    pending: std::collections::VecDeque<[u8; crate::rgb::REPORT_BYTES]>,
    /// Every command report the controller received, in order.
    pub commands: Vec<[u8; crate::rgb::REPORT_BYTES]>,
    /// When set, every write fails with this error instead of landing.
    pub write_failure: Option<crate::rgb::RgbError>,
    /// When true, queries are accepted but never answered.
    pub silent: bool,
    /// When true, the firmware answers but the topology never does.
    ///
    /// That is not hypothetical: the topology answer takes most of a second on
    /// the owned controller while the firmware answer takes two milliseconds,
    /// so a run that gives up too early sees exactly this.
    pub withhold_topology: bool,
}

impl FakeController {
    /// A controller reporting `firmware` and one accessory on each channel.
    pub fn new(firmware: &str, channel_count: usize) -> Self {
        Self {
            firmware: firmware.to_string(),
            channels: vec![vec![0x04]; channel_count],
            ..Self::default()
        }
    }

    /// A controller that answers nothing, like one that is wedged or gone.
    pub fn silent() -> Self {
        Self {
            silent: true,
            ..Self::new("0.0.0", 0)
        }
    }

    /// A controller that answers its firmware and nothing else.
    pub fn withholding_topology(mut self) -> Self {
        self.withhold_topology = true;
        self
    }

    /// Replace the accessories reported on one zero-based channel.
    pub fn with_accessories(mut self, channel: usize, accessories: Vec<u8>) -> Self {
        if let Some(slot) = self.channels.get_mut(channel) {
            *slot = accessories;
        }
        self
    }

    /// Color commands received, excluding the queries.
    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    fn firmware_answer(&self) -> [u8; crate::rgb::REPORT_BYTES] {
        let mut report = [0u8; crate::rgb::REPORT_BYTES];
        report[0] = 0x11;
        report[1] = 0x01;
        for (index, part) in self.firmware.split('.').take(3).enumerate() {
            report[0x11 + index] = part.parse().unwrap_or(0);
        }
        report
    }

    fn topology_answer(&self) -> [u8; crate::rgb::REPORT_BYTES] {
        let mut report = [0u8; crate::rgb::REPORT_BYTES];
        report[0] = 0x21;
        report[1] = 0x03;
        report[14] = self.channels.len() as u8;
        for (channel, accessories) in self.channels.iter().enumerate() {
            for (slot, id) in accessories
                .iter()
                .take(crate::rgb::MAX_ACCESSORIES_PER_CHANNEL)
                .enumerate()
            {
                let offset = 15 + channel * crate::rgb::MAX_ACCESSORIES_PER_CHANNEL + slot;
                if let Some(byte) = report.get_mut(offset) {
                    *byte = *id;
                }
            }
        }
        report
    }
}

impl crate::rgb::HidTransport for FakeController {
    fn write_report(
        &mut self,
        report: &[u8; crate::rgb::REPORT_BYTES],
    ) -> Result<(), crate::rgb::RgbError> {
        if let Some(failure) = &self.write_failure {
            return Err(failure.clone());
        }
        match [report[0], report[1]] {
            crate::rgb::packet::FIRMWARE_REQUEST if !self.silent => {
                let answer = self.firmware_answer();
                self.pending.push_back(answer);
            }
            crate::rgb::packet::LIGHTING_REQUEST if !self.silent && !self.withhold_topology => {
                let answer = self.topology_answer();
                self.pending.push_back(answer);
            }
            crate::rgb::packet::COLOR_COMMAND => self.commands.push(*report),
            _ => {}
        }
        Ok(())
    }

    fn read_report(
        &mut self,
        _timeout: std::time::Duration,
    ) -> Result<Option<[u8; crate::rgb::REPORT_BYTES]>, crate::rgb::RgbError> {
        Ok(self.pending.pop_front())
    }

    fn source(&self) -> String {
        "fake:hidraw".to_string()
    }
}

/// Every bulk transfer a fake device received, shared with the test.
///
/// Behind a mutex because the transport is boxed into the link under test, so
/// the assertions reach it through a handle rather than through the box.
#[derive(Default)]
pub struct BulkRecorder {
    transfers: std::sync::Mutex<Vec<(u8, Vec<u8>)>>,
    failure: std::sync::Mutex<Option<crate::usbfs::UsbfsError>>,
}

impl BulkRecorder {
    /// Payloads received, in order, without their endpoint.
    pub fn transfers(&self) -> Vec<Vec<u8>> {
        self.transfers
            .lock()
            .map(|t| t.iter().map(|(_, payload)| payload.clone()).collect())
            .unwrap_or_default()
    }

    /// Endpoint each transfer was addressed to, in the same order.
    pub fn endpoints(&self) -> Vec<u8> {
        self.transfers
            .lock()
            .map(|t| t.iter().map(|(endpoint, _)| *endpoint).collect())
            .unwrap_or_default()
    }

    /// Total bytes that reached the endpoint.
    pub fn bytes(&self) -> usize {
        self.transfers
            .lock()
            .map(|t| t.iter().map(|(_, payload)| payload.len()).sum())
            .unwrap_or(0)
    }

    /// Make every later transfer fail, as an unplugged device would.
    pub fn fail_with(&self, error: crate::usbfs::UsbfsError) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = Some(error);
        }
    }

    /// Let transfers succeed again, as a device coming back would.
    ///
    /// The counterpart of [`BulkRecorder::fail_with`]: a stream that stops on a
    /// failure is supposed to be recoverable, and a recorder that can only ever
    /// fail cannot exercise the recovery.
    pub fn recover(&self) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = None;
        }
    }
}

/// Every report a fake device received, shared with the test.
///
/// Mirrors [`BulkRecorder`] and exists for the same reason: the HID half is
/// boxed into the link under test, so a caller asking "did that report actually
/// go out?" has to hold the log rather than the device.
#[derive(Default)]
pub struct ReportRecorder {
    reports: std::sync::Mutex<Vec<[u8; crate::hid::REPORT_BYTES]>>,
}

impl ReportRecorder {
    /// Every report received, in order.
    pub fn reports(&self) -> Vec<[u8; crate::hid::REPORT_BYTES]> {
        self.reports.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// The ones carrying `identifier`, which is how a caller asks whether one
    /// particular command reached the device.
    pub fn matching(&self, identifier: [u8; 2]) -> Vec<[u8; crate::hid::REPORT_BYTES]> {
        self.reports()
            .into_iter()
            .filter(|report| report[0] == identifier[0] && report[1] == identifier[1])
            .collect()
    }
}

/// The [`crate::usbfs::BulkTransport`] half of a fake device.
struct RecordingBulk(std::sync::Arc<BulkRecorder>);

impl crate::usbfs::BulkTransport for RecordingBulk {
    fn write_bulk(&mut self, endpoint: u8, payload: &[u8]) -> Result<(), crate::usbfs::UsbfsError> {
        if let Some(failure) = self.0.failure.lock().ok().and_then(|f| f.clone()) {
            return Err(failure);
        }
        if let Ok(mut transfers) = self.0.transfers.lock() {
            transfers.push((endpoint, payload.to_vec()));
        }
        Ok(())
    }

    fn source(&self) -> String {
        "fake:usbfs interface 0".to_string()
    }
}

/// A Kraken that answers the reports the real one answers.
///
/// Built for the LCD path: it responds to the firmware request and to the
/// display report only a device carrying a panel responds to, and it records
/// every report it was sent. [`FakeKraken::without_panel`] is the same device
/// with no screen behind it, which is the case US-016 must keep read-only.
pub struct FakeKraken {
    firmware: String,
    /// `None` for a Kraken whose cap carries no display.
    panel: Option<herschel_core::capability::LcdDisplaySettings>,
    pending: std::collections::VecDeque<[u8; crate::hid::REPORT_BYTES]>,
    /// Every report the device received, in order.
    reports: std::sync::Arc<ReportRecorder>,
    /// When set, every write fails with this error instead of landing.
    pub write_failure: Option<crate::hid::HidError>,
    bulk: std::sync::Arc<BulkRecorder>,
}

impl FakeKraken {
    /// A Kraken reporting `firmware`, with a panel at 60% and no rotation.
    pub fn new(firmware: &str) -> Self {
        Self {
            firmware: firmware.to_string(),
            panel: Some(herschel_core::capability::LcdDisplaySettings {
                brightness_percent: 60,
                quarter_turns: 0,
            }),
            pending: std::collections::VecDeque::new(),
            reports: std::sync::Arc::new(ReportRecorder::default()),
            write_failure: None,
            bulk: std::sync::Arc::new(BulkRecorder::default()),
        }
    }

    /// The same device with no display behind its cap.
    ///
    /// It answers its firmware, like every Kraken, and stays silent on the
    /// display report. That is the one observation separating a unit with a
    /// screen from a unit without one.
    pub fn without_panel(firmware: &str) -> Self {
        Self {
            panel: None,
            ..Self::new(firmware)
        }
    }

    /// The panel's reported settings, for a fixture that needs them changed.
    pub fn with_display(mut self, brightness_percent: u8, quarter_turns: u8) -> Self {
        self.panel = Some(herschel_core::capability::LcdDisplaySettings {
            brightness_percent,
            quarter_turns,
        });
        self
    }

    /// Handle on the bulk endpoint, taken before the device is boxed away.
    pub fn bulk_recorder(&self) -> std::sync::Arc<BulkRecorder> {
        std::sync::Arc::clone(&self.bulk)
    }

    /// Handle on the reports written, taken before the device is boxed away.
    pub fn report_recorder(&self) -> std::sync::Arc<ReportRecorder> {
        std::sync::Arc::clone(&self.reports)
    }

    /// Consume the device into the link the LCD code drives.
    pub fn link(self) -> crate::lcd::LcdLink {
        let bulk = RecordingBulk(std::sync::Arc::clone(&self.bulk));
        crate::lcd::LcdLink::new(Box::new(self), Box::new(bulk))
    }

    /// Reports received whose identifier matches `identifier`.
    pub fn reports_matching(&self, identifier: [u8; 2]) -> Vec<[u8; crate::hid::REPORT_BYTES]> {
        self.reports.matching(identifier)
    }

    fn firmware_answer(&self) -> [u8; crate::hid::REPORT_BYTES] {
        let mut report = [0u8; crate::hid::REPORT_BYTES];
        report[0] = 0x11;
        report[1] = 0x01;
        for (index, part) in self.firmware.split('.').take(3).enumerate() {
            report[0x11 + index] = part.parse().unwrap_or(0);
        }
        report
    }

    fn display_answer(
        &self,
        settings: herschel_core::capability::LcdDisplaySettings,
    ) -> [u8; crate::hid::REPORT_BYTES] {
        let mut report = [0u8; crate::hid::REPORT_BYTES];
        report[0] = 0x31;
        report[1] = 0x01;
        report[0x18] = settings.brightness_percent;
        report[0x1a] = settings.quarter_turns;
        report
    }
}

impl crate::hid::HidTransport for FakeKraken {
    fn write_report(
        &mut self,
        report: &[u8; crate::hid::REPORT_BYTES],
    ) -> Result<(), crate::hid::HidError> {
        if let Some(failure) = &self.write_failure {
            return Err(failure.clone());
        }
        if let Ok(mut reports) = self.reports.reports.lock() {
            reports.push(*report);
        }
        match [report[0], report[1]] {
            crate::hid::FIRMWARE_REQUEST => {
                let answer = self.firmware_answer();
                self.pending.push_back(answer);
            }
            crate::lcd::packet::DISPLAY_INFO_REQUEST => {
                if let Some(settings) = self.panel {
                    let answer = self.display_answer(settings);
                    self.pending.push_back(answer);
                }
            }
            // A panel acknowledges both transfer commands, which is what the
            // frame path waits for before it puts pixels on the endpoint.
            [0x36, sub] if self.panel.is_some() => {
                let mut answer = [0u8; crate::hid::REPORT_BYTES];
                answer[0] = crate::lcd::packet::TRANSFER_ANSWER;
                answer[1] = sub;
                self.pending.push_back(answer);
            }
            _ => {}
        }
        Ok(())
    }

    fn read_report(
        &mut self,
        _timeout: std::time::Duration,
    ) -> Result<Option<[u8; crate::hid::REPORT_BYTES]>, crate::hid::HidError> {
        Ok(self.pending.pop_front())
    }

    fn source(&self) -> String {
        "fake:hidraw10".to_string()
    }
}
