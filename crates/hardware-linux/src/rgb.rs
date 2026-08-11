// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The only code in this workspace that talks to the RGB controller.
//!
//! The controller speaks HID on interface 0, bound to `usbhid`, so every
//! transfer goes through the `hidraw` node the kernel already created. No
//! kernel driver is detached and no USB endpoint is claimed directly, which is
//! the same rule the thermal path follows for `kraken2023`.
//!
//! The wire format is split from the transport on purpose. [`packet`] is pure
//! arithmetic over a 64-byte buffer and is proven byte by byte in tests, so the
//! encoding is checked without a controller present. The transport does nothing
//! but move those buffers.
//!
//! # Provenance
//!
//! The report identifiers, the query sequence and the color packet layout are
//! adapted from liquidctl's `Nzxt2023RgbController` driver
//! (`liquidctl/driver/smart_device.py`) and its `Hue2Accessory` table
//! (`liquidctl/util.py`), both `GPL-3.0-or-later` and therefore compatible with
//! this project's license. The report identifiers were cross-checked against
//! the HID report descriptor this machine's controller publishes: every
//! identifier used below appears in it with a 63-byte payload, which is what
//! the 64-byte report plus its identifier byte add up to.
//!
//! Being adapted from a working implementation is not evidence that a command
//! is safe on *this* firmware. That is what `--rgb-write-probe` is for, and
//! why [`VALIDATED_FIRMWARE`] gates every production write.

use std::path::PathBuf;
use std::time::Duration;

use kori_core::capability::{Evidenced, RgbAccessory, RgbChannel, RgbTopology};
use kori_core::lighting::{
    Brightness, EffectDirection, LightingEffect, LightingProgram, MIN_COMMAND_INTERVAL_MS, Rgb,
};

use crate::hid::{self, HidError, HidTransport, Hidraw, REPORT_BYTES, silence};

/// Why a lighting command could not be sent.
///
/// The transport has its own error and it is carried rather than reinterpreted.
/// That separation is not cosmetic: a program this module refuses to encode is
/// not a missing device node, and the reason string reaches the operator
/// verbatim through [`kori_core::ipc::HardwareState::Uncertain`]. An encoding
/// refusal wearing a transport error's message would send someone to check a
/// udev rule for a channel number that was simply out of range.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RgbError {
    #[error("{0}")]
    Transport(#[from] HidError),
    #[error("channel {channel} is not addressable on this controller")]
    UnknownChannel { channel: u8 },
    #[error("the program carries {colors} colors, and one report holds at most {limit}")]
    TooManyColors { colors: usize, limit: usize },
}

/// Channels the controller can address, and therefore the widest bitmask.
pub const MAX_CHANNELS: usize = 3;

/// Accessories the controller reports per channel.
pub const MAX_ACCESSORIES_PER_CHANNEL: usize = 6;

/// Firmware revisions whose command set was exercised on real hardware.
///
/// Filled only from a `--rgb-write-probe` run, never from a datasheet and never
/// from another controller. An empty list means every controller stays
/// read-only, so the gate fails closed by construction.
///
/// `1.5.0` was validated on 2026-08-06 against the owned `1e71:2021`, three
/// channels each carrying one F140 RGB Core. Both scopes ran with zero write
/// errors and the operator confirmed every step:
/// `docs/rgb-write-probe-1.5.0-fixed-and-off.json` covers the fixed color and
/// off, and `docs/rgb-write-probe-1.5.0-with-effects.json` adds Breathing and
/// Spectrum Wave. `docs/rgb-write-probe-1.5.0-effect-sweep.json` covers every
/// speed and direction the Lighting screen can select.
pub const VALIDATED_FIRMWARE: &[&str] = &["1.5.0"];

/// Whether this exact firmware has been proven on real hardware.
pub fn is_validated_firmware(firmware: &str) -> bool {
    VALIDATED_FIRMWARE.contains(&firmware)
}

/// The wire format, as pure functions over a 64-byte buffer.
pub mod packet {
    use super::*;

