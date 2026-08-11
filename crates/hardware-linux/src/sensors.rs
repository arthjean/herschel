// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! CPU and memory metrics from the kernel's own interfaces.
//!
//! `/proc/stat`, `/proc/meminfo` and one `hwmon` temperature. No vendor daemon,
//! no helper process, no network. Every failure resolves to a typed
//! [`Unavailable`] naming the file that could not be used, so one dead sensor
//! never blanks the rest of the dashboard.

use std::path::{Path, PathBuf};

use kori_core::telemetry::{MemoryUsage, Reading, SystemTelemetry, Unavailable};

use crate::sysfs::{SysfsRoot, read_attribute, read_attribute_detailed, sorted_entries};

/// Relocates `/proc`, so the collectors can be tested against a fixture.
pub const PROC_ROOT_ENV: &str = "KORI_PROC_ROOT";

/// The `/proc` tree this machine resolves to.
///
/// Resolution lives here rather than inside the collector so a caller can pass
/// an explicit root instead of mutating a process-global variable, which two
/// tests running side by side cannot do safely.
pub fn proc_root_from_env() -> PathBuf {
    match std::env::var_os(PROC_ROOT_ENV) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from("/proc"),
    }
}

/// `hwmon` drivers this build reads a CPU package temperature from.
///
/// The certified platform is AMD, where `k10temp` publishes `Tctl` on
/// `temp1_input`. Another machine resolves to no driver and reports the CPU
/// temperature as unavailable rather than borrowing a reading from an
/// unrelated sensor.
pub const CPU_TEMPERATURE_DRIVERS: [&str; 1] = ["k10temp"];

/// The `/proc/stat` counters one sample is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

/// CPU and memory collectors, holding only what a delta needs.
#[derive(Debug, Clone)]
pub struct SystemSensors {
    proc_root: PathBuf,
    cpu_temperature: Option<PathBuf>,
    previous: Option<CpuTimes>,
}

impl SystemSensors {
    /// Resolve every source once, then sample from the resolved paths.
    ///
    /// The first CPU reading needs a baseline, so one is taken here: the
    /// collector's first published sample is a real interval rather than a
    /// figure computed against process start.
    pub fn open(sysfs: &SysfsRoot, proc_root: impl Into<PathBuf>) -> Self {
        let proc_root = proc_root.into();
        let mut sensors = Self {
            cpu_temperature: cpu_temperature_path(sysfs),
            previous: None,
            proc_root,
        };
        sensors.previous = read_cpu_times(&sensors.stat_path()).ok();
        sensors
    }

    fn stat_path(&self) -> PathBuf {
        self.proc_root.join("stat")
    }

    fn meminfo_path(&self) -> PathBuf {
        self.proc_root.join("meminfo")
    }

    /// Busy time since the previous call, as a percentage.
    pub fn cpu_load_percent(&mut self) -> Reading<f32> {
        let path = self.stat_path();
        let current = match read_cpu_times(&path) {
            Ok(times) => times,
            Err(cause) => return Reading::unavailable(cause),
        };

        let Some(previous) = self.previous.replace(current) else {
            return Reading::unavailable(Unavailable::unreadable(format!(
                "{} has not been sampled twice yet.",
                path.display()
            )));
        };

        let total = current.total.saturating_sub(previous.total);
        let idle = current.idle.saturating_sub(previous.idle);
        if total == 0 {
            // Two reads inside one clock tick. The previous baseline is kept so
            // the next interval is measured against real elapsed time.
            self.previous = Some(previous);
            return Reading::unavailable(Unavailable::unreadable(format!(
                "{} advanced by zero ticks between samples.",
                path.display()
            )));
        }

        let busy = total.saturating_sub(idle) as f32 / total as f32 * 100.0;
        Reading::percent(busy, path.display())
    }

