// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! How a sampled metric presents to whoever is looking at it.
//!
//! Everything here is downstream of the readings rather than part of them. A
//! [`Reading`] is what one collector produced in one pass; a [`MetricView`] is
//! what a readout shows now, which depends on how long ago that pass was.
//! [`Tracked`] holds the difference and [`History`] keeps the bounded series a
//! chart plots.
//!
//! Nothing in this module derives `Serialize`, and that is the seam it was
//! split along rather than a detail: none of it crosses the socket. The daemon
//! stamps a snapshot and sends it; deciding when a retained value becomes stale
//! and when it disappears is the reader's own work, done against the reader's
//! own clock. The formatters are here for the same reason: they turn a value
//! into the characters drawn next to it, which is presentation and not
//! measurement.

use std::collections::VecDeque;

use super::{Reading, Unavailable};

/// Age at which a retained value is presented as stale.
pub const STALE_AFTER_MS: u64 = 2_000;

/// Age at which a retained value is dropped rather than shown stale.
pub const DROP_AFTER_MS: u64 = 10_000;

/// Longest history the product keeps, in milliseconds.
///
/// Fifteen minutes, in memory, and no history database anywhere.
pub const HISTORY_WINDOW_MS: u64 = 15 * 60 * 1_000;

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

    /// The same view with its value converted, keeping the freshness intact.
    ///
    /// Freshness is a state rather than a label, so widening a duty byte into
    /// the number a readout formats must not lose the fact that it is stale.
    pub fn map<U>(&self, convert: impl FnOnce(&T) -> U) -> MetricView<U> {
        match self {
            Self::Fresh { value } => MetricView::Fresh {
                value: convert(value),
            },
            Self::Stale { value, age_ms } => MetricView::Stale {
                value: convert(value),
                age_ms: *age_ms,
            },
            Self::Unavailable { cause } => MetricView::Unavailable {
                cause: cause.clone(),
            },
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
/// thresholds that decide when.
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
/// Nothing older than the window is kept, and nothing is written
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
    use crate::telemetry::Unavailable;

    fn valid(value: f32) -> Reading<f32> {
        Reading::valid(value)
    }

    fn missing() -> Reading<f32> {
        Reading::unavailable(Unavailable::unreadable("/sys/.../temp1_input: EIO"))
    }

    #[test]
    fn converting_a_view_keeps_how_old_it_is() {
        let fresh: MetricView<u8> = MetricView::Fresh { value: 180 };
        assert_eq!(fresh.map(|duty| f32::from(*duty)).copied(), Some(180.0));

        let stale: MetricView<u8> = MetricView::Stale {
            value: 180,
            age_ms: 3_000,
        };
        let widened = stale.map(|duty| f32::from(*duty));
        assert!(widened.is_stale(), "freshness is a state, not a label");
        assert_eq!(widened.copied(), Some(180.0));

        let missing: MetricView<u8> = MetricView::Unavailable {
            cause: Some(Unavailable::unreadable("pwm1")),
        };
        let widened = missing.map(|duty| f32::from(*duty));
        assert!(widened.is_unavailable());
        assert_eq!(widened.copied(), None);
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
    fn the_default_history_window_is_fifteen_minutes() {
        assert_eq!(History::default().window_ms(), 15 * 60 * 1_000);
    }

    #[test]
    fn temperatures_keep_exactly_one_decimal() {
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
}
