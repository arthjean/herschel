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
//! Aging lives here too. The daemon stamps each snapshot, the client decides
//! when a retained value becomes stale and when it disappears, and both use the
//! same constants.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::profile::Channel;

/// Interval between two samples, in milliseconds.
///
/// FR-05: Kraken, CPU, GPU and RAM are sampled once per second.
pub const SAMPLE_INTERVAL_MS: u64 = 1_000;

/// Age at which a retained value is presented as stale.
pub const STALE_AFTER_MS: u64 = 2_000;

/// Age at which a retained value is dropped rather than shown stale.
pub const DROP_AFTER_MS: u64 = 10_000;

/// Longest history the product keeps, in milliseconds.
///
/// FR-16: fifteen minutes, in memory, and no history database anywhere.
pub const HISTORY_WINDOW_MS: u64 = 15 * 60 * 1_000;

/// Liquid temperature at which the firmware failsafe owns the cooler.
///
/// The kernel curve ABI stops at 59 C by construction, so nothing this product
/// writes can alter behavior at or above this temperature. The threshold is
/// here so the interface can say so, not so it can intervene.
pub const LIQUID_CRITICAL_C: f32 = 60.0;

/// Consecutive zero-RPM samples that turn a commanded channel into an alert.
pub const STALL_SAMPLE_COUNT: u8 = 3;

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
    pub fn percent(self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// A condition the Cooling screen must surface immediately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "alert", rename_all = "snake_case")]
pub enum SafetyAlert {
    /// The coolant is at or above the failsafe threshold.
    LiquidCritical {
        temperature_c: f32,
        threshold_c: f32,
    },
    /// A commanded channel has reported zero RPM for several samples.
    ChannelStalled {
        channel: Channel,
        commanded_duty: u8,
        samples: u8,
        rpm: u32,
    },
}

impl SafetyAlert {
    /// The channel this alert is about, when it names one.
    pub fn channel(&self) -> Option<Channel> {
        match self {
            Self::LiquidCritical { .. } => None,
            Self::ChannelStalled { channel, .. } => Some(*channel),
        }
    }

    /// One sentence naming the affected channel and the current readback.
    pub fn message(&self) -> String {
        match self {
            Self::LiquidCritical {
                temperature_c,
                threshold_c,
            } => format!(
                "Liquid temperature is {} C, at or above the {threshold_c:.0} C failsafe \
                 threshold. The firmware runs both channels at 100% and this application does \
                 not override it.",
                format_temperature(*temperature_c)
            ),
            Self::ChannelStalled {
                channel,
                commanded_duty,
                samples,
                rpm,
            } => format!(
                "{channel} reports {rpm} RPM for {samples} consecutive samples while a duty of \
                 {commanded_duty}/255 is commanded. Readback shows the channel is not turning."
            ),
        }
    }
}

/// Turns a stream of Kraken samples into the alerts the Cooling screen shows.
///
/// A single zero-RPM reading is not a fault: a tachometer misses a pulse now
/// and then. [`STALL_SAMPLE_COUNT`] consecutive zero readings on a channel that
/// is being commanded to turn is, and at one sample per second the condition
/// surfaces well inside the two seconds US-012 allows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertTracker {
    pump_zero_samples: u8,
    fan_zero_samples: u8,
}

impl AlertTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one sample in and return every alert currently active.
    pub fn observe(&mut self, telemetry: &KrakenTelemetry) -> Vec<SafetyAlert> {
        let mut alerts = Vec::new();

        if let Some(temperature_c) = telemetry.liquid_temperature_c.copied()
            && temperature_c >= LIQUID_CRITICAL_C
        {
            alerts.push(SafetyAlert::LiquidCritical {
                temperature_c,
                threshold_c: LIQUID_CRITICAL_C,
            });
        }

        for channel in [Channel::Pump, Channel::Fan] {
            let entry = telemetry.channel(channel);
            let commanded = commanded_duty(entry);
            let rpm = entry.rpm.copied();

            let counter = match channel {
                Channel::Pump => &mut self.pump_zero_samples,
                Channel::Fan => &mut self.fan_zero_samples,
            };

            match (commanded, rpm) {
                (Some(duty), Some(0)) if duty > 0 => {
                    *counter = counter.saturating_add(1);
                    if *counter >= STALL_SAMPLE_COUNT {
                        alerts.push(SafetyAlert::ChannelStalled {
                            channel,
                            commanded_duty: duty,
                            samples: *counter,
                            rpm: 0,
                        });
                    }
                }
                // An unreadable sample proves nothing either way, so the streak
                // is neither advanced nor cleared by it.
                (None, _) | (_, None) => {}
                _ => *counter = 0,
            }
        }

        alerts
    }
}

