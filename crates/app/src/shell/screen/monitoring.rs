// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Monitoring screen: system and cooling state at a glance.
//!
//! Readouts and their provenance, and nothing operable. A device that is ready
//! says so by the readings under it moving; the strip at the top speaks only
//! for a device that is not.

use gpui::{Div, div, prelude::*};

use kori_core::telemetry::{Collector, HISTORY_WINDOW_MS, format_binary_bytes, format_temperature};

use crate::components::{DeviceHealth, DeviceRow, Metric, Panel, Sparkline};
use crate::link::DeviceSummary;
use crate::shell::Shell;
use crate::theme::{DEGREE_C, DEVICE_LINE_HEIGHT, color, space};

use super::{metric_row, screen};

/// The devices the monitoring strip still has to speak for.
///
/// A ready device has nothing left to say there: everything it can do is
/// already offered, and everything it cannot is refused at the control that
/// tried, by [`crate::link::LinkState::control_state`]. A device in any other
/// state is the case the strip exists for, so the filter is written once and
/// tested, rather
/// than being an inline predicate that could quietly widen to `true` and take
/// the screen back to a permanent block, or narrow and hide a device that is
/// not answering.
fn degraded_devices(rows: Vec<DeviceSummary>) -> Vec<DeviceSummary> {
    rows.into_iter()
        .filter(|summary| summary.health != DeviceHealth::Ready)
        .collect()
}

impl Shell {
    pub(crate) fn monitoring(&self) -> Div {
        let now = self.now_unix_ms;
        let book = &self.metrics;

        let percent = |value: f32| format!("{value:.0}");
        let temperature = format_temperature;
        let rpm = |value: f32| format!("{value:.0}");

        let memory_detail = match book.memory.view(now).copied() {
            Some(usage) => format!(
                "{} of {} in use",
                format_binary_bytes(usage.used_bytes),
                format_binary_bytes(usage.total_bytes)
            ),
            None => "Installed memory is not readable.".to_string(),
        };

        let gpu_subtitle = match book.gpu_name.view(now).value() {
            Some(name) => format!("{name}, read through {}", self.gpu_source()),
            None => self
                .link
                .collector_failure(Collector::Gpu)
                .map(|failure| failure.detail.clone())
                .unwrap_or_else(|| {
                    "No GPU management interface answered on this machine.".to_string()
                }),
        };

        screen("Monitoring", "System and cooling state at a glance.")
            .children(self.device_strip())
            .child(
                Panel::new("CPU")
                    .subtitle(self.collector_note(Collector::Cpu, "Load and package temperature."))
                    .render()
                    .child(
                        metric_row()
                            .child(
                                Metric::from_view("Load", &book.cpu_load.view(now), percent)
                                    .unit("%")
                                    .bar(book.cpu_load.view(now).copied().map(|v| v / 100.0))
                                    .render(),
                            )
                            .child(
                                Metric::from_view(
                                    "Temperature",
                                    &book.cpu_temperature.view(now),
                                    temperature,
                                )
                                .unit(DEGREE_C)
                                .bar(
                                    book.cpu_temperature
                                        .view(now)
                                        .copied()
                                        .map(|value| value / 100.0),
                                )
                                .render(),
                            ),
                    )
                    .child(Sparkline::new(book.cpu_load.history(), 0.0, 100.0).render()),
            )
            .child(
                Panel::new("GPU")
                    .subtitle(gpu_subtitle)
                    .render()
                    .child(
                        metric_row()
                            .child(
                                Metric::from_view("Load", &book.gpu_load.view(now), percent)
                                    .unit("%")
                                    .bar(book.gpu_load.view(now).copied().map(|v| v / 100.0))
                                    .render(),
                            )
                            .child(
                                Metric::from_view(
                                    "Temperature",
                                    &book.gpu_temperature.view(now),
                                    temperature,
                                )
                                .unit(DEGREE_C)
                                .bar(
                                    book.gpu_temperature
                                        .view(now)
                                        .copied()
                                        .map(|value| value / 100.0),
                                )
                                .render(),
                            ),
                    )
                    .child(Sparkline::new(book.gpu_load.history(), 0.0, 100.0).render()),
            )
            .child(
                Panel::new("Memory")
                    .subtitle(memory_detail)
                    .render()
                    .child(
                        metric_row().child(
                            Metric::from_view("In use", &book.memory_percent.view(now), percent)
                                .unit("%")
                                .bar(
                                    book.memory_percent
                                        .view(now)
                                        .copied()
                                        .map(|value| value / 100.0),
                                )
                                .render(),
                        ),
                    )
                    .child(Sparkline::new(book.memory_percent.history(), 0.0, 100.0).render()),
            )
            .child(
                Panel::new("Kraken")
                    .subtitle(self.collector_note(
                        Collector::Kraken,
                        "Coolant temperature and both tachometers, through kraken2023.",
                    ))
                    .render()
                    .child(
                        metric_row()
                            .child(
                                Metric::from_view("Liquid", &book.liquid.view(now), temperature)
                                    .unit(DEGREE_C)
                                    .bar(
                                        book.liquid
                                            .view(now)
                                            .copied()
                                            .map(|value| (value - 20.0) / 40.0),
                                    )
                                    .render(),
                            )
                            .child(
                                Metric::from_view("Pump", &book.pump.rpm.view(now), rpm)
                                    .unit(" RPM")
                                    .bar(
                                        book.pump
                                            .rpm
                                            .view(now)
                                            .copied()
                                            .map(|value| value / 3000.0),
                                    )
                                    .render(),
                            )
                            .child(
                                Metric::from_view("Fan", &book.fan.rpm.view(now), rpm)
                                    .unit(" RPM")
                                    .bar(
                                        book.fan.rpm.view(now).copied().map(|value| value / 2000.0),
                                    )
                                    .render(),
                            ),
                    )
                    .child(Sparkline::new(book.liquid.history(), 20.0, 60.0).render())
                    .child(
                        div()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child(format!(
                                "Rolling {} minute window, held in memory only.",
                                HISTORY_WINDOW_MS / 60_000
                            )),
                    ),
            )
    }

