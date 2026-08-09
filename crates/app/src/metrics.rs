// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The client's rolling view of every metric.
//!
//! The daemon sends one snapshot at a time. Turning that into a dashboard needs
//! two things it does not carry: what the last valid value was, so a momentary
//! read failure does not blank a readout, and the recent series, so a chart has
//! something to draw. Both live here, in memory, bounded by
//! [`herschel_core::telemetry::HISTORY_WINDOW_MS`]. Nothing is written to disk.

use herschel_core::profile::Channel;
use herschel_core::telemetry::{
    History, KrakenTelemetry, MemoryUsage, MetricView, PwmMode, Reading, TelemetrySnapshot, Tracked,
};

/// One numeric metric: its last valid value and its recent series.
#[derive(Debug, Clone, Default)]
pub struct MetricSeries {
    tracked: Tracked<f32>,
    history: History,
}

impl MetricSeries {
    fn observe(&mut self, reading: &Reading<f32>, at_unix_ms: u64, record: bool) {
        self.tracked.observe(reading, at_unix_ms);
        if record {
            // A gap is recorded as a gap. Charting a missing sample as zero
            // would draw a plunge that never happened.
            self.history.push(at_unix_ms, reading.copied());
        }
    }

    pub fn view(&self, now_unix_ms: u64) -> MetricView<f32> {
        self.tracked.view(now_unix_ms)
    }

    pub fn history(&self) -> &History {
        &self.history
    }
}

/// The readings of one cooling channel.
#[derive(Debug, Clone, Default)]
pub struct ChannelMetrics {
    pub rpm: MetricSeries,
    pub duty: Tracked<u8>,
    pub mode: Tracked<PwmMode>,
}

/// Every metric the interface shows.
#[derive(Debug, Clone, Default)]
pub struct MetricBook {
    pub cpu_load: MetricSeries,
    pub cpu_temperature: MetricSeries,
    pub gpu_load: MetricSeries,
    pub gpu_temperature: MetricSeries,
    pub gpu_name: Tracked<String>,
    pub memory_percent: MetricSeries,
    pub memory: Tracked<MemoryUsage>,
    pub liquid: MetricSeries,
    pub pump: ChannelMetrics,
    pub fan: ChannelMetrics,
    kraken_at_unix_ms: u64,
    system_at_unix_ms: u64,
    gpu_at_unix_ms: u64,
}

