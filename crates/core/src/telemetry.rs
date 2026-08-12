// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The telemetry vocabulary shared by the sampler and the client.
//!
//! One rule governs everything here: a missing reading is never a zero. A
//! metric is either [`Reading::Valid`] with a value, or [`Reading::Unavailable`]
//! with a typed cause naming what was tried. A control surface that shows `0 C`
//! for an unreadable sensor is indistinguishable from a cooler that is actually
//! at zero, which is exactly the confusion this product exists to remove.
//!
//! This file is the vocabulary itself: what a sample is worth and what one
//! pass of every collector produced. Two things sit next to it, each in its own
//! module because each is a different job done on top of these types. `view`
//! is how a sample presents once it has aged, which is the reader's work
//! against the reader's clock. `alerts` is the state machine that decides
//! what an operator has to be told right now.
//!
//! The seam is visible in the derives: everything in this file crosses the
//! socket and derives `Serialize`, and nothing in `view` does.

use serde::{Deserialize, Serialize};

use crate::profile::Channel;

mod alerts;
mod view;

pub use alerts::{AlertTracker, LIQUID_CRITICAL_C, STALL_SAMPLE_COUNT, SafetyAlert};
pub use view::{
    DROP_AFTER_MS, HISTORY_WINDOW_MS, History, HistoryPoint, MetricView, STALE_AFTER_MS, Tracked,
    format_binary_bytes, format_temperature,
};

/// Interval between two samples, in milliseconds.
///
/// Kraken, CPU, GPU and RAM are sampled once per second.
pub const SAMPLE_INTERVAL_MS: u64 = 1_000;

/// Why a reading has no value.
///
/// Every variant names what was attempted, so a disabled control and a blank
/// readout can both explain themselves without inventing a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum Unavailable {
    /// The attribute or interface does not exist on this machine.
    Absent { detail: String },
    /// It exists but could not be read: I/O error, or permission.
    Unreadable { detail: String },
    /// It was read but its contents could not be interpreted.
    Unparsable { detail: String },
    /// The device it belongs to is not connected.
    NoDevice { detail: String },
}

impl Unavailable {
    pub fn absent(detail: impl Into<String>) -> Self {
        Self::Absent {
            detail: detail.into(),
        }
    }

    pub fn unreadable(detail: impl Into<String>) -> Self {
        Self::Unreadable {
            detail: detail.into(),
        }
    }

    pub fn unparsable(detail: impl Into<String>) -> Self {
        Self::Unparsable {
            detail: detail.into(),
        }
    }

    pub fn no_device(detail: impl Into<String>) -> Self {
        Self::NoDevice {
            detail: detail.into(),
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Absent { detail }
            | Self::Unreadable { detail }
            | Self::Unparsable { detail }
            | Self::NoDevice { detail } => detail,
        }
    }
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

/// One sampled metric.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reading", rename_all = "snake_case")]
pub enum Reading<T> {
    Valid { value: T },
    Unavailable { cause: Unavailable },
}

impl<T> Reading<T> {
    pub fn valid(value: T) -> Self {
        Self::Valid { value }
    }

    pub fn unavailable(cause: Unavailable) -> Self {
        Self::Unavailable { cause }
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Valid { value } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }

    /// The same reading with its value converted, keeping the failure intact.
    ///
    /// A gap survives as a gap. Three call sites wrote this match out by hand to
    /// widen a byte or a memory figure into the `f32` a chart plots, and a
    /// hand-written one is a place where an `Unavailable` can quietly become a
    /// zero, which is the one substitution this product refuses to make.
    pub fn map<U>(&self, convert: impl FnOnce(&T) -> U) -> Reading<U> {
        match self {
            Self::Valid { value } => Reading::valid(convert(value)),
            Self::Unavailable { cause } => Reading::unavailable(cause.clone()),
        }
    }

    pub fn cause(&self) -> Option<&Unavailable> {
        match self {
            Self::Valid { .. } => None,
            Self::Unavailable { cause } => Some(cause),
        }
    }

    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }
}