    /// Report identifiers the controller publishes, all 63 payload bytes wide.
    ///
    /// The firmware pair both devices share lives in [`crate::hid`] and is used
    /// from there. It is deliberately not re-exported: an alias would let a
    /// reader believe this module could carry a different value, and the test
    /// written to catch that drift would be comparing a constant with itself.
    pub const LIGHTING_REQUEST: [u8; 2] = [0x20, 0x03];
    pub const LIGHTING_ANSWER: [u8; 2] = [0x21, 0x03];
    pub const COLOR_COMMAND: [u8; 2] = [0x2a, 0x04];

    /// Offset of the channel count in a `0x21 0x03` answer.
    ///
    /// Crate-visible so the fixture builds its answer at the offset the decoder
    /// reads. The literal is pinned once, by
    /// `the_topology_offsets_are_the_ones_the_controller_uses`.
    pub(crate) const CHANNEL_COUNT_OFFSET: usize = 14;
    /// Offset of the first accessory identifier in a `0x21 0x03` answer.
    pub(crate) const ACCESSORY_OFFSET: usize = 15;

    /// The color command's trailer always occupies the last nine bytes.
    const FOOTER_BYTES: usize = 9;
    const FOOTER_OFFSET: usize = REPORT_BYTES - FOOTER_BYTES;

    /// Largest number of colors that still leaves room for the trailer.
    ///
    /// Header and color block grow from the front, the trailer is pinned to the
    /// back, and the two must not meet. Every exposed effect stays far below
    /// this, and [`color_command`] refuses anything above it rather than
    /// truncating a color the operator chose.
    pub const MAX_COLORS: usize = (FOOTER_OFFSET - 7) / 3;

    /// The channels a `0x21 0x03` answer describes.
    ///
    /// A channel count above what the controller can address is clamped to
    /// [`MAX_CHANNELS`]: the answer is 63 bytes wide, and a larger count would
    /// read accessory identifiers past the end of it.
    pub fn channels(report: &[u8; REPORT_BYTES]) -> Vec<RgbChannel> {
        let source = "hidraw report 0x21 0x03";
        let count = (report[CHANNEL_COUNT_OFFSET] as usize).min(MAX_CHANNELS);
        (0..count)
            .map(|channel| {
                let accessories = (0..MAX_ACCESSORIES_PER_CHANNEL)
                    .map_while(|slot| {
                        let offset =
                            ACCESSORY_OFFSET + channel * MAX_ACCESSORIES_PER_CHANNEL + slot;
                        let id = *report.get(offset)?;
                        // The controller pads a channel's slots with zeros once
                        // its accessories are listed.
                        (id != 0).then_some(RgbAccessory {
                            id,
                            name: accessory_name(id),
                        })
                    })
                    .collect();
                RgbChannel {
                    index: channel as u8 + 1,
                    accessories,
                    led_count: Evidenced::unknown(
                        "the controller reports accessory identifiers, not LED counts",
                        source,
                    ),
                }
            })
            .collect()
    }

    /// Name of an accessory identifier, or an explicit unknown marker.
    ///
    /// Adapted from liquidctl's `Hue2Accessory` table (GPL-3.0-or-later). An
    /// identifier outside it is reported as unknown rather than guessed,
    /// because the name is what the operator uses to tell two channels apart.
    pub fn accessory_name(id: u8) -> String {
        match id {
            0x01 => "HUE+ LED Strip".to_string(),
            0x02 => "AER RGB 1".to_string(),
            0x04 => "HUE 2 LED Strip 300 mm".to_string(),
            0x05 => "HUE 2 LED Strip 250 mm".to_string(),
            0x06 => "HUE 2 LED Strip 200 mm".to_string(),
            0x07 => "HUE 2 Cable Comb".to_string(),
            0x09 => "HUE 2 Underglow 300 mm".to_string(),
            0x0a => "HUE 2 Underglow 200 mm".to_string(),
            0x0b => "AER RGB 2 120 mm".to_string(),
            0x0c => "AER RGB 2 140 mm".to_string(),
            0x10 => "Kraken X Pump Ring".to_string(),
            0x11 => "Kraken X Pump Logo".to_string(),
            0x13 => "F120 RGB".to_string(),
            0x14 => "F140 RGB".to_string(),
            0x17 => "F140 RGB Core".to_string(),
            0x1b => "F240 RGB Core".to_string(),
            0x1d => "F360 RGB Core".to_string(),
            other => format!("Unknown accessory 0x{other:02x}"),
        }
    }