    /// Package temperature in degrees Celsius.
    pub fn cpu_temperature_c(&self) -> Reading<f32> {
        let Some(path) = &self.cpu_temperature else {
            return Reading::unavailable(Unavailable::absent(format!(
                "No {} hwmon instance publishes a CPU temperature on this machine.",
                CPU_TEMPERATURE_DRIVERS.join(" or ")
            )));
        };
        match read_attribute_detailed(path).and_then(|text| {
            text.parse::<i32>()
                .map_err(|_| Unavailable::unparsable(format!("{} holds {text:?}.", path.display())))
        }) {
            Ok(millidegrees) => Reading::valid(millidegrees as f32 / 1000.0),
            Err(cause) => Reading::unavailable(cause),
        }
    }

    /// One complete CPU and memory sample.
    pub fn telemetry(&mut self, at_unix_ms: u64) -> SystemTelemetry {
        SystemTelemetry {
            at_unix_ms,
            cpu_load_percent: self.cpu_load_percent(),
            cpu_temperature_c: self.cpu_temperature_c(),
            memory: self.memory(),
        }
    }

    /// Physical memory in use and installed.
    pub fn memory(&self) -> Reading<MemoryUsage> {
        let path = self.meminfo_path();
        let text = match read_attribute_detailed(&path) {
            Ok(text) => text,
            Err(cause) => return Reading::unavailable(cause),
        };

        let total = meminfo_field(&text, "MemTotal");
        // `MemAvailable` is the kernel's own estimate of what a workload can
        // claim without swapping, which is the figure an operator recognizes.
        // `MemFree` would count reclaimable cache as used.
        let available = meminfo_field(&text, "MemAvailable");

        match (total, available) {
            (Some(total), Some(available)) if total > 0 => Reading::valid(MemoryUsage {
                used_bytes: total.saturating_sub(available),
                total_bytes: total,
            }),
            _ => Reading::unavailable(Unavailable::unparsable(format!(
                "{} does not publish both MemTotal and MemAvailable.",
                path.display()
            ))),
        }
    }
}

/// Read the aggregate `cpu` line of `/proc/stat`.
fn read_cpu_times(path: &Path) -> Result<CpuTimes, Unavailable> {
    let text = read_attribute_detailed(path)?;
    let line = text
        .lines()
        .find(|line| line.starts_with("cpu "))
        .ok_or_else(|| {
            Unavailable::unparsable(format!("{} has no aggregate cpu line.", path.display()))
        })?;

    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse().ok())
        .collect();
    // user, nice, system, idle, iowait: the first five are all that is needed,
    // and every later field still counts towards the total.
    if fields.len() < 5 {
        return Err(Unavailable::unparsable(format!(
            "{} publishes {} cpu fields, fewer than the five required.",
            path.display(),
            fields.len()
        )));
    }

    Ok(CpuTimes {
        // iowait is idle time: the processor is waiting, not working.
        idle: fields[3] + fields[4],
        total: fields.iter().sum(),
    })
}

/// One `/proc/meminfo` field, converted from kibibytes to bytes.
fn meminfo_field(text: &str, key: &str) -> Option<u64> {
    let line = text
        .lines()
        .find(|line| line.starts_with(key) && line[key.len()..].starts_with(':'))?;
    let value: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    value.checked_mul(1024)
}

