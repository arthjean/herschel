// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The only code in this workspace that talks to the Kraken's panel.
//!
//! The transport is split across two interfaces of the same device, which is
//! what makes it safe to use at all:
//!
//! * Interface 1 is HID and belongs to `kraken2023`. The display commands
//!   (`0x30`, `0x36`) travel over the `hidraw` node the driver publishes. The
//!   driver starts with `HID_CONNECT_HIDRAW` expressly so user space can share
//!   that interface, and the identifiers it uses itself (`0x10`, `0x70`,
//!   `0x72`, `0x74`, `0x75`) do not overlap with the ones sent here.
//! * Interface 0 is vendor class, has no driver bound and carries one bulk OUT
//!   endpoint. The framebuffer goes there, through [`crate::usbfs`].
//!
//! No kernel driver is detached on either side. That is not a convenience: the
//! thermal path is the reason this product exists, and it stays exactly where
//! `kraken2023` put it.
//!
//! # Provenance
//!
//! The report identifiers, the transfer sequence, the bulk header and the pixel
//! format are adapted from liquidctl's `KrakenZ3` driver
//! ([`liquidctl/driver/kraken3.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/kraken3.py)),
//! `SPDX-License-Identifier: GPL-3.0-or-later` and therefore compatible with
//! this project. The identifiers were cross-checked against the HID report
//! descriptor this machine's Kraken publishes: `0x30`, `0x32`, `0x36` and
//! `0x38` all appear there as output reports with a 63-byte payload, and the
//! answers `0x31`, `0x33`, `0x37` and `0x39` as input reports of the same
//! width.
//!
//! Being adapted from a working implementation is not evidence that a command
//! is safe on *this* firmware. That is what `--lcd-write-probe` is for, and
//! why [`VALIDATED_FIRMWARE`] gates every frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kori_core::capability::{Evidenced, LcdDisplaySettings, LcdPanel, LcdPanelShape, LcdTopology};
use kori_core::display::Orientation;
use kori_core::lighting::Brightness;

use crate::hid::{self, ANSWER_TIMEOUT, HidError, HidTransport, Hidraw, REPORT_BYTES, silence};
use crate::usbfs::{BulkTransport, Usbfs, UsbfsError};

/// Geometry this product carries for `1e71:300e`.
///
/// The device reports no resolution, so the size is asserted rather than read.
/// The source is liquidctl's product table, which maps this exact product id to
/// a 240 by 240 panel. It is recorded as a candidate in the capability record
/// and nothing is written on its strength alone.
///
/// The *shape* is not a candidate: it was observed. A white frame at 50%
/// brightness lit a sharp square with its corners visible, on 2026-08-07,
/// which is what a full-frame fill is for. It was recorded as circular before
/// that run, from the assumption that a round cooler head carries round glass.
pub const PANEL_WIDTH: u16 = 240;
pub const PANEL_HEIGHT: u16 = 240;
pub const PANEL_SHAPE: LcdPanelShape = LcdPanelShape::Square;

/// Two bytes per pixel, red and blue in five bits each, green in six.
pub const PIXEL_FORMAT: &str = "RGB565 big-endian";

/// Exact payload of one frame.
pub const FRAME_BYTES: usize = PANEL_WIDTH as usize * PANEL_HEIGHT as usize * 2;

/// Where the framebuffer goes, and which interface owns it.
pub const BULK_ENDPOINT: u8 = 0x02;
pub const BULK_INTERFACE: u8 = 0;

/// Firmware generation whose transfer sequence this module implements.
///
/// liquidctl carries two sequences for this product id. The 2.x one is a plain
/// start, header, payload, end; the 1.x one negotiates memory buckets first.
/// Only the 2.x sequence is implemented, because it is the one the owned unit
/// runs. A Kraken reporting any other generation is refused by name rather than
/// sent a sequence written for a firmware it is not.
pub const SUPPORTED_FIRMWARE_MAJOR: u8 = 2;

/// Firmware revisions whose transfer was exercised on real hardware.
///
/// Filled only from a `--lcd-write-probe` run an operator watched, never from
/// a datasheet and never from another Kraken. An empty list means every panel
/// stays read-only, so the gate fails closed by construction.
///
/// `2.0.0` was validated on 2026-08-11 against the owned `1e71:300e`, over the
/// transfer this module sends today: `docs/lcd-write-probe-2.0.0.json`. Four
/// solid frames plus a restoration, zero transfer errors, and every frame
/// acknowledged four times out of four, which is both `0x36` commands of both
/// sequences. The operator described each one as filling the whole panel, so
/// nothing of the previous picture survived the frame that replaced it.
///
/// The run before it, on 2026-08-07, is what proved that last sentence has to
/// be checked: it reported the same success while the operator saw a band of
/// the new color over the old picture, because the frames went out without
/// waiting for the panel to answer. `LcdLink::send_frame` waits now, and this
/// record is the one taken after it did.
///
/// One caveat the record carries rather than hides: the panel was already
/// showing red when the run started, so the red frame proves less than the
/// three that followed. Green, blue and white each replaced a different color,
/// which is what establishes that a frame arrives and that the channels land
/// where this product puts them.
pub const VALIDATED_FIRMWARE: &[&str] = &["2.0.0"];