    /// Wire encoding of one program: mode byte, speed pair and trailer flag.
    struct Encoding {
        mode: u8,
        speed: [u8; 2],
        /// Trailer byte the firmware reads as the animation variant.
        variant: u8,
        colors: Vec<Rgb>,
    }

    /// Speed table of an effect, slowest first.
    ///
    /// Values come from liquidctl's `_SPEED_VALUES` for this controller.
    fn speed_values(effect: LightingEffect) -> [[u8; 2]; 5] {
        match effect {
            LightingEffect::Breathing => [
                [0x28, 0x00],
                [0x1e, 0x00],
                [0x14, 0x00],
                [0x0a, 0x00],
                [0x04, 0x00],
            ],
            LightingEffect::SpectrumWave => [
                [0x5e, 0x01],
                [0x2c, 0x01],
                [0xfa, 0x00],
                [0x96, 0x00],
                [0x50, 0x00],
            ],
        }
    }

    fn encode(program: &LightingProgram) -> Encoding {
        match program {
            // Off is the fixed mode carrying one black step: the firmware has
            // no separate blank command, and a zero-length color list would
            // leave the previous animation running.
            LightingProgram::Off => Encoding {
                mode: 0x00,
                speed: [0x00, 0x00],
                variant: 0x00,
                colors: vec![Rgb::BLACK],
            },
            LightingProgram::Fixed { color, brightness } => Encoding {
                mode: 0x00,
                speed: [0x32, 0x00],
                variant: 0x00,
                colors: vec![color.scaled(*brightness)],
            },
            LightingProgram::Effect {
                effect,
                colors,
                brightness,
                speed,
                ..
            } => {
                let table = speed_values(*effect);
                let step = table[speed.index().min(table.len() - 1)];
                match effect {
                    LightingEffect::Breathing => Encoding {
                        mode: 0x07,
                        speed: step,
                        variant: 0x08,
                        colors: colors.iter().map(|c| c.scaled(*brightness)).collect(),
                    },
                    // The wave cycles the whole spectrum, so it carries one
                    // black step the firmware ignores rather than a color the
                    // operator would wrongly believe was used.
                    LightingEffect::SpectrumWave => Encoding {
                        mode: 0x02,
                        speed: step,
                        variant: 0x00,
                        colors: vec![Rgb::BLACK],
                    },
                }
            }
        }
    }

    /// Build the 64-byte color command for one channel bitmask.
    ///
    /// Fails only when the program carries more colors than the report can
    /// hold, which validation rejects long before this point. Refusing is still
    /// the right answer: silently dropping a color would show the operator
    /// something they did not ask for.
    pub fn color_command(
        channel_bit: u8,
        program: &LightingProgram,
    ) -> Result<[u8; REPORT_BYTES], RgbError> {
        let encoding = encode(program);
        if encoding.colors.len() > MAX_COLORS {
            return Err(RgbError::TooManyColors {
                colors: encoding.colors.len(),
                limit: MAX_COLORS,
            });
        }

        let mut report = [0u8; REPORT_BYTES];
        report[0] = COLOR_COMMAND[0];
        report[1] = COLOR_COMMAND[1];
        report[2] = channel_bit;
        report[3] = channel_bit;
        report[4] = encoding.mode;
        report[5] = encoding.speed[0];
        report[6] = encoding.speed[1];

        // The controller orders each triplet green, red, blue.
        for (index, color) in encoding.colors.iter().enumerate() {
            let offset = 7 + index * 3;
            report[offset] = color.g;
            report[offset + 1] = color.r;
            report[offset + 2] = color.b;
        }

        let backward = matches!(
            program,
            LightingProgram::Effect {
                direction: EffectDirection::Backward,
                ..
            }
        );
        report[FOOTER_OFFSET] = if backward { 0x02 } else { 0x00 };
        report[FOOTER_OFFSET + 1] = encoding.colors.len() as u8;
        report[FOOTER_OFFSET + 2] = encoding.variant;
        report[FOOTER_OFFSET + 3] = 0x08;
        report[FOOTER_OFFSET + 4] = 0x03;
        Ok(report)
    }

    /// Bitmask addressing the one-based `channel`.
    pub fn channel_bit(channel: u8) -> Option<u8> {
        (channel >= 1 && (channel as usize) <= MAX_CHANNELS).then(|| 1u8 << (channel - 1))
    }

