// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Moving 64-byte reports through the `hidraw` node the kernel already created.
//!
//! Both allowlisted devices speak the same shape: numbered reports of 63
//! payload bytes behind one identifier byte, over an interface `usbhid` owns.
//! Neither device is detached from its driver to reach it. On the Kraken that
//! matters twice over, because the interface carrying these reports is the one
//! `kraken2023` is bound to: the driver calls `hid_hw_start` with
//! `HID_CONNECT_HIDRAW` precisely so user space can share it, and the report
//! identifiers this product sends are disjoint from the ones the driver uses.
//!
//! What travels over a report is device-specific and lives in [`crate::rgb`]
//! and [`crate::lcd`]. This module knows only how to move one.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Every report on both devices carries 63 payload bytes behind its identifier.
pub const REPORT_BYTES: usize = 64;

/// How long a query waits for one answer before giving up.
///
/// Measured on the owned RGB controller, five runs: the firmware answer lands
/// in 2 ms and the topology answer takes 518 to 699 ms. The ceiling is set well
/// above the slowest observation rather than at it, because a probe that times
/// out reports "the device said nothing" for a device that was still answering.
pub const ANSWER_TIMEOUT: Duration = Duration::from_millis(2_000);

/// How long the transport waits on a single read attempt.
const READ_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Ceiling on how many stale reports one drain discards.
const MAX_DRAINED_REPORTS: usize = 64;

/// Reports one query reads before it gives up matching answers.
pub const MAX_QUERY_READS: usize = 12;

/// The firmware report both devices answer.
///
/// NZXT uses one identifier pair across this generation, so the Kraken and the
/// RGB controller are asked the same way. `lcd::packet` and `rgb::packet` both
/// re-export these, and a test in each pins them to this definition so the two
/// cannot drift onto different bytes.
pub const FIRMWARE_REQUEST: [u8; 2] = [0x10, 0x01];
pub const FIRMWARE_ANSWER: [u8; 2] = [0x11, 0x01];

/// Offset of the first of the three firmware bytes in a `0x11 0x01` answer.
const FIRMWARE_OFFSET: usize = 0x11;

/// Why a report could not be moved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HidError {
    #[error("no hidraw node exists for the controller: {reason}")]
    NodeAbsent { reason: String },
    #[error("permission denied on {path}. Check the installed udev rule.")]
    PermissionDenied { path: String },
    #[error("{path}: {detail}")]
    Io { path: String, detail: String },
    #[error("the controller sent no {expected} answer within {waited_ms} ms")]
    NoAnswer { expected: String, waited_ms: u64 },
    #[error("the controller accepted {wrote} of {expected} bytes")]
    ShortWrite { wrote: usize, expected: usize },
}

impl HidError {
    pub(crate) fn io(path: &Path, error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied {
                path: path.display().to_string(),
            },
            std::io::ErrorKind::NotFound => Self::NodeAbsent {
                reason: format!("{} does not exist", path.display()),
            },
            _ => Self::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            },
        }
    }
}

/// The reason one unanswered field carries in a capability record.
pub fn silence(what: &str) -> String {
    HidError::NoAnswer {
        expected: what.to_string(),
        waited_ms: ANSWER_TIMEOUT.as_millis() as u64,
    }
    .to_string()
}

/// Moving 64-byte reports to and from a device.
///
/// A trait rather than a concrete file so the daemon, its integration tests and
/// the write probes all drive the same command code. A test device answers the
/// same reports the hardware does, which is what lets the encoding and the
/// serialization be proven without the device.
pub trait HidTransport: Send {
    /// Send one report. The first byte is its identifier.
    fn write_report(&mut self, report: &[u8; REPORT_BYTES]) -> Result<(), HidError>;

    /// Read one report, or `None` when none arrived within `timeout`.
    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_BYTES]>, HidError>;

    /// Where the reports go, for the capability record's evidence.
    fn source(&self) -> String;
}