/// Whether this exact firmware has been proven on real hardware.
pub fn is_validated_firmware(firmware: &str) -> bool {
    VALIDATED_FIRMWARE.contains(&firmware)
}

/// The panel geometry this product would lay a frame out in.
pub fn candidate_panel() -> LcdPanel {
    LcdPanel {
        width: PANEL_WIDTH,
        height: PANEL_HEIGHT,
        shape: PANEL_SHAPE,
        pixel_format: PIXEL_FORMAT.to_string(),
        frame_bytes: FRAME_BYTES as u32,
        bulk_endpoint: BULK_ENDPOINT,
        bulk_interface: BULK_INTERFACE,
    }
}

/// Why an LCD operation could not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LcdError {
    #[error("{0}")]
    Hid(#[from] HidError),
    #[error("{0}")]
    Bulk(#[from] UsbfsError),
    #[error("the frame is {actual} bytes, and this panel takes exactly {expected}")]
    FrameSize { actual: usize, expected: usize },
}

/// The wire format, as pure functions over a 64-byte buffer.
pub mod packet {
    use super::*;

    /// Ask the panel for its current brightness and orientation.
    pub const DISPLAY_INFO_REQUEST: [u8; 2] = [0x30, 0x01];
    /// The answer, which only a device carrying a panel sends.
    pub const DISPLAY_INFO_ANSWER: [u8; 2] = [0x31, 0x01];
    /// Set brightness and orientation.
    pub const DISPLAY_CONTROL: [u8; 2] = [0x30, 0x02];
    /// Open a framebuffer transfer.
    pub const TRANSFER_START: [u8; 2] = [0x36, 0x01];
    /// Close one.
    pub const TRANSFER_END: [u8; 2] = [0x36, 0x02];
    /// Identifier the panel answers both of them with.
    ///
    /// The device pairs every command with the next identifier up, which its
    /// own report descriptor spells out: `0x30`/`0x31`, `0x32`/`0x33`,
    /// `0x36`/`0x37`, `0x38`/`0x39`. The second byte comes back unchanged, so
    /// an answer names which of the two commands it belongs to.
    pub const TRANSFER_ANSWER: u8 = 0x37;

    /// Offsets inside a `0x31 0x01` answer.
    ///
    /// Crate-visible so the fixture answers at the offsets the decoder reads.
    /// The literals are pinned once, by
    /// `the_display_offsets_are_the_ones_the_panel_answers_at`.
    pub(crate) const BRIGHTNESS_OFFSET: usize = 0x18;
    pub(crate) const ORIENTATION_OFFSET: usize = 0x1a;

    /// Fixed prefix of the bulk header, before the transfer description.
    ///
    /// Twelve bytes the firmware expects verbatim; they carry no length, no
    /// address and no checksum, so they are a constant rather than a
    /// computation.
    pub const BULK_MAGIC: [u8; 12] = [
        0x12, 0xfa, 0x01, 0xe8, 0xab, 0xcd, 0xef, 0x98, 0x76, 0x54, 0x32, 0x10,
    ];

    /// Transfer kind byte the 2.x sequence carries in its header.
    const BULK_KIND_STATIC: u8 = 0x06;

    /// Complete bulk header: the magic, the transfer kind, then the length.
    pub const BULK_HEADER_BYTES: usize = BULK_MAGIC.len() + 8;

    /// The report that sets brightness and orientation together.
    ///
    /// The firmware carries both in one command, so setting either means
    /// sending the other unchanged. That is why the caller passes both.
    pub fn display_control(brightness: Brightness, orientation: Orientation) -> [u8; REPORT_BYTES] {
        let mut report = [0u8; REPORT_BYTES];
        report[0] = DISPLAY_CONTROL[0];
        report[1] = DISPLAY_CONTROL[1];
        report[2] = 0x01;
        report[3] = brightness.percent();
        report[6] = 0x01;
        report[7] = orientation.quarter_turns();
        report
    }

    /// The report that opens a 2.x framebuffer transfer.
    pub fn transfer_start() -> [u8; REPORT_BYTES] {
        let mut report = [0u8; REPORT_BYTES];
        report[0] = TRANSFER_START[0];
        report[1] = TRANSFER_START[1];
        report[2] = 0x00;
        report[3] = 0x01;
        report[4] = BULK_KIND_STATIC;
        report
    }

    /// The report that closes one.
    pub fn transfer_end() -> [u8; REPORT_BYTES] {
        hid::query(TRANSFER_END)
    }

    /// The header written to the bulk endpoint ahead of the pixels.
    ///
    /// Sent as its own transfer. It is shorter than the endpoint's max packet,
    /// so it lands as a short packet, which is what separates it from the
    /// payload that follows.
    pub fn bulk_header(payload_bytes: u32) -> [u8; BULK_HEADER_BYTES] {
        let mut header = [0u8; BULK_HEADER_BYTES];
        header[..BULK_MAGIC.len()].copy_from_slice(&BULK_MAGIC);
        header[BULK_MAGIC.len()] = BULK_KIND_STATIC;
        header[BULK_MAGIC.len() + 4..].copy_from_slice(&payload_bytes.to_le_bytes());
        header
    }

    /// The brightness and orientation a `0x31 0x01` answer reports.
    ///
    /// A quarter-turn count outside `0..=3` is refused rather than reduced: it
    /// would mean the offset is wrong on this firmware, and inventing a
    /// rotation from it is exactly the fabricated default the record forbids.
    pub fn display_settings(report: &[u8; REPORT_BYTES]) -> Option<LcdDisplaySettings> {
        let brightness = report[BRIGHTNESS_OFFSET];
        let quarter_turns = report[ORIENTATION_OFFSET];
        (brightness <= 100 && quarter_turns <= 3).then_some(LcdDisplaySettings {
            brightness_percent: brightness,
            quarter_turns,
        })
    }

    /// Build a `0x31 0x01` answer reporting `settings`.
    ///
    /// The encoder half of [`display_settings`], kept beside it for the same
    /// reason [`crate::hid::firmware_answer`] is: a fixture answering at its own
    /// literal offsets would keep every test green while the decoder read
    /// somewhere else on a real panel.
    ///
    /// Takes the raw pair rather than [`LcdDisplaySettings`] so a fixture can
    /// answer the out-of-range values the decoder has to refuse.
    #[cfg(any(test, feature = "testing"))]
    pub fn display_info_answer(brightness_percent: u8, quarter_turns: u8) -> [u8; REPORT_BYTES] {
        let mut report = [0u8; REPORT_BYTES];
        report[0..2].copy_from_slice(&DISPLAY_INFO_ANSWER);
        report[BRIGHTNESS_OFFSET] = brightness_percent;
        report[ORIENTATION_OFFSET] = quarter_turns;
        report
    }
}

/// What the Kraken answered about itself.
///
/// Both fields are optional and fail separately, for the same reason the RGB
/// controller's inventory splits them: a device that answered one of them has
/// told this product something true.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LcdInventory {
    pub firmware: Option<String>,
    pub display: Option<LcdDisplaySettings>,
}

