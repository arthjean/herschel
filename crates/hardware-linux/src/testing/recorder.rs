// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared logs of what a fake device was actually sent.
//!
//! Both transports are boxed into the link under test, so a test asking "did
//! that report reach the device?" holds a handle on the log rather than on the
//! device.

use std::sync::{Arc, Mutex};

use crate::hid::REPORT_BYTES;
use crate::usbfs::{BulkTransport, UsbfsError};

/// Every bulk transfer a fake device received, shared with the test.
#[derive(Default)]
pub struct BulkRecorder {
    transfers: Mutex<Vec<(u8, Vec<u8>)>>,
    failure: Mutex<Option<UsbfsError>>,
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
    pub fn fail_with(&self, error: UsbfsError) {
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
#[derive(Default)]
pub struct ReportRecorder {
    pub(super) reports: Mutex<Vec<[u8; REPORT_BYTES]>>,
}

impl ReportRecorder {
    /// Every report received, in order.
    pub fn reports(&self) -> Vec<[u8; REPORT_BYTES]> {
        self.reports.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// The ones carrying `identifier`, which is how a caller asks whether one
    /// particular command reached the device.
    pub fn matching(&self, identifier: [u8; 2]) -> Vec<[u8; REPORT_BYTES]> {
        self.reports()
            .into_iter()
            .filter(|report| report[0] == identifier[0] && report[1] == identifier[1])
            .collect()
    }
}

/// The [`BulkTransport`] half of a fake device.
pub(super) struct RecordingBulk(pub(super) Arc<BulkRecorder>);

impl BulkTransport for RecordingBulk {
    fn write_bulk(&mut self, endpoint: u8, payload: &[u8]) -> Result<(), UsbfsError> {
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