/// The `temp1_input` of the first recognized CPU temperature driver.
fn cpu_temperature_path(sysfs: &SysfsRoot) -> Option<PathBuf> {
    for entry in sorted_entries(&sysfs.hwmon()) {
        let Some(name) = read_attribute(&entry.join("name")) else {
            continue;
        };
        if !CPU_TEMPERATURE_DRIVERS.contains(&name.as_str()) {
            continue;
        }
        let path = entry.join("temp1_input");
        if path.exists() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeSysfs;

    fn fixture(name: &str) -> (FakeSysfs, PathBuf) {
        let fake = FakeSysfs::new(name);
        let proc_root = fake.add_proc();
        (fake, proc_root)
    }

    #[test]
    fn cpu_load_is_the_busy_fraction_between_two_samples() {
        let (fake, proc_root) = fixture("sensors-cpu");
        fake.add_cpu_hwmon();
        let mut sensors = SystemSensors::open(&fake.root(), &proc_root);

        // Half the ticks busy, half idle.
        fake.set_proc_stat(&proc_root, 100, 100);
        let load = sensors.cpu_load_percent();
        assert!((load.copied().unwrap() - 50.0).abs() < 0.001, "{load:?}");

        // Fully busy over the next interval.
        fake.set_proc_stat(&proc_root, 300, 100);
        let load = sensors.cpu_load_percent();
        assert!((load.copied().unwrap() - 100.0).abs() < 0.001, "{load:?}");
    }

    #[test]
    fn two_reads_inside_one_tick_are_unavailable_rather_than_zero() {
        let (fake, proc_root) = fixture("sensors-cpu-idle");
        let mut sensors = SystemSensors::open(&fake.root(), &proc_root);
        fake.set_proc_stat(&proc_root, 10, 10);
        sensors.cpu_load_percent();

        let repeated = sensors.cpu_load_percent();
        assert!(!repeated.is_valid());
        assert!(repeated.cause().unwrap().detail().contains("zero ticks"));

        // The baseline survived, so the next real interval still measures.
        fake.set_proc_stat(&proc_root, 20, 10);
        assert!(sensors.cpu_load_percent().is_valid());
    }

    #[test]
    fn a_missing_proc_stat_is_unavailable_with_its_path() {
        let (fake, proc_root) = fixture("sensors-no-stat");
        let mut sensors = SystemSensors::open(&fake.root(), proc_root.join("absent"));
        let load = sensors.cpu_load_percent();
        assert!(!load.is_valid());
        assert!(matches!(
            load.cause(),
            Some(Unavailable::Absent { .. } | Unavailable::Unreadable { .. })
        ));
        assert!(load.cause().unwrap().detail().contains("stat"));
    }

    #[test]
    fn cpu_temperature_comes_from_the_recognized_driver_only() {
        let (fake, proc_root) = fixture("sensors-cpu-temp");
        // Only an unrelated driver is present.
        fake.add_unrelated_hwmon();
        let sensors = SystemSensors::open(&fake.root(), &proc_root);
        let reading = sensors.cpu_temperature_c();
        assert!(!reading.is_valid());
        assert!(reading.cause().unwrap().detail().contains("k10temp"));

        fake.add_cpu_hwmon();
        let sensors = SystemSensors::open(&fake.root(), &proc_root);
        assert_eq!(sensors.cpu_temperature_c().copied(), Some(46.75));
    }

    #[test]
    fn memory_reports_used_and_total_in_bytes() {
        let (fake, proc_root) = fixture("sensors-memory");
        let sensors = SystemSensors::open(&fake.root(), &proc_root);

        let usage = sensors.memory().copied().expect("meminfo is readable");
        assert_eq!(usage.total_bytes, 31_979_068 * 1024);
        assert_eq!(usage.used_bytes, (31_979_068 - 21_489_412) * 1024);
        let occupancy = usage.percent().expect("a real total is never zero");
        assert!(occupancy > 0.0 && occupancy < 100.0);
    }

    #[test]
    fn a_truncated_meminfo_is_unavailable_rather_than_half_a_figure() {
        let (fake, proc_root) = fixture("sensors-bad-memory");
        fake.set_proc_meminfo(&proc_root, "MemTotal:       31979068 kB\n");
        let sensors = SystemSensors::open(&fake.root(), &proc_root);

        let reading = sensors.memory();
        assert!(!reading.is_valid());
        assert!(reading.cause().unwrap().detail().contains("MemAvailable"));
    }

    #[test]
    fn meminfo_fields_are_matched_exactly_not_by_prefix() {
        let text = "MemTotal:       100 kB\nMemAvailable:   40 kB\nMemTotalHuge:   999 kB\n";
        assert_eq!(meminfo_field(text, "MemTotal"), Some(100 * 1024));
        assert_eq!(meminfo_field(text, "MemAvailable"), Some(40 * 1024));
        assert_eq!(meminfo_field(text, "SwapTotal"), None);
    }
}