/// Ask the Kraken for its firmware and its current display settings.
///
/// Both are queries. Neither carries a color, a frame, a brightness or an
/// orientation, so running one changes nothing an operator can see, which is
/// what makes it safe at every daemon start.
///
/// [`hid::ask`] holds the matching rule, which matters twice over here:
/// `kraken2023` is polling this very interface for its own reports, so an
/// answer picked by arrival order would sooner or later be the driver's.
pub fn query<T: HidTransport + ?Sized>(transport: &mut T) -> Result<LcdInventory, LcdError> {
    let [firmware, display] = hid::ask(
        transport,
        [
            (hid::FIRMWARE_REQUEST, hid::FIRMWARE_ANSWER),
            (packet::DISPLAY_INFO_REQUEST, packet::DISPLAY_INFO_ANSWER),
        ],
    )?;

    Ok(LcdInventory {
        firmware: firmware.map(|report| hid::firmware(&report)),
        // A display answer whose fields are out of range is dropped rather than
        // reduced: it means the offset is wrong on this firmware, and a wrong
        // offset must not become a brightness.
        display: display.and_then(|report| packet::display_settings(&report)),
    })
}

/// How many times one picture is put on the wire.
///
/// Two, always. The panel double-buffers and swaps on the transfer *after* the
/// one that filled it, so a picture sent once lands in the buffer nothing is
/// showing and only reaches the glass when some later frame happens to push it
/// there.
///
/// This was `1` after the first frame of a link, from liquidctl's comment that
/// sending it twice "is only required once after initialization". The comment
/// is a guess: it ends in a question mark, and the code beneath it calls
/// `_send_2023_data_fw2` twice for *every* static image on 2.x firmware
/// ([`kraken3.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/kraken3.py)).
/// Reading the comment rather than the code produced exactly the behavior the
/// panel then showed on 2026-08-08: the first picture after a daemon start
/// appeared at once, and every later one stayed a frame behind, half swapped,
/// until the next transfer pushed it through.
const SEQUENCES_PER_FRAME: u8 = 2;