impl<T: Copy> Reading<T> {
    pub fn copied(&self) -> Option<T> {
        match self {
            Self::Valid { value } => Some(*value),
            Self::Unavailable { .. } => None,
        }
    }
}

impl Reading<f32> {
    /// A percentage reading, clamped into 0-100.
    ///
    /// Every collector that produces a percentage divides, and a division is
    /// where a non-finite value comes from. Deciding what to do with one here,
    /// once, is what keeps each collector from having to answer that question
    /// separately, and keeps the answer from being a number a real sensor could
    /// also have reported. `measured` names what was being read, and is only
    /// formatted when the value has to be refused.
    pub fn percent(value: f32, measured: impl std::fmt::Display) -> Self {
        match clamp_percent(value) {
            Some(percent) => Self::valid(percent),
            None => Self::unavailable(Unavailable::unparsable(format!(
                "{measured} did not resolve to a finite percentage."
            ))),
        }
    }
}

/// Control mode of one kernel PWM channel.
///
/// The values are the `pwm[1-2]_enable` ABI of `nzxt-kraken3`. Mode 0 is the
/// firmware failsafe, not "off": the kernel documents it as running the channel
/// at 100%.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PwmMode {
    /// `0`: the channel runs at 100%.
    FullSpeed,
    /// `1`: the channel follows the value written to `pwmN`.
    Fixed,
    /// `2`: the channel follows the onboard curve over coolant temperature.
    Curve,
}

impl PwmMode {
    pub fn from_kernel(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::FullSpeed),
            1 => Some(Self::Fixed),
            2 => Some(Self::Curve),
            _ => None,
        }
    }

    pub fn to_kernel(self) -> u8 {
        match self {
            Self::FullSpeed => 0,
            Self::Fixed => 1,
            Self::Curve => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FullSpeed => "100% failsafe",
            Self::Fixed => "Fixed",
            Self::Curve => "Curve",
        }
    }
}

/// Everything one cooling channel reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelTelemetry {
    pub channel: Channel,
    /// Tachometer reading.
    pub rpm: Reading<u32>,
    /// Current duty, 0-255 as the kernel reports it.
    pub duty: Reading<u8>,
    pub mode: Reading<PwmMode>,
}

impl ChannelTelemetry {
    /// A channel whose device is absent, with the same cause on every field.
    pub fn unavailable(channel: Channel, cause: Unavailable) -> Self {
        Self {
            channel,
            rpm: Reading::unavailable(cause.clone()),
            duty: Reading::unavailable(cause.clone()),
            mode: Reading::unavailable(cause),
        }
    }

    /// Duty as a percentage of full scale, when it is known.
    pub fn duty_percent(&self) -> Option<f32> {
        self.duty.copied().map(|duty| duty as f32 / 255.0 * 100.0)
    }
}

/// The Kraken's own readings.
///
/// Each section carries the time *it* was sampled rather than inheriting the
/// snapshot's. The collectors run independently, so one that wedges must be
/// visible as one aging section next to sections that are still current.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KrakenTelemetry {
    pub at_unix_ms: u64,
    /// False when no supported Kraken is bound to a `hwmon` instance.
    pub present: bool,
    pub liquid_temperature_c: Reading<f32>,
    pub pump: ChannelTelemetry,
    pub fan: ChannelTelemetry,
}

impl KrakenTelemetry {
    /// The reading set produced when no Kraken is available at all.
    pub fn absent(at_unix_ms: u64, cause: Unavailable) -> Self {
        Self {
            at_unix_ms,
            present: false,
            liquid_temperature_c: Reading::unavailable(cause.clone()),
            pump: ChannelTelemetry::unavailable(Channel::Pump, cause.clone()),
            fan: ChannelTelemetry::unavailable(Channel::Fan, cause),
        }
    }