    /// Build a `0x21 0x03` answer describing one accessory list per channel.
    ///
    /// The encoder half of [`channels`], kept beside it for the same reason
    /// [`crate::hid::firmware_answer`] is kept beside its decoder: a fixture
    /// that laid accessories out at its own literal offsets would agree with
    /// itself forever while the decoder read somewhere else.
    #[cfg(any(test, feature = "testing"))]
    pub fn topology_answer(channels: &[Vec<u8>]) -> [u8; REPORT_BYTES] {
        let mut report = [0u8; REPORT_BYTES];
        report[0..2].copy_from_slice(&LIGHTING_ANSWER);
        report[CHANNEL_COUNT_OFFSET] = channels.len() as u8;
        for (channel, accessories) in channels.iter().enumerate() {
            for (slot, id) in accessories
                .iter()
                .take(MAX_ACCESSORIES_PER_CHANNEL)
                .enumerate()
            {
                let offset = ACCESSORY_OFFSET + channel * MAX_ACCESSORIES_PER_CHANNEL + slot;
                if let Some(byte) = report.get_mut(offset) {
                    *byte = *id;
                }
            }
        }
        report
    }
}

/// What the controller answered about itself.
///
/// Both fields are optional because they fail separately and at very different
/// speeds: the firmware answer lands in milliseconds and the topology answer
/// takes most of a second. A controller that answered one and not the other has
/// told this product something true, and throwing that away to report a single
/// failure would put an `unknown` in the record next to evidence that exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RgbInventory {
    pub firmware: Option<String>,
    pub channels: Option<Vec<RgbChannel>>,
}

/// Ask the controller for its firmware and channel topology.
///
/// Both requests are queries: they carry no color, no mode and no parameter, so
/// running one changes nothing the operator can see. That is what makes this
/// safe to run at every daemon start, which in turn is what lets the Lighting
/// screen refuse a write against evidence read from the device rather than
/// against a constant.
///
/// The error is reserved for a transport that failed. A controller that simply
/// stayed silent returns an inventory with the fields it did answer, so the
/// caller records evidence and absence side by side. [`hid::ask`] holds the
/// rest of the rule, which the Kraken follows identically.
pub fn query<T: HidTransport + ?Sized>(transport: &mut T) -> Result<RgbInventory, RgbError> {
    let [firmware, topology] = hid::ask(
        transport,
        [
            (hid::FIRMWARE_REQUEST, hid::FIRMWARE_ANSWER),
            (packet::LIGHTING_REQUEST, packet::LIGHTING_ANSWER),
        ],
    )?;

    Ok(RgbInventory {
        firmware: firmware.map(|report| hid::firmware(&report)),
        channels: topology.map(|report| packet::channels(&report)),
    })
}

/// Turn one program for one channel into the exact bytes that would be sent.
///
/// The single place a lighting program becomes a report. The write path and the
/// write probe both go through it, so the bytes an operator reads in a probe
/// record are the bytes the daemon sends, rather than a second encoding that
/// happens to agree.
pub fn encode(channel: u8, program: &LightingProgram) -> Result<[u8; REPORT_BYTES], RgbError> {
    let bit = packet::channel_bit(channel).ok_or(RgbError::UnknownChannel { channel })?;
    packet::color_command(bit, program)
}

/// Send one program to one channel.
///
/// The caller is responsible for validation, ownership and cadence. This
/// function encodes and writes, and does nothing else, so there is exactly one
/// place where a lighting byte reaches the controller.
///
/// Nothing is read back. The controller does emit unsolicited status reports on
/// `0xff` while commands are in flight, and the write probe captured them, but
/// none of them carries a channel's current color. There is therefore no
/// readback to compare a command against.
pub fn apply<T: HidTransport + ?Sized>(
    transport: &mut T,
    channel: u8,
    program: &LightingProgram,
) -> Result<[u8; REPORT_BYTES], RgbError> {
    let report = encode(channel, program)?;
    transport.write_report(&report)?;
    Ok(report)
}