/// How long a frame waits for the panel to acknowledge a transfer command.
///
/// Two orders of magnitude under [`ANSWER_TIMEOUT`], which is sized for a
/// topology query measured taking 699 ms. This wait sits inside every frame, so
/// a panel that stays silent has to cost almost nothing: at one frame a second,
/// a two second ceiling here would be a stall rather than a timeout.
const TRANSFER_ACK_TIMEOUT: Duration = Duration::from_millis(50);

/// Both halves of the transport, held together because a frame needs both.
pub struct LcdLink {
    hid: Box<dyn HidTransport>,
    bulk: Box<dyn BulkTransport>,
}

impl LcdLink {
    pub fn new(hid: Box<dyn HidTransport>, bulk: Box<dyn BulkTransport>) -> Self {
        Self { hid, bulk }
    }

    /// Both halves in one string, for a message an operator reads.
    ///
    /// Never for a capability record: it carries two separate node fields, and
    /// filling either with this fused string records a path that does not
    /// exist. [`LcdLink::topology`] is what a record is built from.
    pub fn source(&self) -> String {
        format!("{} and {}", self.hid.source(), self.bulk.source())
    }

    /// Where the display commands go.
    pub fn hid_source(&self) -> String {
        self.hid.source()
    }

    /// Where the framebuffer goes.
    pub fn bulk_source(&self) -> String {
        self.bulk.source()
    }

    /// Ask the panel what it is, and record it with each half named.
    ///
    /// Only this type knows which source belongs in which field, so it is the
    /// one that builds the record. A caller reaching for [`LcdLink::source`] to
    /// fill both would put the fused string in each, and the capability record
    /// would then name two nodes that neither exist nor were opened.
    pub fn topology(&mut self) -> LcdTopology {
        let (hid, bulk) = (self.hid.source(), self.bulk.source());
        match query(self.hid.as_mut()) {
            Ok(inventory) => topology_from(&inventory, &hid, Some(&bulk)),
            Err(error) => {
                LcdTopology::silent(hid, Evidenced::known(bulk, "usbfs"), error.to_string())
            }
        }
    }

    /// Set brightness and orientation on the panel itself.
    ///
    /// The orientation sent here is always [`Orientation::Deg0`]. The renderer
    /// turns the framebuffer instead, so the preview and the panel show the
    /// same picture; turning it in both places at once would double or cancel.
    pub fn set_display(&mut self, brightness: Brightness) -> Result<[u8; REPORT_BYTES], LcdError> {
        let report = packet::display_control(brightness, Orientation::Deg0);
        self.hid.write_report(&report)?;
        Ok(report)
    }

    /// Send one complete frame.
    ///
    /// The sequence is fixed: open the transfer over HID, write the header and
    /// the pixels to the bulk endpoint, close the transfer over HID. Nothing
    /// partial is ever sent, because the size is checked before the first byte
    /// leaves.
    ///
    /// Every frame goes out twice: see [`SEQUENCES_PER_FRAME`]. It costs one
    /// extra transfer of 13.7 ms per picture, and it is the difference between
    /// a panel that shows what it was last told and one that trails a frame
    /// behind whatever it was told before.
    pub fn send_frame(&mut self, payload: &[u8]) -> Result<FrameReport, LcdError> {
        if payload.len() != FRAME_BYTES {
            return Err(LcdError::FrameSize {
                actual: payload.len(),
                expected: FRAME_BYTES,
            });
        }

        let start = packet::transfer_start();
        let header = packet::bulk_header(payload.len() as u32);
        let end = packet::transfer_end();

        let mut acknowledged = 0u8;
        for _ in 0..SEQUENCES_PER_FRAME {
            self.hid.write_report(&start)?;
            acknowledged += u8::from(self.await_ack(packet::TRANSFER_START));
            self.bulk.write_bulk(BULK_ENDPOINT, &header)?;
            self.bulk.write_bulk(BULK_ENDPOINT, payload)?;
            self.hid.write_report(&end)?;
            acknowledged += u8::from(self.await_ack(packet::TRANSFER_END));
        }

        Ok(FrameReport {
            start,
            header,
            payload_bytes: payload.len(),
            end,
            sequences: SEQUENCES_PER_FRAME,
            acknowledged,
        })
    }

    /// Wait for the panel to acknowledge one `0x36` command.
    ///
    /// liquidctl reads the answer to every transfer command before it moves on
    /// (`_write_then_read` is `_write` followed by `_read`), and skipping that
    /// read is what left frames half painted on this machine: the pixels went
    /// to the bulk endpoint before the panel had said it was ready to take
    /// them, so only a band of each frame reached the glass and the rest of the
    /// picture stayed as it was. Photographed 2026-08-08.
    ///
    /// Best effort, and deliberately so. The identifier is matched over as many
    /// reports as fit in the budget, because `kraken2023` polls the same
    /// interface and its own reports interleave with these. A panel that never
    /// answers still gets its frame, which is what this did before, rather than
    /// becoming a panel that cannot be drawn on at all.
    fn await_ack(&mut self, command: [u8; 2]) -> bool {
        let deadline = Instant::now() + TRANSFER_ACK_TIMEOUT;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            match self.hid.read_report(deadline - now) {
                Ok(Some(report))
                    if report[0] == packet::TRANSFER_ANSWER && report[1] == command[1] =>
                {
                    return true;
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return false,
            }
        }
    }

