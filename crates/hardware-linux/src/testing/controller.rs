// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! A stand-in for the RGB controller.

use std::collections::VecDeque;
use std::time::Duration;

use crate::hid::{self, HidError, HidTransport, REPORT_BYTES};
use crate::rgb::packet;

/// A controller that answers the reports the real one answers.
///
/// It exists so the command path can be proven end to end without a physical
/// device: the daemon, its integration tests and the write probe all drive the
/// same encoding, ownership and cadence code, and this stands in for the only
/// part a test cannot own.
///
/// Its answers are built by the encoders that live beside the decoders, so a
/// fixture cannot describe a controller at an offset the product does not read.
#[derive(Debug, Default)]
pub struct FakeController {
    firmware: String,
    /// Accessory identifiers per channel, in channel order.
    channels: Vec<Vec<u8>>,
    pending: VecDeque<[u8; REPORT_BYTES]>,
    /// Every command report the controller received, in order.
    pub commands: Vec<[u8; REPORT_BYTES]>,
    /// When set, every write fails with this error instead of landing.
    pub write_failure: Option<HidError>,
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
}

impl HidTransport for FakeController {
    fn write_report(&mut self, report: &[u8; REPORT_BYTES]) -> Result<(), HidError> {
        if let Some(failure) = &self.write_failure {
            return Err(failure.clone());
        }
        match [report[0], report[1]] {
            hid::FIRMWARE_REQUEST if !self.silent => {
                let answer = hid::firmware_answer(&self.firmware);
                self.pending.push_back(answer);
            }
            packet::LIGHTING_REQUEST if !self.silent && !self.withhold_topology => {
                let answer = packet::topology_answer(&self.channels);
                self.pending.push_back(answer);
            }
            packet::COLOR_COMMAND => self.commands.push(*report),
            _ => {}
        }
        Ok(())
    }

    fn read_report(&mut self, _timeout: Duration) -> Result<Option<[u8; REPORT_BYTES]>, HidError> {
        Ok(self.pending.pop_front())
    }

    fn source(&self) -> String {
        "fake:hidraw".to_string()
    }
}