/// The `hidraw` node, opened read-write and never blocking indefinitely.
#[derive(Debug)]
pub struct Hidraw {
    file: std::fs::File,
    path: PathBuf,
}

impl Hidraw {
    /// Open the node for reports in both directions.
    ///
    /// `O_NONBLOCK` is deliberate: a device that answers nothing must time out,
    /// not park the daemon's startup thread forever.
    pub fn open(path: &Path) -> Result<Self, HidError> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(path)
            .map_err(|error| HidError::io(path, &error))?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Discard input reports the device queued before this run.
    ///
    /// Without it, the first answer read after a write could be a stale report
    /// from a previous session and an answer would be parsed out of it.
    pub fn drain(&mut self) {
        let mut buffer = [0u8; REPORT_BYTES];
        // Bounded: a device streaming reports faster than they are read must
        // not turn a drain into an unbounded loop.
        for _ in 0..MAX_DRAINED_REPORTS {
            match self.file.read(&mut buffer) {
                Ok(0) => return,
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    }
}

impl HidTransport for Hidraw {
    fn write_report(&mut self, report: &[u8; REPORT_BYTES]) -> Result<(), HidError> {
        let wrote = self
            .file
            .write(report)
            .map_err(|error| HidError::io(&self.path, &error))?;
        if wrote != REPORT_BYTES {
            return Err(HidError::ShortWrite {
                wrote,
                expected: REPORT_BYTES,
            });
        }
        Ok(())
    }

    fn read_report(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT_BYTES]>, HidError> {
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; REPORT_BYTES];
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => {}
                Ok(_) => return Ok(Some(buffer)),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(HidError::io(&self.path, &error)),
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(READ_POLL_INTERVAL);
        }
    }

    fn source(&self) -> String {
        self.path.display().to_string()
    }
}

/// A request carrying nothing but its identifier.
pub fn query(identifier: [u8; 2]) -> [u8; REPORT_BYTES] {
    let mut report = [0u8; REPORT_BYTES];
    report[0] = identifier[0];
    report[1] = identifier[1];
    report
}

/// True when `report` is the answer identified by `identifier`.
pub fn answers(report: &[u8; REPORT_BYTES], identifier: [u8; 2]) -> bool {
    report[0] == identifier[0] && report[1] == identifier[1]
}

/// The firmware revision carried by a `0x11 0x01` answer.
pub fn firmware(report: &[u8; REPORT_BYTES]) -> String {
    format!(
        "{}.{}.{}",
        report[FIRMWARE_OFFSET],
        report[FIRMWARE_OFFSET + 1],
        report[FIRMWARE_OFFSET + 2]
    )
}

/// The major component of a firmware revision, when it parses as one.
///
/// The Kraken's transfer sequence differs between firmware generations, so the
/// major number decides which one is sent. A revision that does not parse
/// yields `None` and the caller refuses rather than guessing a generation.
pub fn firmware_major(firmware: &str) -> Option<u8> {
    firmware.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_carries_its_identifier_and_nothing_else() {
        let report = query(FIRMWARE_REQUEST);
        assert_eq!(report[0], 0x10);
        assert_eq!(report[1], 0x01);
        assert!(
            report[2..].iter().all(|byte| *byte == 0),
            "a query must not carry a parameter the device could act on"
        );
        assert!(answers(&report, FIRMWARE_REQUEST));
        assert!(!answers(&report, FIRMWARE_ANSWER));
    }

    #[test]
    fn the_firmware_answer_is_read_at_the_offset_both_devices_use() {
        let mut report = [0u8; REPORT_BYTES];
        report[0..2].copy_from_slice(&FIRMWARE_ANSWER);
        report[0x11] = 2;
        report[0x12] = 0;
        report[0x13] = 4;
        assert_eq!(firmware(&report), "2.0.4");
        assert_eq!(firmware_major("2.0.4"), Some(2));
        assert_eq!(firmware_major("1.5.0"), Some(1));
        assert_eq!(firmware_major("unknown"), None);
        assert_eq!(firmware_major(""), None);
    }
}