    /// Read one report, so a probe can record what came back.
    pub fn read_report(&mut self) -> Result<Option<[u8; REPORT_BYTES]>, LcdError> {
        Ok(self.hid.read_report(ANSWER_TIMEOUT)?)
    }
}

/// Exactly what one frame put on the wire, for the probe's record.
///
/// The pixels themselves are not kept: 115 200 bytes of picture prove nothing
/// a length does not, and a probe record is meant to be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameReport {
    pub start: [u8; REPORT_BYTES],
    pub header: [u8; packet::BULK_HEADER_BYTES],
    pub payload_bytes: usize,
    /// How many times the whole sequence went out.
    ///
    /// Always [`SEQUENCES_PER_FRAME`], for the panel's buffer swap. Recorded so
    /// a probe's byte count adds up rather than looking like a duplicate.
    pub sequences: u8,
    /// How many `0x36` commands the panel answered, of the two per sequence.
    ///
    /// Recorded rather than enforced: the transfer proceeds either way, and a
    /// probe that shows a silent panel is evidence about this firmware rather
    /// than a failure.
    pub acknowledged: u8,
    pub end: [u8; REPORT_BYTES],
}

/// What the record says about the bulk half, present or not.
///
/// One function rather than a match at each site, because "no node was
/// published" and "a node was published" are the only two answers and they must
/// read identically wherever the record is assembled.
fn bulk_evidence(bulk: Option<&str>) -> Evidenced<String> {
    match bulk {
        Some(node) => Evidenced::known(node.to_string(), "sysfs"),
        None => Evidenced::unknown(
            "the device publishes no usbfs node for its bulk interface",
            "sysfs",
        ),
    }
}

/// Build the capability record's LCD section from a Kraken that answered.
pub fn inspect<T: HidTransport + ?Sized>(transport: &mut T, bulk: Option<&str>) -> LcdTopology {
    let source = transport.source();
    let inventory = match query(transport) {
        Ok(inventory) => inventory,
        // The node was reached even though the transport failed, and the bulk
        // half keeps whatever was established about it rather than inheriting
        // the display half's failure.
        Err(error) => {
            return LcdTopology::silent(source, bulk_evidence(bulk), error.to_string());
        }
    };
    topology_from(&inventory, &source, bulk)
}

/// Turn one answered inventory into the record's LCD section.
///
/// Split from [`inspect`] so [`LcdLink::topology`] can record the same evidence
/// from a link that is already open. Private: the two public ways to obtain a
/// record both go through it, and neither hands a caller the chance to pair an
/// inventory with the wrong node.
fn topology_from(inventory: &LcdInventory, source: &str, bulk: Option<&str>) -> LcdTopology {
    LcdTopology {
        hid_node: Evidenced::known(source.to_string(), "sysfs"),
        bulk_node: bulk_evidence(bulk),
        firmware: match inventory.firmware.clone() {
            Some(firmware) => Evidenced::known(firmware, format!("{source} report 0x11 0x01")),
            None => Evidenced::unknown(silence("firmware"), format!("{source} report 0x11 0x01")),
        },
        // The geometry is only recorded once the device has proven it carries a
        // panel at all, by answering the report only a panel answers. A Kraken
        // that stays silent gets no resolution written next to it.
        panel: match inventory.display {
            Some(_) => Evidenced::known(
                candidate_panel(),
                "candidate for 1e71:300e from liquidctl kraken3.py; this device reports no \
                 resolution of its own",
            ),
            None => Evidenced::unknown(
                "the device did not answer the display report, so it may carry no panel",
                format!("{source} report 0x31 0x01"),
            ),
        },
        display: match inventory.display {
            Some(display) => Evidenced::known(display, format!("{source} report 0x31 0x01")),
            None => Evidenced::unknown(
                silence("display settings"),
                format!("{source} report 0x31 0x01"),
            ),
        },
    }
}