/// The duty a channel is currently being told to run at.
///
/// Mode 0 is the firmware failsafe, which commands 100% whatever `pwmN` holds.
fn commanded_duty(entry: &ChannelTelemetry) -> Option<u8> {
    match entry.mode.copied()? {
        PwmMode::FullSpeed => Some(255),
        PwmMode::Fixed | PwmMode::Curve => entry.duty.copied(),
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

/// How a retained metric currently presents.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricView<T> {
    /// Sampled recently enough to be shown as current.
    Fresh { value: T },
    /// The last valid value, kept visible and marked as aging.
    Stale { value: T, age_ms: u64 },
    /// Nothing valid is available, and `cause` says why when one is known.
    Unavailable { cause: Option<Unavailable> },
}

impl<T> MetricView<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Fresh { value } | Self::Stale { value, .. } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// A word shown next to the value, so state never rests on color alone.
    pub fn qualifier(&self) -> Option<&'static str> {
        match self {
            Self::Fresh { .. } => None,
            Self::Stale { .. } => Some("Stale"),
            Self::Unavailable { .. } => Some("N/A"),
        }
    }

    /// The sentence explaining an unavailable metric, when there is one.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Fresh { .. } | Self::Stale { .. } => None,
            Self::Unavailable { cause } => cause.as_ref().map(Unavailable::detail),
        }
    }
}

impl<T: Copy> MetricView<T> {
    pub fn copied(&self) -> Option<T> {
        self.value().copied()
    }
}

/// The last valid value of one metric, with its age.
///
/// A temporary read failure does not blank a readout: the previous value stays
/// visible and ages out. [`STALE_AFTER_MS`] and [`DROP_AFTER_MS`] are the two
/// thresholds US-006 sets.
#[derive(Debug, Clone, PartialEq)]
pub struct Tracked<T> {
    last_valid: Option<(T, u64)>,
    cause: Option<Unavailable>,
}

// Written out rather than derived: a derived `Default` would demand
// `T: Default`, and a metric that has never been sampled has no default value
// to fall back on. That is the whole point of the type.
impl<T> Default for Tracked<T> {
    fn default() -> Self {
        Self {
            last_valid: None,
            cause: None,
        }
    }
}

impl<T: Clone> Tracked<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one reading in, stamped with the time it was sampled.
    pub fn observe(&mut self, reading: &Reading<T>, at_unix_ms: u64) {
        match reading {
            Reading::Valid { value } => {
                self.last_valid = Some((value.clone(), at_unix_ms));
                self.cause = None;
            }
            Reading::Unavailable { cause } => self.cause = Some(cause.clone()),
        }
    }

    /// How the metric presents at `now_unix_ms`.
    pub fn view(&self, now_unix_ms: u64) -> MetricView<T> {
        let Some((value, at)) = &self.last_valid else {
            return MetricView::Unavailable {
                cause: self.cause.clone(),
            };
        };
        let age_ms = now_unix_ms.saturating_sub(*at);
        if age_ms >= DROP_AFTER_MS {
            MetricView::Unavailable {
                cause: self.cause.clone(),
            }
        } else if age_ms >= STALE_AFTER_MS {
            MetricView::Stale {
                value: value.clone(),
                age_ms,
            }
        } else {
            MetricView::Fresh {
                value: value.clone(),
            }
        }
    }
}

/// One charted point. `None` marks a gap the chart must draw as a gap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryPoint {
    pub at_unix_ms: u64,
    pub value: Option<f32>,
}

/// A bounded in-memory series.
///
/// FR-16: nothing older than the window is kept, and nothing is written
/// anywhere. The deque is pruned by timestamp, so a sampler that stalls cannot
/// leave a series longer than the window either.
#[derive(Debug, Clone)]
pub struct History {
    points: VecDeque<HistoryPoint>,
    window_ms: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::new(HISTORY_WINDOW_MS)
    }
}