/// Build the capability record's RGB section from a controller that answered.
///
/// A controller that answers nothing yields an evidenced unknown rather than an
/// error: the caller records it and every RGB control stays disabled with that
/// reason attached.
pub fn inspect<T: HidTransport + ?Sized>(transport: &mut T) -> RgbTopology {
    let source = transport.source();
    let inventory = match query(transport) {
        Ok(inventory) => inventory,
        // The node was resolved even though the transport failed, so the record
        // keeps that fact instead of losing it.
        Err(error) => return RgbTopology::silent(source, error.to_string()),
    };

    RgbTopology {
        node: Evidenced::known(source.clone(), "sysfs"),
        firmware: match inventory.firmware {
            Some(firmware) => Evidenced::known(firmware, format!("{source} report 0x11 0x01")),
            None => Evidenced::unknown(silence("firmware"), format!("{source} report 0x11 0x01")),
        },
        channels: match inventory.channels {
            Some(channels) => Evidenced::known(channels, format!("{source} report 0x21 0x03")),
            None => Evidenced::unknown(
                silence("lighting topology"),
                format!("{source} report 0x21 0x03"),
            ),
        },
    }
}

/// Open the controller and ask it what it is.
///
/// The transport comes back only when the controller actually answered: a
/// caller cannot end up holding a writable handle to a device whose topology
/// is unknown, which is the state every RGB write is forbidden in.
pub fn connect(node: Option<PathBuf>) -> (RgbTopology, Option<Hidraw>) {
    let Some(path) = node else {
        return (
            RgbTopology::unavailable(
                "the kernel exposed no hidraw node for this controller",
                "sysfs",
            ),
            None,
        );
    };
    let source = path.display().to_string();

    let mut link = match Hidraw::open(&path) {
        Ok(link) => link,
        Err(error) => return (RgbTopology::silent(source, error.to_string()), None),
    };
    link.drain();

    let topology = inspect(&mut link);
    let answered = topology.channels.is_known();
    (topology, answered.then_some(link))
}

/// Slowest cadence the daemon is allowed to command, as a duration.
pub fn minimum_command_interval() -> Duration {
    Duration::from_millis(MIN_COMMAND_INTERVAL_MS)
}