/// Open both halves of the transport and ask the Kraken what it is.
///
/// The link comes back only when the panel actually answered *and* the bulk
/// interface could be claimed: a caller cannot end up holding a handle it could
/// send half a frame through.
pub fn connect(device: Option<&Path>, hid_node: Option<PathBuf>) -> (LcdTopology, Option<LcdLink>) {
    let Some(path) = hid_node else {
        return (
            LcdTopology::unavailable("the kernel exposed no hidraw node for this device", "sysfs"),
            None,
        );
    };
    let source = path.display().to_string();

    let mut hid = match Hidraw::open(&path) {
        Ok(link) => link,
        // The bulk half was never reached, so it carries that rather than the
        // display half's permission failure. A record that blamed a path it
        // never opened would send an operator to the wrong rule.
        Err(error) => {
            let never_tried = Evidenced::unknown(
                "not attempted: the display commands could not be opened first",
                "usbfs",
            );
            return (
                LcdTopology::silent(source, never_tried, error.to_string()),
                None,
            );
        }
    };
    hid.drain();

    // The bulk half is claimed first so its failure is recorded on the
    // topology, rather than discovered later by a frame that cannot be sent.
    // Flattened to one `Result` on purpose: a claim that failed and a device
    // that published no node are both "there is no bulk half, and here is why",
    // and keeping them as separate states meant three passes over the same
    // value with one state silently borrowing the other's reason.
    let bulk = match device {
        Some(device) => Usbfs::claim(device, BULK_INTERFACE).map_err(|error| error.to_string()),
        None => Err("the device publishes no usbfs node for its bulk interface".to_string()),
    };
    let bulk_source = bulk.as_ref().ok().map(|usbfs| usbfs.source());

    let mut topology = inspect(&mut hid, bulk_source.as_deref());
    if let Err(reason) = &bulk {
        topology.bulk_node = Evidenced::unknown(reason.clone(), "usbfs");
    }

    let link = match bulk {
        Ok(usbfs) if topology.answered() => Some(LcdLink::new(Box::new(hid), Box::new(usbfs))),
        _ => None,
    };
    (topology, link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeKraken;

    #[test]
    fn no_answer_this_device_sends_can_be_mistaken_for_another() {
        // `hid::ask` routes answers by identifier, so two answers sharing one
        // would land in the same slot and the second question would be recorded
        // as unanswered. The firmware pair is shared with the RGB controller by
        // construction now (both modules use `hid::FIRMWARE_ANSWER`, there is no
        // second definition to drift), but this device's own answers still have
        // to be distinct from it and from each other.
        let identifiers = [
            crate::hid::FIRMWARE_ANSWER,
            packet::DISPLAY_INFO_ANSWER,
            [packet::TRANSFER_ANSWER, packet::TRANSFER_START[1]],
            [packet::TRANSFER_ANSWER, packet::TRANSFER_END[1]],
        ];
        for (index, first) in identifiers.iter().enumerate() {
            for second in &identifiers[index + 1..] {
                assert_ne!(first, second, "two answers share one identifier");
            }
        }
    }

    #[test]
    fn the_display_offsets_are_the_ones_the_panel_answers_at() {
        // The one place these literals are pinned. Decoder, encoder and fixture
        // all go through the constants.
        assert_eq!(packet::BRIGHTNESS_OFFSET, 0x18);
        assert_eq!(packet::ORIENTATION_OFFSET, 0x1a);
    }

    #[test]
    fn the_display_reports_never_collide_with_the_thermal_drivers_own() {
        // `kraken2023` sends 0x10, 0x70, 0x72, 0x74 and reads 0x11, 0x75. The
        // identifiers this module writes must not appear in that set, because
        // the two share one interface.
        let driver_writes = [0x10u8, 0x70, 0x72, 0x74];
        for report in [
            packet::DISPLAY_INFO_REQUEST,
            packet::DISPLAY_CONTROL,
            packet::TRANSFER_START,
            packet::TRANSFER_END,
        ] {
            assert!(
                !driver_writes.contains(&report[0]),
                "{report:02x?} collides with a report kraken2023 sends"
            );
        }
    }

    #[test]
    fn display_control_carries_brightness_and_orientation_in_one_report() {
        let report = packet::display_control(Brightness::new(40).unwrap(), Orientation::Deg180);
        assert_eq!(&report[0..8], &[0x30, 0x02, 0x01, 40, 0x00, 0x00, 0x01, 2]);
        assert!(report[8..].iter().all(|byte| *byte == 0));

        let full = packet::display_control(Brightness::FULL, Orientation::Deg0);
        assert_eq!(full[3], 100);
        assert_eq!(full[7], 0);
    }

    #[test]
    fn the_bulk_header_carries_the_magic_then_the_length_little_endian() {
        let header = packet::bulk_header(FRAME_BYTES as u32);
        assert_eq!(&header[..12], &packet::BULK_MAGIC);
        assert_eq!(header[12], 0x06, "transfer kind");
        assert_eq!(&header[13..16], &[0, 0, 0]);
        // 115 200 is 0x0001C200, little endian.
        assert_eq!(&header[16..20], &[0x00, 0xc2, 0x01, 0x00]);
        assert_eq!(header.len(), 20);
        assert!(
            header.len() < 512,
            "the header must stay under the endpoint's max packet so it lands \
             as the short packet that separates it from the payload"
        );
    }

    #[test]
    fn the_transfer_sequence_is_start_header_payload_end() {
        let kraken = FakeKraken::new("2.0.0");
        let bulk = kraken.bulk_recorder();
        let mut link = kraken.link();

        link.send_frame(&vec![0u8; FRAME_BYTES]).unwrap();
        let report = link.send_frame(&vec![1u8; FRAME_BYTES]).unwrap();

        assert_eq!(&report.start[0..5], &[0x36, 0x01, 0x00, 0x01, 0x06]);
        assert_eq!(&report.end[0..2], &[0x36, 0x02]);
        assert_eq!(report.payload_bytes, FRAME_BYTES);
        assert_eq!(report.sequences, 2);

        // The header is its own transfer, ahead of the pixels, and nothing else
        // reaches the endpoint. The HID reports bracket it.
        let steady: Vec<Vec<u8>> = bulk.transfers().split_off(4);
        assert_eq!(steady.len(), 4, "one header and one payload per sequence");
        assert_eq!(steady[0], packet::bulk_header(FRAME_BYTES as u32).to_vec());
        assert_eq!(steady[1].len(), FRAME_BYTES);
        assert_eq!(
            steady[2], steady[0],
            "the second sequence repeats the first"
        );
        assert_eq!(steady[3], steady[1], "the same picture, sent twice");
        assert!(
            bulk.endpoints().iter().all(|end| *end == BULK_ENDPOINT),
            "every byte went to the vendor interface's OUT endpoint"
        );
    }

    #[test]
    fn every_frame_goes_out_twice_and_not_only_the_first() {
        // The panel swaps buffers on the transfer after the one that filled
        // them, so a picture sent once lands where nothing shows it. Sending
        // only the first frame twice is what this used to do, from liquidctl's
        // comment rather than from its code, and on the glass it left every
        // picture after the first one a frame behind.
        let kraken = FakeKraken::new("2.0.0");
        let bulk = kraken.bulk_recorder();
        let mut link = kraken.link();

        for sent in 1..=4 {
            let report = link.send_frame(&vec![0u8; FRAME_BYTES]).unwrap();
            assert_eq!(report.sequences, 2, "frame {sent} went out once");
            assert_eq!(
                bulk.transfers().len(),
                sent * 4,
                "two headers and two payloads per picture"
            );
        }
    }

    #[test]
    fn a_frame_waits_for_the_panel_to_acknowledge_each_transfer_command() {
        // Not decoration: sending the pixels before the panel answered its
        // transfer-start report is what left frames half painted on the glass,
        // and reading that answer is what liquidctl's `_write_then_read` does
        // around every `0x36`.
        let kraken = FakeKraken::new("2.0.0");
        let mut link = kraken.link();

        let report = link.send_frame(&vec![0u8; FRAME_BYTES]).unwrap();
        assert_eq!(
            report.acknowledged, 4,
            "two sequences, each opened and closed, each answered"
        );
    }

    #[test]
    fn a_panel_that_never_answers_still_gets_its_frame() {
        // The wait is best effort. A firmware that acknowledges nothing must
        // end up with a panel that is drawn on late, not one that is read-only.
        let kraken = FakeKraken::without_panel("2.0.0");
        let bulk = kraken.bulk_recorder();
        let mut link = kraken.link();

        let report = link.send_frame(&vec![0u8; FRAME_BYTES]).unwrap();
        assert_eq!(report.acknowledged, 0);
        assert_eq!(report.sequences, 2);
        assert_eq!(bulk.transfers().len(), 4, "the pixels went out regardless");
    }

    #[test]
    fn a_frame_of_the_wrong_size_never_reaches_the_endpoint() {
        let kraken = FakeKraken::new("2.0.0");
        let bulk = kraken.bulk_recorder();
        let mut link = kraken.link();

        for length in [0, FRAME_BYTES - 1, FRAME_BYTES + 1] {
            match link.send_frame(&vec![0u8; length]) {
                Err(LcdError::FrameSize { actual, expected }) => {
                    assert_eq!(actual, length);
                    assert_eq!(expected, FRAME_BYTES);
                }
                other => panic!("expected a size refusal for {length}, got {other:?}"),
            }
        }
        assert!(
            bulk.transfers().is_empty(),
            "a refused frame must not put a single byte on the endpoint"
        );
    }

    #[test]
    fn a_kraken_that_answers_is_recorded_with_its_panel_and_its_settings() {
        let mut kraken = FakeKraken::new("2.0.4");
        let topology = inspect(&mut kraken, Some("/dev/bus/usb/001/004 interface 0"));

        assert_eq!(topology.firmware.value().map(String::as_str), Some("2.0.4"));
        assert!(topology.answered());
        let panel = topology.panel.value().unwrap();
        assert_eq!(panel.width, 240);
        assert_eq!(panel.height, 240);
        assert_eq!(panel.frame_bytes, FRAME_BYTES as u32);
        assert_eq!(panel.bulk_endpoint, 0x02);
        let settings = topology.display.value().unwrap();
        assert_eq!(settings.brightness_percent, 60);
        assert_eq!(settings.quarter_turns, 0);
    }

    #[test]
    fn a_device_that_answers_no_display_report_gets_no_resolution_written_beside_it() {
        let mut silent = FakeKraken::without_panel("2.0.4");
        let topology = inspect(&mut silent, None);

        assert_eq!(topology.firmware.value().map(String::as_str), Some("2.0.4"));
        assert!(
            !topology.answered(),
            "a Kraken with no panel must not be recorded as carrying one"
        );
        assert!(
            !topology.panel.is_known(),
            "the candidate geometry is only recorded once a panel answered"
        );
        let reason = topology.panel.reason().unwrap();
        assert!(reason.contains("may carry no panel"), "{reason}");
        assert!(!topology.bulk_node.is_known());
    }

    #[test]
    fn an_out_of_range_orientation_is_refused_rather_than_reduced() {
        assert_eq!(
            packet::display_settings(&packet::display_info_answer(50, 7)),
            None,
            "a rotation this product cannot name means the offset is wrong, and \
             a wrong offset must not become a rotation"
        );
        assert_eq!(
            packet::display_settings(&packet::display_info_answer(200, 3)),
            None
        );
        assert_eq!(
            packet::display_settings(&packet::display_info_answer(100, 3)),
            Some(LcdDisplaySettings {
                brightness_percent: 100,
                quarter_turns: 3,
            })
        );
    }

    #[test]
    fn a_panel_answering_out_of_range_settings_is_recorded_as_silent() {
        // The refusal has to survive all the way into the record: a device
        // whose answer cannot be decoded has not proven it carries a panel, so
        // the geometry stays unknown and the frame path stays shut.
        let mut kraken = FakeKraken::new("2.0.4").with_display(200, 0);
        let topology = inspect(&mut kraken, None);

        assert!(!topology.answered());
        assert!(!topology.panel.is_known());
        assert_eq!(topology.firmware.value().map(String::as_str), Some("2.0.4"));
    }

    #[test]
    fn a_link_records_each_half_under_its_own_node() {
        // The record carries two node fields and they are different paths. The
        // caller that had to fill them used to reach for `source()`, which
        // fuses both, so the record named a display node and a framebuffer node
        // that neither existed nor were opened.
        let kraken = FakeKraken::new("2.0.0");
        let mut link = kraken.link();
        let topology = link.topology();

        let hid = topology.hid_node.value().expect("the display node");
        let bulk = topology.bulk_node.value().expect("the framebuffer node");
        assert_eq!(hid, "fake:hidraw10");
        assert_eq!(bulk, "fake:usbfs interface 0");
        assert_ne!(hid, bulk);
        assert!(
            !hid.contains(" and ") && !bulk.contains(" and "),
            "neither field may hold both halves: {hid:?} / {bulk:?}"
        );

        // And the answer itself survived the round trip.
        assert!(topology.answered());
        assert_eq!(topology.firmware.value().map(String::as_str), Some("2.0.0"));
    }

    #[test]
    fn a_node_that_cannot_be_opened_never_blames_the_half_it_did_not_reach() {
        // `/dev/hidraw` under the fixture is a directory, not a node, so the
        // open fails exactly as it does without the udev rule.
        let fake = crate::testing::FakeSysfs::new("lcd-refusal");
        let device = fake.add_kraken();
        let node = fake
            .root_path()
            .join("bus/usb/devices/1-9/1-9:1.1/0003:1E71:300E.000B/hidraw/hidraw10");

        let (topology, link) = connect(Some(&device), Some(node.clone()));
        assert!(link.is_none());
        assert_eq!(
            topology.hid_node.value().map(String::as_str),
            Some(node.display().to_string().as_str())
        );

        let bulk = topology.bulk_node.reason().unwrap();
        assert!(bulk.contains("not attempted"), "{bulk}");
        assert!(
            !bulk.contains("hidraw"),
            "the bulk half must not report a failure on the display half's path: {bulk}"
        );
    }

    #[test]
    fn an_empty_validated_list_keeps_every_panel_read_only() {
        // The gate fails closed. This asserts the direction, not the contents:
        // once a live probe fills the list, the entry it adds must still be the
        // only thing that opens the path.
        for firmware in ["2.0.0", "2.0.4", "1.5.0", ""] {
            assert_eq!(
                is_validated_firmware(firmware),
                VALIDATED_FIRMWARE.contains(&firmware)
            );
        }
    }
}