    pub fn channel(&self, channel: Channel) -> &ChannelTelemetry {
        match channel {
            Channel::Pump => &self.pump,
            Channel::Fan => &self.fan,
        }
    }
}

/// Memory occupancy in bytes, so the client picks the unit it displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub used_bytes: u64,
    pub total_bytes: u64,
}

impl MemoryUsage {
    /// Occupancy as a percentage, clamped into 0-100.
    ///
    /// `None` when the collector reported no total at all. A machine with no
    /// memory does not exist, so a zero total means the figure was never read,
    /// and returning 0% for it would draw an empty bar next to a full one.
    pub fn percent(self) -> Option<f32> {
        if self.total_bytes == 0 {
            return None;
        }
        clamp_percent(self.used_bytes as f32 / self.total_bytes as f32 * 100.0)
    }
}

/// The GPU the collector could reach, and what it reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuTelemetry {
    pub at_unix_ms: u64,
    /// Interface the values came from, for example `NVML`.
    pub source: String,
    pub name: Reading<String>,
    pub load_percent: Reading<f32>,
    pub temperature_c: Reading<f32>,
}

/// Reported as the source when no GPU interface could be resolved.
pub const GPU_SOURCE_UNAVAILABLE: &str = "unavailable";

impl GpuTelemetry {
    /// The reading set produced when no GPU interface is usable.
    pub fn unavailable(at_unix_ms: u64, cause: Unavailable) -> Self {
        Self {
            at_unix_ms,
            source: GPU_SOURCE_UNAVAILABLE.to_string(),
            name: Reading::unavailable(cause.clone()),
            load_percent: Reading::unavailable(cause.clone()),
            temperature_c: Reading::unavailable(cause),
        }
    }
}

/// CPU and memory metrics, normalized.
///
/// The GPU is a separate section because it is a separate collector on a
/// separate vendor interface: a driver that stops answering must not take the
/// CPU and memory readings down with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemTelemetry {
    pub at_unix_ms: u64,
    pub cpu_load_percent: Reading<f32>,
    pub cpu_temperature_c: Reading<f32>,
    pub memory: Reading<MemoryUsage>,
}

impl SystemTelemetry {
    pub fn unavailable(at_unix_ms: u64, cause: Unavailable) -> Self {
        Self {
            at_unix_ms,
            cpu_load_percent: Reading::unavailable(cause.clone()),
            cpu_temperature_c: Reading::unavailable(cause.clone()),
            memory: Reading::unavailable(cause),
        }
    }
}

/// A collector that failed or timed out during one pass.
///
/// The daemon keeps running and reports the failure instead of losing the
/// whole snapshot to one bad sensor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectorFailure {
    pub collector: Collector,
    pub detail: String,
}

/// Ordered so a collector can key a map: the daemon tracks one health state per
/// collector, and the order the variants are declared in is the order those
/// states are reported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collector {
    Kraken,
    Cpu,
    Gpu,
    Memory,
}

impl Collector {
    pub fn label(self) -> &'static str {
        match self {
            Self::Kraken => "Kraken",
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Memory => "Memory",
        }
    }
}

/// One complete pass of every collector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    /// Increments once per pass, so a client can tell a repeat from a refresh.
    pub sequence: u64,
    /// When the pass started, in milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// Interval the sampler is running at.
    pub interval_ms: u64,
    pub kraken: KrakenTelemetry,
    pub system: SystemTelemetry,
    pub gpu: GpuTelemetry,
    pub alerts: Vec<SafetyAlert>,
    pub failed: Vec<CollectorFailure>,
}

impl TelemetrySnapshot {
    /// A snapshot in which nothing could be sampled.
    pub fn unavailable(at_unix_ms: u64, cause: Unavailable) -> Self {
        Self {
            sequence: 0,
            at_unix_ms,
            interval_ms: SAMPLE_INTERVAL_MS,
            kraken: KrakenTelemetry::absent(at_unix_ms, cause.clone()),
            system: SystemTelemetry::unavailable(at_unix_ms, cause.clone()),
            gpu: GpuTelemetry::unavailable(at_unix_ms, cause),
            alerts: Vec::new(),
            failed: Vec::new(),
        }
    }