/// The program the write probe applies while it tests a channel.
pub fn probe_program(color: Rgb) -> LightingProgram {
    LightingProgram::Fixed {
        color,
        brightness: Brightness::new(kori_core::lighting::PROBE_BRIGHTNESS)
            .unwrap_or(Brightness::OFF),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::lighting::EffectSpeed;

    fn fixed(color: Rgb, percent: u8) -> LightingProgram {
        LightingProgram::Fixed {
            color,
            brightness: Brightness::new(percent).unwrap(),
        }
    }

    #[test]
    fn the_topology_offsets_are_the_ones_the_controller_uses() {
        // The one place these literals are pinned. The decoder, the encoder and
        // every fixture go through the constants, so this is what fails if an
        // offset ever moves.
        assert_eq!(packet::CHANNEL_COUNT_OFFSET, 14);
        assert_eq!(packet::ACCESSORY_OFFSET, 15);
    }

    #[test]
    fn a_firmware_answer_is_never_mistaken_for_a_topology_answer() {
        // The two answers arrive on the same node, interleaved with unrelated
        // status reports, so telling them apart is what `hid::ask` relies on.
        let report = hid::firmware_answer("1.2.3");
        assert!(hid::answers(&report, hid::FIRMWARE_ANSWER));
        assert!(!hid::answers(&report, packet::LIGHTING_ANSWER));
        assert_eq!(hid::firmware(&report), "1.2.3");

        let topology = packet::topology_answer(&[vec![0x04]]);
        assert!(hid::answers(&topology, packet::LIGHTING_ANSWER));
        assert!(!hid::answers(&topology, hid::FIRMWARE_ANSWER));
    }

    #[test]
    fn the_topology_answer_lists_accessories_per_channel() {
        // Channel 1 carries two strips, channel 2 one fan, channel 3 nothing.
        let report = packet::topology_answer(&[vec![0x04, 0x05], vec![0x0b], vec![]]);

        let channels = packet::channels(&report);
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].index, 1);
        assert_eq!(channels[0].accessories.len(), 2);
        assert_eq!(channels[0].accessories[0].name, "HUE 2 LED Strip 300 mm");
        assert_eq!(channels[1].accessories.len(), 1);
        assert_eq!(channels[1].accessories[0].name, "AER RGB 2 120 mm");
        assert!(channels[2].accessories.is_empty());
        // The controller does not report LED counts, so none is invented.
        assert!(channels.iter().all(|c| !c.led_count.is_known()));
    }

    #[test]
    fn an_unknown_accessory_is_named_as_unknown_rather_than_guessed() {
        let name = packet::accessory_name(0x7f);
        assert!(name.contains("Unknown"), "{name}");
        assert!(name.contains("0x7f"), "{name}");
    }

    #[test]
    fn a_channel_count_beyond_the_controller_cannot_read_past_the_report() {
        let mut report = packet::topology_answer(&[vec![0x04]]);
        report[packet::CHANNEL_COUNT_OFFSET] = 0xff;
        let channels = packet::channels(&report);
        assert_eq!(channels.len(), MAX_CHANNELS);
    }

    #[test]
    fn channel_bits_address_one_channel_each() {
        assert_eq!(packet::channel_bit(1), Some(0b001));
        assert_eq!(packet::channel_bit(2), Some(0b010));
        assert_eq!(packet::channel_bit(3), Some(0b100));
        assert_eq!(packet::channel_bit(0), None);
        assert_eq!(packet::channel_bit(4), None);
    }

    #[test]
    fn a_fixed_color_is_encoded_green_red_blue_at_full_brightness() {
        let report = packet::color_command(0b001, &fixed(Rgb::new(0x10, 0x20, 0x30), 100)).unwrap();
        assert_eq!(&report[0..4], &[0x2a, 0x04, 0x01, 0x01]);
        assert_eq!(report[4], 0x00, "fixed is mode zero");
        assert_eq!(&report[5..7], &[0x32, 0x00]);
        assert_eq!(
            &report[7..10],
            &[0x20, 0x10, 0x30],
            "the controller orders each triplet green, red, blue"
        );
        // Trailer: forward, one color, no variant, then the fixed tail.
        assert_eq!(&report[55..64], &[0x00, 0x01, 0x00, 0x08, 0x03, 0, 0, 0, 0]);
    }

    #[test]
    fn brightness_dims_the_triplet_that_reaches_the_controller() {
        let report = packet::color_command(0b001, &fixed(Rgb::new(200, 100, 50), 50)).unwrap();
        assert_eq!(&report[7..10], &[50, 100, 25]);

        let dark = packet::color_command(0b001, &fixed(Rgb::new(200, 100, 50), 0)).unwrap();
        assert_eq!(&dark[7..10], &[0, 0, 0]);
    }

    #[test]
    fn off_sends_one_black_step_rather_than_no_step() {
        let report = packet::color_command(0b010, &LightingProgram::Off).unwrap();
        assert_eq!(&report[0..4], &[0x2a, 0x04, 0x02, 0x02]);
        assert_eq!(report[4], 0x00);
        assert_eq!(&report[5..7], &[0x00, 0x00]);
        assert_eq!(&report[7..10], &[0x00, 0x00, 0x00]);
        assert_eq!(
            report[56], 0x01,
            "a zero color count would leave the previous animation running"
        );
    }

    #[test]
    fn breathing_carries_its_mode_variant_and_every_color() {
        let program = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![Rgb::new(255, 0, 0), Rgb::new(0, 0, 255)],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Slowest,
            direction: EffectDirection::Forward,
        };
        let report = packet::color_command(0b100, &program).unwrap();
        assert_eq!(report[4], 0x07);
        assert_eq!(&report[5..7], &[0x28, 0x00], "slowest is the first step");
        assert_eq!(&report[7..13], &[0x00, 0xff, 0x00, 0x00, 0x00, 0xff]);
        assert_eq!(report[55], 0x00, "breathing never travels backward");
        assert_eq!(report[56], 0x02, "two colors");
        assert_eq!(report[57], 0x08, "breathing carries the animated variant");
    }

    #[test]
    fn the_speed_step_moves_with_the_selected_speed() {
        let steps: Vec<[u8; 2]> = EffectSpeed::ALL
            .into_iter()
            .map(|speed| {
                let program = LightingProgram::Effect {
                    effect: LightingEffect::SpectrumWave,
                    colors: vec![],
                    brightness: Brightness::FULL,
                    speed,
                    direction: EffectDirection::Forward,
                };
                let report = packet::color_command(0b001, &program).unwrap();
                [report[5], report[6]]
            })
            .collect();
        assert_eq!(
            steps,
            vec![
                [0x5e, 0x01],
                [0x2c, 0x01],
                [0xfa, 0x00],
                [0x96, 0x00],
                [0x50, 0x00]
            ]
        );
        // Slower steps are larger delays, so the table must strictly decrease.
        let as_u16: Vec<u16> = steps
            .iter()
            .map(|step| u16::from(step[0]) | (u16::from(step[1]) << 8))
            .collect();
        assert!(
            as_u16.windows(2).all(|pair| pair[0] > pair[1]),
            "{as_u16:?}"
        );
    }

    #[test]
    fn direction_only_changes_the_trailer_flag() {
        let make = |direction| LightingProgram::Effect {
            effect: LightingEffect::SpectrumWave,
            colors: vec![],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction,
        };
        let forward = packet::color_command(0b001, &make(EffectDirection::Forward)).unwrap();
        let backward = packet::color_command(0b001, &make(EffectDirection::Backward)).unwrap();
        assert_eq!(forward[55], 0x00);
        assert_eq!(backward[55], 0x02);

        let differences = forward
            .iter()
            .zip(backward.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(differences, 1, "direction must not disturb anything else");
    }

    #[test]
    fn the_color_block_can_never_overwrite_the_trailer() {
        // Sixteen colors is the ceiling the report geometry imposes. Every
        // exposed effect stays under it; the encoder still refuses more rather
        // than corrupting the trailer that carries the mode.
        assert_eq!(packet::MAX_COLORS, 16);
        let program = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![Rgb::new(1, 2, 3); packet::MAX_COLORS + 1],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        };
        assert_eq!(
            packet::color_command(0b001, &program),
            Err(RgbError::TooManyColors {
                colors: packet::MAX_COLORS + 1,
                limit: packet::MAX_COLORS,
            })
        );

        let at_ceiling = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![Rgb::new(1, 2, 3); packet::MAX_COLORS],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        };
        let report = packet::color_command(0b001, &at_ceiling).unwrap();
        assert_eq!(
            &report[55..60],
            &[0x00, packet::MAX_COLORS as u8, 0x08, 0x08, 0x03],
            "the trailer survived the widest program"
        );
    }

    #[test]
    fn every_command_this_product_sends_uses_a_published_report_identifier() {
        // Identifiers the controller's own HID report descriptor declares as
        // output or feature reports. A command outside this set would be a
        // report the device never said it accepts.
        const DECLARED_OUTPUT_REPORTS: [u8; 13] = [
            0x10, 0x12, 0x20, 0x22, 0x24, 0x26, 0x2a, 0xf2, 0xfe, 0xe2, 0xf4, 0x2c, 0x1c,
        ];
        for identifier in [
            hid::FIRMWARE_REQUEST,
            packet::LIGHTING_REQUEST,
            packet::COLOR_COMMAND,
        ] {
            assert!(
                DECLARED_OUTPUT_REPORTS.contains(&identifier[0]),
                "0x{:02x} is not declared as an output report",
                identifier[0]
            );
        }
    }

    #[test]
    fn a_controller_that_answers_only_its_firmware_keeps_that_evidence() {
        // The two answers arrive hundreds of milliseconds apart on the real
        // controller. Reporting both as unknown because one was late would put
        // an `unknown` in the record next to a value that was actually read.
        let mut controller = crate::testing::FakeController::new("1.5.0", 3).withholding_topology();
        let topology = inspect(&mut controller);

        assert_eq!(topology.firmware.value().map(String::as_str), Some("1.5.0"));
        assert!(!topology.channels.is_known());
        assert_eq!(topology.channel_count(), None);
        let reason = topology.channels.reason().unwrap_or_default();
        assert!(reason.contains("lighting topology"), "{reason}");
    }

    #[test]
    fn a_controller_that_answers_nothing_records_absence_rather_than_an_error() {
        // The node opened and the queries left the process; the controller
        // simply never answered. That is an evidenced unknown on every field,
        // not a transport failure, and it is what keeps every RGB control
        // disabled with the missing evidence named.
        let mut controller = crate::testing::FakeController::silent();
        let topology = inspect(&mut controller);

        assert!(topology.node.is_known(), "the node was resolved");
        assert!(!topology.firmware.is_known());
        assert!(!topology.channels.is_known());
        assert_eq!(topology.channel_count(), None);
        for reason in [
            topology.firmware.reason().unwrap_or_default(),
            topology.channels.reason().unwrap_or_default(),
        ] {
            assert!(reason.contains("sent no"), "{reason}");
        }
    }

    #[test]
    fn a_controller_that_answers_everything_is_recorded_in_full() {
        let mut controller = crate::testing::FakeController::new("1.5.0", 3)
            .with_accessories(0, vec![0x17])
            .with_accessories(1, vec![0x17])
            .with_accessories(2, vec![0x17]);
        let topology = inspect(&mut controller);

        assert_eq!(topology.channel_count(), Some(3));
        assert_eq!(topology.firmware.value().map(String::as_str), Some("1.5.0"));
        let channels = topology.channels.value().unwrap();
        assert!(
            channels
                .iter()
                .all(|channel| channel.accessories[0].name == "F140 RGB Core")
        );
    }

    #[test]
    fn no_firmware_is_writable_until_a_probe_records_one() {
        // The gate fails closed. Once a write probe fills `VALIDATED_FIRMWARE`,
        // this still holds for anything outside the recorded list.
        assert!(!is_validated_firmware("0.0.0"));
        assert!(!is_validated_firmware(""));
        for firmware in VALIDATED_FIRMWARE {
            assert!(is_validated_firmware(firmware));
        }
    }

    #[test]
    fn the_command_floor_leaves_real_headroom() {
        assert!(minimum_command_interval() >= Duration::from_millis(10));
        assert_eq!(
            minimum_command_interval().as_millis() as u64,
            MIN_COMMAND_INTERVAL_MS
        );
    }

    #[test]
    fn an_encoding_refusal_never_wears_a_transport_errors_message() {
        // This is the whole reason this module carries an error of its own. The
        // string below reaches the operator verbatim through
        // `HardwareState::Uncertain`, so a refusal that called itself a missing
        // node would send someone to check a udev rule for a channel number
        // that was simply out of range.
        let program = fixed(Rgb::new(1, 2, 3), 100);
        let error = encode(4, &program).unwrap_err();
        assert_eq!(error, RgbError::UnknownChannel { channel: 4 });
        let message = error.to_string();
        assert!(message.contains("channel 4"), "{message}");
        assert!(
            !message.contains("hidraw") && !message.contains("udev"),
            "an encoding refusal must not point at the transport: {message}"
        );

        let wide = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![Rgb::new(1, 2, 3); packet::MAX_COLORS + 1],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        };
        let message = encode(1, &wide).unwrap_err().to_string();
        assert!(message.contains("17"), "{message}");
        assert!(!message.contains("hidraw"), "{message}");
    }

    #[test]
    fn a_transport_failure_keeps_the_transports_own_wording() {
        // The other direction: a real permission problem must still name the
        // node and the udev rule, because that one *is* actionable.
        let mut controller = crate::testing::FakeController::new("1.5.0", 3);
        controller.write_failure = Some(crate::hid::HidError::PermissionDenied {
            path: "/dev/hidraw12".to_string(),
        });

        let error = apply(&mut controller, 1, &fixed(Rgb::new(1, 2, 3), 100)).unwrap_err();
        assert!(matches!(error, RgbError::Transport(_)));
        let message = error.to_string();
        assert!(message.contains("/dev/hidraw12"), "{message}");
        assert!(message.contains("udev"), "{message}");
    }

    #[test]
    fn nothing_reaches_the_controller_when_the_program_cannot_be_encoded() {
        let mut controller = crate::testing::FakeController::new("1.5.0", 3);
        assert!(apply(&mut controller, 0, &fixed(Rgb::new(1, 2, 3), 100)).is_err());
        assert_eq!(
            controller.command_count(),
            0,
            "a refused program must not put a byte on the wire"
        );
    }

    #[test]
    fn the_probe_program_is_a_dim_fixed_color() {
        match probe_program(Rgb::new(255, 255, 255)) {
            LightingProgram::Fixed { brightness, .. } => {
                assert_eq!(brightness.percent(), kori_core::lighting::PROBE_BRIGHTNESS);
            }
            other => panic!("the probe must apply a fixed color, got {other:?}"),
        }
    }
}