impl History {
    pub fn new(window_ms: u64) -> Self {
        Self {
            points: VecDeque::new(),
            window_ms: window_ms.max(1),
        }
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// Append a point and drop everything outside the window.
    ///
    /// A point older than the newest one is ignored: a clock that jumps
    /// backwards must not reorder the series.
    pub fn push(&mut self, at_unix_ms: u64, value: Option<f32>) {
        if let Some(last) = self.points.back()
            && at_unix_ms < last.at_unix_ms
        {
            return;
        }
        self.points.push_back(HistoryPoint { at_unix_ms, value });
        let cutoff = at_unix_ms.saturating_sub(self.window_ms);
        while self
            .points
            .front()
            .is_some_and(|point| point.at_unix_ms < cutoff)
        {
            self.points.pop_front();
        }
    }

    pub fn points(&self) -> impl Iterator<Item = &HistoryPoint> {
        self.points.iter()
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// Clamp a percentage into the 0-100 range the interface promises.
pub fn clamp_percent(value: f32) -> f32 {
    if value.is_nan() {
        return 0.0;
    }
    value.clamp(0.0, 100.0)
}

/// Format a temperature with exactly one decimal place.
pub fn format_temperature(value: f32) -> String {
    format!("{value:.1}")
}

/// Format a byte count with an explicit binary unit.
///
/// Binary rather than decimal, and spelled out, so `GiB` can never be read as
/// `GB` by whoever compares the figure with another tool.
pub fn format_binary_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= KIB * KIB * KIB {
        format!("{:.1} GiB", value / (KIB * KIB * KIB))
    } else if value >= KIB * KIB {
        format!("{:.0} MiB", value / (KIB * KIB))
    } else if value >= KIB {
        format!("{:.0} KiB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(value: f32) -> Reading<f32> {
        Reading::valid(value)
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
    fn a_retained_value_becomes_stale_then_disappears() {
        let mut tracked = Tracked::new();
        tracked.observe(&valid(29.8), 10_000);

        assert_eq!(tracked.view(10_500), MetricView::Fresh { value: 29.8 });

        // A read failure does not blank the readout.
        tracked.observe(&missing(), 11_000);
        assert_eq!(tracked.view(11_500), MetricView::Fresh { value: 29.8 });

        let stale = tracked.view(10_000 + STALE_AFTER_MS);
        assert!(stale.is_stale(), "{stale:?}");
        assert_eq!(stale.copied(), Some(29.8));
        assert_eq!(stale.qualifier(), Some("Stale"));

        let dropped = tracked.view(10_000 + DROP_AFTER_MS);
        assert!(dropped.is_unavailable(), "{dropped:?}");
        assert_eq!(dropped.copied(), None);
        assert!(dropped.detail().unwrap().contains("EIO"));
    }

    #[test]
    fn a_fresh_sample_clears_a_previous_failure() {
        let mut tracked = Tracked::new();
        tracked.observe(&valid(29.8), 0);
        tracked.observe(&missing(), 1_000);
        tracked.observe(&valid(31.2), 2_000);
        assert_eq!(tracked.view(2_100), MetricView::Fresh { value: 31.2 });
        assert_eq!(tracked.view(2_100).detail(), None);
    }

    #[test]
    fn a_metric_never_sampled_is_unavailable_from_the_start() {
        let tracked: Tracked<f32> = Tracked::new();
        assert!(tracked.view(0).is_unavailable());
        assert_eq!(tracked.view(0).qualifier(), Some("N/A"));
    }

    #[test]
    fn history_keeps_only_the_window_and_records_gaps() {
        let mut history = History::new(10_000);
        for step in 0..20u64 {
            let at = step * 1_000;
            history.push(at, if step == 5 { None } else { Some(step as f32) });
        }

        assert!(history.len() <= 11, "{} points retained", history.len());
        let oldest = history.points().next().unwrap();
        assert!(oldest.at_unix_ms >= 19_000 - 10_000);
        // The gap survives pruning as an explicit hole, not as a zero.
        let mut history = History::new(10_000);
        history.push(0, Some(1.0));
        history.push(1_000, None);
        history.push(2_000, Some(3.0));
        assert_eq!(history.points().filter(|p| p.value.is_none()).count(), 1);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn history_ignores_a_point_that_travels_backwards_in_time() {
        let mut history = History::new(10_000);
        history.push(5_000, Some(1.0));
        history.push(4_000, Some(2.0));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn the_default_history_window_is_the_fifteen_minutes_the_prd_allows() {
        assert_eq!(History::default().window_ms(), 15 * 60 * 1_000);
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
    fn percentages_are_clamped_and_temperatures_keep_one_decimal() {
        assert_eq!(clamp_percent(-5.0), 0.0);
        assert_eq!(clamp_percent(140.0), 100.0);
        assert_eq!(clamp_percent(f32::NAN), 0.0);
        assert_eq!(format_temperature(46.75), "46.8");
        assert_eq!(format_temperature(29.0), "29.0");
    }

    #[test]
    fn memory_is_reported_in_explicit_binary_units() {
        assert_eq!(format_binary_bytes(0), "0 B");
        assert_eq!(format_binary_bytes(2048), "2 KiB");
        assert_eq!(format_binary_bytes(12 * 1024 * 1024), "12 MiB");
        assert_eq!(format_binary_bytes(32 * 1024 * 1024 * 1024), "32.0 GiB");
    }

    #[test]
    fn memory_occupancy_is_a_clamped_percentage() {
        let usage = MemoryUsage {
            used_bytes: 512,
            total_bytes: 1024,
        };
        assert!((usage.percent() - 50.0).abs() < 0.001);
        assert_eq!(
            MemoryUsage {
                used_bytes: 1,
                total_bytes: 0
            }
            .percent(),
            0.0
        );
    }

    #[test]
    fn a_stalled_channel_alert_names_the_channel_and_its_readback() {
        let alert = SafetyAlert::ChannelStalled {
            channel: Channel::Pump,
            commanded_duty: 180,
            samples: STALL_SAMPLE_COUNT,
            rpm: 0,
        };
        let message = alert.message();
        assert_eq!(alert.channel(), Some(Channel::Pump));
        assert!(message.contains("Pump"), "{message}");
        assert!(message.contains("180"), "{message}");
        assert!(message.contains("0 RPM"), "{message}");
    }

    #[test]
    fn the_critical_alert_states_that_the_failsafe_is_not_overridden() {
        let alert = SafetyAlert::LiquidCritical {
            temperature_c: 61.4,
            threshold_c: LIQUID_CRITICAL_C,
        };
        let message = alert.message();
        assert!(message.contains("61.4"), "{message}");
        assert!(message.contains("does not override"), "{message}");
        assert_eq!(alert.channel(), None);
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

    fn kraken(temperature_c: f32, mode: PwmMode, duty: u8, pump_rpm: u32) -> KrakenTelemetry {
        KrakenTelemetry {
            at_unix_ms: 0,
            present: true,
            liquid_temperature_c: Reading::valid(temperature_c),
            pump: ChannelTelemetry {
                channel: Channel::Pump,
                rpm: Reading::valid(pump_rpm),
                duty: Reading::valid(duty),
                mode: Reading::valid(mode),
            },
            fan: ChannelTelemetry {
                channel: Channel::Fan,
                rpm: Reading::valid(1_700),
                duty: Reading::valid(duty),
                mode: Reading::valid(mode),
            },
        }
    }

    #[test]
    fn a_single_zero_rpm_sample_is_not_yet_a_stall() {
        let mut tracker = AlertTracker::new();
        let stalled = kraken(30.0, PwmMode::Fixed, 180, 0);

        assert!(tracker.observe(&stalled).is_empty());
        assert!(tracker.observe(&stalled).is_empty());

        let alerts = tracker.observe(&stalled);
        assert_eq!(alerts.len(), 1);
        match &alerts[0] {
            SafetyAlert::ChannelStalled {
                channel,
                commanded_duty,
                samples,
                rpm,
            } => {
                assert_eq!(*channel, Channel::Pump);
                assert_eq!(*commanded_duty, 180);
                assert_eq!(*samples, STALL_SAMPLE_COUNT);
                assert_eq!(*rpm, 0);
            }
            other => panic!("expected a stall, got {other:?}"),
        }

        // One good sample clears the streak.
        tracker.observe(&kraken(30.0, PwmMode::Fixed, 180, 2_900));
        assert!(tracker.observe(&stalled).is_empty());
    }

    #[test]
    fn a_channel_commanded_to_zero_is_not_a_stall() {
        let mut tracker = AlertTracker::new();
        let idle = kraken(30.0, PwmMode::Fixed, 0, 0);
        for _ in 0..5 {
            assert!(tracker.observe(&idle).is_empty());
        }
    }

    #[test]
    fn the_failsafe_mode_counts_as_a_full_duty_command() {
        let mut tracker = AlertTracker::new();
        // `pwmN` can hold anything in mode 0: the channel still runs at 100%.
        let stalled = kraken(30.0, PwmMode::FullSpeed, 0, 0);
        for _ in 0..STALL_SAMPLE_COUNT {
            let _ = tracker.observe(&stalled);
        }
        let alerts = tracker.observe(&stalled);
        assert!(matches!(
            alerts.first(),
            Some(SafetyAlert::ChannelStalled {
                commanded_duty: 255,
                ..
            })
        ));
    }

    #[test]
    fn an_unreadable_sample_neither_raises_nor_clears_a_stall() {
        let mut tracker = AlertTracker::new();
        let stalled = kraken(30.0, PwmMode::Fixed, 180, 0);
        tracker.observe(&stalled);
        tracker.observe(&stalled);

        let mut unreadable = stalled.clone();
        unreadable.pump.rpm = Reading::unavailable(Unavailable::unreadable("EIO"));
        assert!(tracker.observe(&unreadable).is_empty());

        // The streak survived the gap rather than restarting from zero.
        assert!(!tracker.observe(&stalled).is_empty());
    }

    #[test]
    fn the_liquid_alert_fires_on_the_first_sample_at_the_threshold() {
        let mut tracker = AlertTracker::new();
        assert!(
            tracker
                .observe(&kraken(59.9, PwmMode::Curve, 200, 2_900))
                .is_empty()
        );
        let alerts = tracker.observe(&kraken(60.0, PwmMode::Curve, 200, 2_900));
        assert!(matches!(
            alerts.first(),
            Some(SafetyAlert::LiquidCritical { .. })
        ));
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
