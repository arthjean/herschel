// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! A stand-in for the Kraken's display half.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use super::recorder::{BulkRecorder, RecordingBulk, ReportRecorder};
use crate::hid::{self, HidError, HidTransport, REPORT_BYTES};
use crate::lcd::{LcdLink, packet};

/// A Kraken that answers the reports the real one answers.
///
/// Built for the LCD path: it responds to the firmware request and to the
/// display report only a device carrying a panel responds to, and it records
/// every report it was sent. [`FakeKraken::without_panel`] is the same device
/// with no screen behind it, which is the case the gate must keep read-only.
pub struct FakeKraken {
    firmware: String,
    /// Brightness and quarter turns the panel answers with, raw.
    ///
    /// `None` for a Kraken whose cap carries no display. The pair is kept raw
    /// rather than as [`kori_core::capability::LcdDisplaySettings`] so a fixture
    /// can answer the out-of-range values the decoder has to refuse.
    panel: Option<(u8, u8)>,
    pending: VecDeque<[u8; REPORT_BYTES]>,
    /// Every report the device received, in order.
    reports: Arc<ReportRecorder>,
    /// When set, every write fails with this error instead of landing.
    pub write_failure: Option<HidError>,
    bulk: Arc<BulkRecorder>,
}

impl FakeKraken {
    /// A Kraken reporting `firmware`, with a panel at 60% and no rotation.
    pub fn new(firmware: &str) -> Self {
        Self {
            firmware: firmware.to_string(),
            panel: Some((60, 0)),
            pending: VecDeque::new(),
            reports: Arc::new(ReportRecorder::default()),
            write_failure: None,
            bulk: Arc::new(BulkRecorder::default()),
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
        self.panel = Some((brightness_percent, quarter_turns));
        self
    }

    /// Handle on the bulk endpoint, taken before the device is boxed away.
    pub fn bulk_recorder(&self) -> Arc<BulkRecorder> {
        Arc::clone(&self.bulk)
    }

    /// Handle on the reports written, taken before the device is boxed away.
    pub fn report_recorder(&self) -> Arc<ReportRecorder> {
        Arc::clone(&self.reports)
    }

    /// Consume the device into the link the LCD code drives.
    pub fn link(self) -> LcdLink {
        let bulk = RecordingBulk(Arc::clone(&self.bulk));
        LcdLink::new(Box::new(self), Box::new(bulk))
    }
}

impl HidTransport for FakeKraken {
    fn write_report(&mut self, report: &[u8; REPORT_BYTES]) -> Result<(), HidError> {
        if let Some(failure) = &self.write_failure {
            return Err(failure.clone());
        }
        if let Ok(mut reports) = self.reports.reports.lock() {
            reports.push(*report);
        }
        match [report[0], report[1]] {
            hid::FIRMWARE_REQUEST => {
                let answer = hid::firmware_answer(&self.firmware);
                self.pending.push_back(answer);
            }
            packet::DISPLAY_INFO_REQUEST => {
                if let Some((brightness, quarter_turns)) = self.panel {
                    let answer = packet::display_info_answer(brightness, quarter_turns);
                    self.pending.push_back(answer);
                }
            }
            // A panel acknowledges both transfer commands, which is what the
            // frame path waits for before it puts pixels on the endpoint.
            [command, sub] if command == packet::TRANSFER_START[0] && self.panel.is_some() => {
                let mut answer = [0u8; REPORT_BYTES];
                answer[0] = packet::TRANSFER_ANSWER;
                answer[1] = sub;
                self.pending.push_back(answer);
            }
            _ => {}
        }
        Ok(())
    }

    fn read_report(&mut self, _timeout: Duration) -> Result<Option<[u8; REPORT_BYTES]>, HidError> {
        Ok(self.pending.pop_front())
    }

    fn source(&self) -> String {
        "fake:hidraw10".to_string()
    }
}
