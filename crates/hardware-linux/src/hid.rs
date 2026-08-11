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
///
/// Visible to the crate so the test fixtures build their answers from the same
/// offset the decoder reads, rather than restating the number. The literal is
/// pinned once, by `the_firmware_answer_is_read_at_the_offset_both_devices_use`.
pub(crate) const FIRMWARE_OFFSET: usize = 0x11;

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

/// One request identifier and the answer identifier it is paired with.
pub type Exchange = ([u8; 2], [u8; 2]);

/// Send every request, then collect the answers that came back.
///
/// Both allowlisted devices are interrogated exactly this way, so the rule
/// lives here rather than once per device module. Three parts of it are load
/// bearing and none of them is obvious:
///
/// * All requests go out before the first read. The two answers of a query
///   arrive hundreds of milliseconds apart on the RGB controller, so asking
///   serially would pay the slow answer's latency twice.
/// * Answers are matched by identifier, never by arrival order. Both devices
///   interleave unsolicited status reports, and on the Kraken `kraken2023` is
///   polling the same interface for its own.
/// * The read count is bounded by [`MAX_QUERY_READS`], so a device streaming
///   reports cannot turn a query into an unbounded loop.
///
/// Answers come back positionally, so a caller destructures them. A slot stays
/// `None` when that answer never arrived, which is evidence in its own right:
/// a device that answered one of two questions has told this product something
/// true, and collapsing that into a single failure would put an unknown in the
/// capability record next to a value that was actually read.
pub fn ask<const N: usize, T: HidTransport + ?Sized>(
    transport: &mut T,
    exchanges: [Exchange; N],
) -> Result<[Option<[u8; REPORT_BYTES]>; N], HidError> {
    for (request, _) in exchanges {
        transport.write_report(&query(request))?;
    }

    let mut received = [None; N];
    for _ in 0..MAX_QUERY_READS {
        if received.iter().all(Option::is_some) {
            break;
        }
        let Some(report) = transport.read_report(ANSWER_TIMEOUT)? else {
            break;
        };
        if let Some(slot) = exchanges
            .iter()
            .position(|(_, answer)| answers(&report, *answer))
        {
            received[slot] = Some(report);
        }
    }
    Ok(received)
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

/// Build a `0x11 0x01` answer carrying `firmware`.
///
/// The encoder half of [`firmware`], deliberately kept beside it. A fixture that
/// wrote the revision at its own literal offset would keep passing if the
/// decoder's offset ever moved, so the tests would go on agreeing with each
/// other while the product read the wrong three bytes off a real device.
///
/// A component that is not a number becomes zero: this stands in for a device,
/// and a device answers bytes.
#[cfg(any(test, feature = "testing"))]
pub fn firmware_answer(firmware: &str) -> [u8; REPORT_BYTES] {
    let mut report = [0u8; REPORT_BYTES];
    report[0..2].copy_from_slice(&FIRMWARE_ANSWER);
    for (index, part) in firmware.split('.').take(3).enumerate() {
        report[FIRMWARE_OFFSET + index] = part.parse().unwrap_or(0);
    }
    report
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
        // The one place the literal is pinned. Every other encoder, decoder and
        // fixture in this crate goes through the constant, so this assertion is
        // what would fail if the offset ever moved.
        assert_eq!(FIRMWARE_OFFSET, 0x11);

        let mut report = [0u8; REPORT_BYTES];
        report[0..2].copy_from_slice(&FIRMWARE_ANSWER);
        report[FIRMWARE_OFFSET] = 2;
        report[FIRMWARE_OFFSET + 1] = 0;
        report[FIRMWARE_OFFSET + 2] = 4;
        assert_eq!(firmware(&report), "2.0.4");
        assert_eq!(firmware_major("2.0.4"), Some(2));
        assert_eq!(firmware_major("1.5.0"), Some(1));
        assert_eq!(firmware_major("unknown"), None);
        assert_eq!(firmware_major(""), None);
    }

    /// A device that answers with a fixed script, in a fixed order.
    struct Scripted {
        sent: Vec<[u8; 2]>,
        answers: std::collections::VecDeque<[u8; REPORT_BYTES]>,
        reads: usize,
    }

    impl Scripted {
        fn new(answers: Vec<[u8; 2]>) -> Self {
            Self {
                sent: Vec::new(),
                answers: answers
                    .into_iter()
                    .map(|identifier| {
                        let mut report = [0u8; REPORT_BYTES];
                        report[0..2].copy_from_slice(&identifier);
                        report
                    })
                    .collect(),
                reads: 0,
            }
        }
    }

    impl HidTransport for Scripted {
        fn write_report(&mut self, report: &[u8; REPORT_BYTES]) -> Result<(), HidError> {
            self.sent.push([report[0], report[1]]);
            Ok(())
        }

        fn read_report(&mut self, _: Duration) -> Result<Option<[u8; REPORT_BYTES]>, HidError> {
            self.reads += 1;
            Ok(self.answers.pop_front())
        }

        fn source(&self) -> String {
            "scripted".to_string()
        }
    }

    const A: Exchange = ([0x10, 0x01], [0x11, 0x01]);
    const B: Exchange = ([0x20, 0x03], [0x21, 0x03]);

    #[test]
    fn every_request_leaves_before_the_first_answer_is_read() {
        // The slow answer costs most of a second on the real controller, so the
        // questions are asked together rather than one after the other.
        let mut device = Scripted::new(vec![[0x11, 0x01], [0x21, 0x03]]);
        ask(&mut device, [A, B]).unwrap();
        assert_eq!(device.sent, vec![[0x10, 0x01], [0x20, 0x03]]);
    }

    #[test]
    fn answers_land_in_their_own_slot_whatever_order_they_arrive_in() {
        // The controller answers its topology hundreds of milliseconds after
        // its firmware, and interleaves unrelated status reports meanwhile.
        let mut device = Scripted::new(vec![[0xff, 0x00], [0x21, 0x03], [0x11, 0x01]]);
        let [first, second] = ask(&mut device, [A, B]).unwrap();
        assert!(answers(&first.unwrap(), A.1), "the firmware slot");
        assert!(answers(&second.unwrap(), B.1), "the topology slot");
    }

    #[test]
    fn an_answer_that_never_arrives_leaves_its_slot_empty_without_losing_the_other() {
        let mut device = Scripted::new(vec![[0x11, 0x01]]);
        let [firmware, topology] = ask(&mut device, [A, B]).unwrap();
        assert!(firmware.is_some());
        assert!(topology.is_none());
    }

    #[test]
    fn a_device_that_streams_noise_cannot_turn_a_query_into_an_endless_loop() {
        let mut device = Scripted::new(vec![[0xff, 0x00]; MAX_QUERY_READS * 4]);
        let [firmware, topology] = ask(&mut device, [A, B]).unwrap();
        assert!(firmware.is_none() && topology.is_none());
        assert_eq!(device.reads, MAX_QUERY_READS);
    }

    #[test]
    fn the_answer_window_clears_the_measured_topology_latency() {
        // Measured on the owned RGB controller: 518-699 ms for the topology
        // answer. The window must sit above that with room, or a probe reports
        // a silent controller for one that was still answering. It belongs to
        // the transport because both devices are read through it.
        assert!(
            ANSWER_TIMEOUT >= Duration::from_millis(1_500),
            "{ANSWER_TIMEOUT:?} is too close to the observed 699 ms"
        );
    }

    #[test]
    fn reading_stops_as_soon_as_every_answer_is_in() {
        let mut device = Scripted::new(vec![[0x11, 0x01], [0x21, 0x03], [0xff, 0x00]]);
        ask(&mut device, [A, B]).unwrap();
        assert_eq!(device.reads, 2, "the trailing status report is never read");
    }
}