    /// The interface NVML or its absence is reported under.
    fn gpu_source(&self) -> String {
        self.link
            .telemetry()
            .map(|snapshot| snapshot.gpu.source.clone())
            .unwrap_or_else(|| "no interface".to_string())
    }

    /// A section subtitle that names a failed collector instead of its usual
    /// description.
    fn collector_note(&self, collector: Collector, usual: &'static str) -> String {
        match self.link.collector_failure(collector) {
            Some(failure) => failure.detail.clone(),
            None => usual.to_string(),
        }
    }

    /// Which hardware answered, as a caption under the screen heading.
    ///
    /// Not a panel, and not always drawn. This used to be a titled card at the
    /// head of the screen, which gave the two devices the same weight as the CPU
    /// and GPU sections and spent a heading, a line of policy and four lines of
    /// prose to say "both devices are ready". Every fact on it was already
    /// carried better somewhere else:
    ///
    /// - The state word duplicates [`crate::link::LinkState::control_state`],
    ///   which is the single
    ///   gate every write passes through and which names the refusal on the
    ///   control the operator just tried to use, in language about that control.
    /// - The Kraken's presence is proven by its own readings further down this
    ///   screen. A device that stopped answering shows it in Liquid, Pump and
    ///   Fan, not in a line of provenance above them.
    /// - Firmware, kernel binding and the USB identity are static for a session
    ///   and are follow-up questions rather than glances, so they live on the
    ///   Settings screen, which is the diagnostics list.
    ///
    /// What is left is the exception, and only the exception: a device that is
    /// not [`DeviceHealth::Ready`] gets its full line here, because that is
    /// exactly when its firmware and its kernel binding are the evidence that
    /// explains the degradation. A machine with both devices ready draws
    /// nothing, and the screen opens on the readings it is named for.
    ///
    /// The dropped sentence about the allowlist is not a loss of meaning: it
    /// described a property of the process, not a state of the hardware. That
    /// policy is stated where it is enforced, in `ALLOWLIST`.
    fn device_strip(&self) -> Option<Div> {
        let strip = div().flex().flex_col().w_full().min_w_0().gap(space::XS);
        let rows = self.link.device_rows();

        // Nothing supported at all is itself the exception, and the one case
        // where no device line can carry the news.
        if rows.is_empty() {
            return Some(
                strip.child(
                    div()
                        .text_xs()
                        .min_h(DEVICE_LINE_HEIGHT)
                        .text_color(color::TEXT_MUTED.hsla())
                        .child("No supported NZXT device detected."),
                ),
            );
        }

        let degraded = degraded_devices(rows);
        if degraded.is_empty() {
            return None;
        }

        Some(strip.children(degraded.into_iter().map(|summary| {
            DeviceRow::new(summary.name.clone(), summary.id.to_string(), summary.health)
                .detail(summary.detail())
                .render()
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::{DeviceId, KRAKEN_BASE, RGB_CONTROLLER};

    fn device(id: DeviceId, health: DeviceHealth) -> DeviceSummary {
        DeviceSummary {
            id,
            name: "Device".to_string(),
            firmware: Some("0200".to_string()),
            driver: "kraken2023".to_string(),
            health,
        }
    }

    #[test]
    fn the_monitoring_strip_speaks_for_a_device_only_while_it_is_not_ready() {
        // A machine where both devices answered and both are writable draws no
        // strip at all: the screen opens on the readings it is named for, and
        // the provenance is on the diagnostics screen.
        assert!(
            degraded_devices(vec![
                device(KRAKEN_BASE, DeviceHealth::Ready),
                device(RGB_CONTROLLER, DeviceHealth::Ready),
            ])
            .is_empty()
        );

        // Every other state is the exception the strip exists for, and it is
        // named per device rather than collapsed into one line: a read-only
        // controller beside a ready Kraken is a different machine from one
        // where neither answered.
        for health in [DeviceHealth::ReadOnly, DeviceHealth::Unavailable] {
            let shown = degraded_devices(vec![
                device(KRAKEN_BASE, DeviceHealth::Ready),
                device(RGB_CONTROLLER, health),
            ]);
            assert_eq!(shown.len(), 1, "{health:?} was not reported");
            assert_eq!(shown[0].id, RGB_CONTROLLER);
            assert_eq!(shown[0].health, health);
        }
    }
}