    /// Milliseconds between the sample and `now`, saturating at zero.
    pub fn age_ms(&self, now_unix_ms: u64) -> u64 {
        now_unix_ms.saturating_sub(self.at_unix_ms)
    }

    /// Age of the oldest section, which is what the freshness budget is about.
    pub fn oldest_section_age_ms(&self, now_unix_ms: u64) -> u64 {
        [
            self.kraken.at_unix_ms,
            self.system.at_unix_ms,
            self.gpu.at_unix_ms,
        ]
        .into_iter()
        .map(|at| now_unix_ms.saturating_sub(at))
        .max()
        .unwrap_or(0)
    }

    pub fn failure(&self, collector: Collector) -> Option<&CollectorFailure> {
        self.failed
            .iter()
            .find(|failure| failure.collector == collector)
    }
}

/// Clamp a percentage into the 0-100 range the interface promises.
///
/// `None` when the value is not finite. Clamping a NaN or an infinity onto an
/// end of the range would publish 0 or 100, and both are figures a working
/// sensor reports, so the result would be indistinguishable from a measurement:
/// the one substitution this module exists to prevent. A percentage that could
/// not be computed is a gap, and [`Reading::percent`] is how a collector says
/// so in one line.
pub fn clamp_percent(value: f32) -> Option<f32> {
    value.is_finite().then(|| value.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converting_a_reading_keeps_a_gap_as_a_gap() {
        let valid: Reading<u16> = Reading::valid(2_970);
        assert_eq!(valid.map(|rpm| f32::from(*rpm)).copied(), Some(2_970.0));

        // The whole point: a failure must not become a zero on the way through.
        let missing: Reading<u16> = Reading::unavailable(Unavailable::unreadable("fan1_input"));
        let widened = missing.map(|rpm| f32::from(*rpm));
        assert_eq!(widened.value(), None);
        assert!(widened.cause().is_some(), "the reason has to survive");
    }

    fn missing() -> Reading<f32> {
        Reading::unavailable(Unavailable::unreadable("/sys/.../temp1_input: EIO"))
    }

    #[test]
    fn an_unreadable_metric_is_unavailable_with_its_cause_not_a_zero() {
        let reading = missing();
        assert!(!reading.is_valid());
        assert_eq!(reading.copied(), None);
        let cause = reading.cause().unwrap();
        assert!(cause.detail().contains("EIO"), "{cause}");
    }

    #[test]
    fn pwm_modes_round_trip_through_the_kernel_abi() {
        for mode in [PwmMode::FullSpeed, PwmMode::Fixed, PwmMode::Curve] {
            assert_eq!(PwmMode::from_kernel(mode.to_kernel()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(PwmMode::from_kernel(3), None);
        // Mode 0 is the firmware failsafe, not "off".
        assert!(PwmMode::FullSpeed.label().contains("100%"));
    }

    #[test]
    fn percentages_are_clamped_into_the_range_the_interface_promises() {
        assert_eq!(clamp_percent(-5.0), Some(0.0));
        assert_eq!(clamp_percent(140.0), Some(100.0));
        assert_eq!(clamp_percent(42.5), Some(42.5));
    }

    /// Clamping a non-finite value onto an end of the range would publish 0 or
    /// 100, and a working sensor reports both. The gap has to stay a gap here
    /// for the same reason it does everywhere else in this module.
    #[test]
    fn a_percentage_that_is_not_a_number_never_becomes_zero_or_a_hundred() {
        for impossible in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(clamp_percent(impossible), None, "{impossible}");

            let reading = Reading::percent(impossible, "/proc/stat");
            assert!(!reading.is_valid());
            let detail = reading.cause().unwrap().detail();
            assert!(detail.contains("/proc/stat"), "{detail}");
            assert!(detail.contains("finite percentage"), "{detail}");
        }

        // A value that is a number still arrives clamped and valid.
        assert_eq!(Reading::percent(140.0, "nvml").copied(), Some(100.0));
        assert_eq!(Reading::percent(-1.0, "nvml").copied(), Some(0.0));
        assert_eq!(Reading::percent(42.5, "nvml").copied(), Some(42.5));
    }

    #[test]
    fn memory_is_reported_in_explicit_binary_units() {
        assert_eq!(format_binary_bytes(0), "0 B");
        assert_eq!(format_binary_bytes(2048), "2 KiB");
        assert_eq!(format_binary_bytes(12 * 1024 * 1024), "12 MiB");
        assert_eq!(format_binary_bytes(32 * 1024 * 1024 * 1024), "32.0 GiB");
    }

    /// A zero total is a figure that was never read. Reporting 0% for it would
    /// draw an empty bar next to a full one, which is the same confusion a
    /// zeroed temperature causes.
    #[test]
    fn memory_occupancy_is_a_clamped_percentage_or_nothing_at_all() {
        let usage = MemoryUsage {
            used_bytes: 512,
            total_bytes: 1024,
        };
        assert!((usage.percent().unwrap() - 50.0).abs() < 0.001);

        assert_eq!(
            MemoryUsage {
                used_bytes: 1,
                total_bytes: 0
            }
            .percent(),
            None
        );
        // Including when nothing at all was read, which is the shape the
        // collector produces before it has answered once.
        assert_eq!(
            MemoryUsage {
                used_bytes: 0,
                total_bytes: 0
            }
            .percent(),
            None
        );
    }

    #[test]
    fn an_absent_kraken_reports_every_field_unavailable() {
        let telemetry =
            KrakenTelemetry::absent(1, Unavailable::no_device("1e71:300e is not present"));
        assert!(!telemetry.present);
        assert!(!telemetry.liquid_temperature_c.is_valid());
        for channel in [Channel::Pump, Channel::Fan] {
            let entry = telemetry.channel(channel);
            assert_eq!(entry.channel, channel);
            assert!(!entry.rpm.is_valid());
            assert!(!entry.duty.is_valid());
            assert!(!entry.mode.is_valid());
            assert_eq!(entry.duty_percent(), None);
        }
    }

    #[test]
    fn a_snapshot_round_trips_through_json_with_its_causes() {
        let snapshot = TelemetrySnapshot::unavailable(1_700, Unavailable::absent("no hwmon"));
        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: TelemetrySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, parsed);
        assert!(json.contains("\"reading\":\"unavailable\""), "{json}");
        assert_eq!(snapshot.age_ms(2_000), 300);
        assert_eq!(snapshot.age_ms(1_000), 0);
    }

    #[test]
    fn a_wedged_section_ages_without_dragging_the_others_with_it() {
        let mut snapshot = TelemetrySnapshot::unavailable(10_000, Unavailable::absent("none"));
        snapshot.kraken.at_unix_ms = 10_000;
        snapshot.system.at_unix_ms = 10_000;
        // The GPU collector stopped publishing eight seconds ago.
        snapshot.gpu.at_unix_ms = 2_000;

        assert_eq!(snapshot.oldest_section_age_ms(10_500), 8_500);
        assert_eq!(
            snapshot.kraken.at_unix_ms.max(snapshot.system.at_unix_ms),
            10_000
        );
    }
    #[test]
    fn a_failed_collector_is_findable_by_name() {
        let mut snapshot = TelemetrySnapshot::unavailable(1, Unavailable::absent("none"));
        snapshot.failed.push(CollectorFailure {
            collector: Collector::Gpu,
            detail: "the collector panicked".into(),
        });
        assert!(snapshot.failure(Collector::Gpu).is_some());
        assert!(snapshot.failure(Collector::Cpu).is_none());
        assert_eq!(Collector::Gpu.label(), "GPU");
    }
}