impl MetricBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one snapshot in.
    ///
    /// Each section is recorded into its series only when that section's own
    /// timestamp advanced. Polling faster than the sampler must not stack
    /// duplicate points into a fifteen-minute window.
    pub fn observe(&mut self, snapshot: &TelemetrySnapshot) {
        let kraken_at = snapshot.kraken.at_unix_ms;
        let fresh_kraken = kraken_at > self.kraken_at_unix_ms;
        self.observe_kraken(&snapshot.kraken, fresh_kraken);
        if fresh_kraken {
            self.kraken_at_unix_ms = kraken_at;
        }

        let system_at = snapshot.system.at_unix_ms;
        let fresh_system = system_at > self.system_at_unix_ms;
        self.cpu_load
            .observe(&snapshot.system.cpu_load_percent, system_at, fresh_system);
        self.cpu_temperature
            .observe(&snapshot.system.cpu_temperature_c, system_at, fresh_system);
        self.memory.observe(&snapshot.system.memory, system_at);
        let occupancy = match &snapshot.system.memory {
            Reading::Valid { value } => Reading::valid(value.percent()),
            Reading::Unavailable { cause } => Reading::unavailable(cause.clone()),
        };
        self.memory_percent
            .observe(&occupancy, system_at, fresh_system);
        if fresh_system {
            self.system_at_unix_ms = system_at;
        }

        let gpu_at = snapshot.gpu.at_unix_ms;
        let fresh_gpu = gpu_at > self.gpu_at_unix_ms;
        self.gpu_load
            .observe(&snapshot.gpu.load_percent, gpu_at, fresh_gpu);
        self.gpu_temperature
            .observe(&snapshot.gpu.temperature_c, gpu_at, fresh_gpu);
        self.gpu_name.observe(&snapshot.gpu.name, gpu_at);
        if fresh_gpu {
            self.gpu_at_unix_ms = gpu_at;
        }
    }

    fn observe_kraken(&mut self, kraken: &KrakenTelemetry, record: bool) {
        let at = kraken.at_unix_ms;
        self.liquid
            .observe(&kraken.liquid_temperature_c, at, record);

        for channel in [Channel::Pump, Channel::Fan] {
            let source = kraken.channel(channel);
            let metrics = match channel {
                Channel::Pump => &mut self.pump,
                Channel::Fan => &mut self.fan,
            };
            let rpm = match &source.rpm {
                Reading::Valid { value } => Reading::valid(*value as f32),
                Reading::Unavailable { cause } => Reading::unavailable(cause.clone()),
            };
            metrics.rpm.observe(&rpm, at, record);
            metrics.duty.observe(&source.duty, at);
            metrics.mode.observe(&source.mode, at);
        }
    }

    pub fn channel(&self, channel: Channel) -> &ChannelMetrics {
        match channel {
            Channel::Pump => &self.pump,
            Channel::Fan => &self.fan,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herschel_core::telemetry::{
        ChannelTelemetry, DROP_AFTER_MS, GpuTelemetry, STALE_AFTER_MS, SystemTelemetry, Unavailable,
    };

    fn snapshot(at: u64, liquid: Option<f32>) -> TelemetrySnapshot {
        let reading = |value: Option<f32>| match value {
            Some(value) => Reading::valid(value),
            None => Reading::unavailable(Unavailable::unreadable("temp1_input: EIO")),
        };

        TelemetrySnapshot {
            sequence: at,
            at_unix_ms: at,
            interval_ms: 1_000,
            kraken: KrakenTelemetry {
                at_unix_ms: at,
                present: true,
                liquid_temperature_c: reading(liquid),
                pump: ChannelTelemetry {
                    channel: Channel::Pump,
                    rpm: Reading::valid(2_970),
                    duty: Reading::valid(255),
                    mode: Reading::valid(PwmMode::FullSpeed),
                },
                fan: ChannelTelemetry {
                    channel: Channel::Fan,
                    rpm: Reading::valid(1_764),
                    duty: Reading::valid(255),
                    mode: Reading::valid(PwmMode::FullSpeed),
                },
            },
            system: SystemTelemetry {
                at_unix_ms: at,
                cpu_load_percent: Reading::valid(12.5),
                cpu_temperature_c: Reading::valid(46.8),
                memory: Reading::valid(MemoryUsage {
                    used_bytes: 512,
                    total_bytes: 1_024,
                }),
            },
            gpu: GpuTelemetry {
                at_unix_ms: at,
                source: "NVML".into(),
                name: Reading::valid("Test GPU".into()),
                load_percent: Reading::valid(30.0),
                temperature_c: Reading::valid(51.0),
            },
            alerts: Vec::new(),
            failed: Vec::new(),
        }
    }

    #[test]
    fn a_series_records_one_point_per_new_sample() {
        let mut book = MetricBook::new();
        for step in 1..=5u64 {
            book.observe(&snapshot(step * 1_000, Some(30.0 + step as f32)));
        }
        assert_eq!(book.liquid.history().len(), 5);
        assert_eq!(book.cpu_load.history().len(), 5);

        // Polling again without a new sample must not duplicate points.
        book.observe(&snapshot(5_000, Some(35.0)));
        assert_eq!(book.liquid.history().len(), 5);
    }

    #[test]
    fn a_read_failure_leaves_the_last_value_visible_then_drops_it() {
        let mut book = MetricBook::new();
        book.observe(&snapshot(1_000, Some(29.8)));
        book.observe(&snapshot(2_000, None));

        assert_eq!(book.liquid.view(2_100).copied(), Some(29.8));
        assert!(book.liquid.view(1_000 + STALE_AFTER_MS).is_stale());
        assert!(book.liquid.view(1_000 + DROP_AFTER_MS).is_unavailable());

        // The chart carries the failure as a hole rather than as a zero.
        let gaps = book
            .liquid
            .history()
            .points()
            .filter(|point| point.value.is_none())
            .count();
        assert_eq!(gaps, 1);
    }

    #[test]
    fn memory_occupancy_is_derived_alongside_the_raw_figures() {
        let mut book = MetricBook::new();
        book.observe(&snapshot(1_000, Some(29.8)));

        assert!((book.memory_percent.view(1_100).copied().unwrap() - 50.0).abs() < 0.001);
        let usage = book.memory.view(1_100).copied().unwrap();
        assert_eq!(usage.total_bytes, 1_024);
    }

    #[test]
    fn a_section_that_stops_updating_ages_alone() {
        let mut book = MetricBook::new();
        let mut first = snapshot(1_000, Some(29.8));
        book.observe(&first);

        // Only the kraken and system sections advance; the GPU collector is
        // wedged and its timestamp stays where it was.
        for step in 2..=6u64 {
            let mut next = snapshot(step * 1_000, Some(29.8));
            next.gpu = first.gpu.clone();
            book.observe(&next);
            first = next;
        }

        let now = 6_000;
        assert!(book.cpu_load.view(now).copied().is_some());
        assert!(!book.cpu_load.view(now).is_stale());
        assert!(
            book.gpu_load.view(now).is_stale(),
            "the GPU should have aged"
        );
        assert_eq!(book.gpu_load.history().len(), 1);
    }

    #[test]
    fn channel_metrics_are_addressable_by_channel() {
        let mut book = MetricBook::new();
        book.observe(&snapshot(1_000, Some(29.8)));
        assert_eq!(
            book.channel(Channel::Pump).rpm.view(1_100).copied(),
            Some(2_970.0)
        );
        assert_eq!(
            book.channel(Channel::Fan).mode.view(1_100).copied(),
            Some(PwmMode::FullSpeed)
        );
    }
}
