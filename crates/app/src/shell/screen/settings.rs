// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Settings screen: local paths, versions and diagnostics.
//!
//! The permanent home of the provenance the monitoring strip does not carry.
//! One row per fact, in a label-and-value shape, because that is what these
//! are: static facts an operator reads once, or quotes in a report.

use gpui::{Div, div, prelude::*};

use crate::components::{Button, ControlState, Panel};
use crate::shell::Shell;
use crate::theme::{META_SEPARATOR, UNOFFICIAL_NOTICE, color};

use super::tab::SCREEN_TAB_BASE;
use super::{screen, setting_row};

impl Shell {
    pub(crate) fn settings(&self) -> Div {
        let (socket, version, config) = match self.link.status() {
            Some(status) => (
                status.socket_path.clone(),
                status.daemon_version.clone(),
                match status.config.recovery_message() {
                    Some(message) => message,
                    None => "Loaded".to_string(),
                },
            ),
            None => (
                "not connected".to_string(),
                "unknown".to_string(),
                "not available".to_string(),
            ),
        };

        let sampling = match self.link.telemetry() {
            Some(snapshot) => format!(
                "Every {} ms, {} ms since the last complete pass",
                snapshot.interval_ms,
                snapshot.oldest_section_age_ms(self.now_unix_ms)
            ),
            None => "no telemetry".to_string(),
        };

        let mut panel = Panel::new("Service")
            .render()
            .child(setting_row("Socket", socket))
            .child(setting_row("Daemon version", version))
            .child(setting_row("Configuration", config))
            .child(setting_row("Sampling", sampling))
            .child(setting_row(
                "Network",
                "No network request, no listening TCP or UDP socket.".to_string(),
            ));

        for failure in self.link.failed_collectors() {
            panel = panel.child(setting_row(
                format!("{} collector", failure.collector.label()),
                failure.detail.clone(),
            ));
        }

        screen("Settings", "Local paths, versions and diagnostics.")
            .child(panel)
            .child(
                Panel::new("Devices")
                    .subtitle("Only the two allowlisted devices are ever opened.")
                    .render()
                    .children(self.device_settings()),
            )
            .child(
                Panel::new("Diagnostics")
                    .subtitle("Serial numbers are redacted before anything leaves this machine.")
                    .render()
                    .child(
                        Button::new("export-diagnostics", "Export diagnostics")
                            .tab_index(SCREEN_TAB_BASE)
                            .state(if self.link.status().is_some() {
                                ControlState::Enabled
                            } else {
                                ControlState::disabled("The background service is not running.")
                            })
                            .render(),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(UNOFFICIAL_NOTICE),
            )
    }

    /// Every device and everything known about it, for the diagnostics screen.
    ///
    /// The permanent home of the provenance the monitoring strip no longer
    /// carries. One row per device, in the same label-and-value shape as the
    /// service rows beside it, because that is what these are: static facts an
    /// operator reads once, or quotes in a report.
    ///
    /// The state is the first fragment rather than a colored word at the end.
    /// On this screen it is one more recorded fact, not a signal to act on; the
    /// screen that asks for action is the one that colors it.
    fn device_settings(&self) -> Vec<Div> {
        let rows = self.link.device_rows();
        if rows.is_empty() {
            return vec![setting_row(
                "Devices",
                "No supported NZXT device detected.".to_string(),
            )];
        }

        rows.into_iter()
            .map(|summary| {
                setting_row(
                    summary.name.clone(),
                    format!(
                        "{} {META_SEPARATOR} {} {META_SEPARATOR} {}",
                        summary.health.label(),
                        summary.id,
                        summary.detail()
                    ),
                )
            })
            .collect()
    }
}
