// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! What a lighting request means, and everything it must satisfy before a byte
//! can reach the RGB controller.
//!
//! Nothing here knows the wire format. A profile stores a color, a brightness
//! and a named effect, never a packet, so a stored configuration cannot pin the
//! product to one reverse-engineered encoding.
//!
//! Validation lives next to the types so the Lighting screen can disable Apply
//! for the same reason the daemon would refuse the command. The daemon still
//! revalidates: the client is not a trusted input.

use serde::{Deserialize, Serialize};

use crate::capability::CapabilityId;

/// Colors are written as six hexadecimal digits, the form the screen accepts.
pub const HEX_COLOR_DIGITS: usize = 6;

/// Highest brightness percentage the product commands.
pub const MAX_BRIGHTNESS: u8 = 100;

/// Brightness the write probe uses on live hardware.
///
/// The write probe sends a deliberately dim color: enough to see on the strip,
/// far from anything that stresses the controller's current budget.
pub const PROBE_BRIGHTNESS: u8 = 10;

/// Shortest interval the daemon leaves between two commands on one channel.
///
/// The controller acknowledges nothing, so cadence is the only backpressure
/// there is. The floor is measured by the write probe and recorded under
/// `cadence` in `docs/rgb-write-probe-1.5.0-fixed-and-off.json`, which landed
/// five commands at every interval down to 10 ms without an error. This value
/// is deliberately set well above that rather than at it.
pub const MIN_COMMAND_INTERVAL_MS: u64 = 50;

/// A 24-bit color, as the operator typed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse six hexadecimal digits, with or without a leading `#`.
    ///
    /// The error names the exact problem, because the screen shows it on the
    /// field the operator is editing.
    pub fn parse_hex(value: &str) -> Result<Self, LightingError> {
        let trimmed = value.trim().trim_start_matches('#');
        if trimmed.len() != HEX_COLOR_DIGITS {
            return Err(LightingError::ColorDigits {
                expected: HEX_COLOR_DIGITS,
                actual: trimmed.chars().count(),
            });
        }
        let packed =
            u32::from_str_radix(trimmed, 16).map_err(|_| LightingError::ColorAlphabet {
                value: trimmed.to_string(),
            })?;
        Ok(Self {
            r: ((packed >> 16) & 0xff) as u8,
            g: ((packed >> 8) & 0xff) as u8,
            b: (packed & 0xff) as u8,
        })
    }

    /// The six-digit form, without a leading `#`.
    pub fn to_hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// This color at `brightness` percent.
    ///
    /// The controller has no brightness command: the fixed-color packet carries
    /// one triplet and nothing else. Brightness is therefore applied to the
    /// triplet before it is encoded, which is the only place it can be applied
    /// without inventing a field the protocol does not have.
    pub fn scaled(self, brightness: Brightness) -> Self {
        let scale = |channel: u8| {
            ((channel as u16 * brightness.percent() as u16) / MAX_BRIGHTNESS as u16) as u8
        };
        Self {
            r: scale(self.r),
            g: scale(self.g),
            b: scale(self.b),
        }
    }

    /// This color moved `amount` of the way toward `toward`, from 0 to 1.
    ///
    /// The one interpolation the product performs on a color. The renderer
    /// shades a band along its sweep with it, dims a resting track with it, and
    /// mixes coverage into a pixel with it; three copies of the same rounding
    /// are three places for a gradient and an antialiased edge to disagree
    /// about the same pair of colors.
    ///
    /// `amount` is expected inside `0.0..=1.0`. Beyond that the arithmetic
    /// extrapolates and the cast saturates, which is why every caller clamps
    /// before it asks.
    pub fn mixed(self, toward: Self, amount: f32) -> Self {
        let channel = |from: u8, to: u8| {
            (f32::from(from) + (f32::from(to) - f32::from(from)) * amount).round() as u8
        };
        Self {
            r: channel(self.r, toward.r),
            g: channel(self.g, toward.g),
            b: channel(self.b, toward.b),
        }
    }
}

/// A brightness percentage that cannot hold an out-of-range value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Brightness(u8);

impl Brightness {
    pub const FULL: Self = Self(MAX_BRIGHTNESS);
    pub const OFF: Self = Self(0);

    pub fn new(percent: u8) -> Result<Self, LightingError> {
        if percent > MAX_BRIGHTNESS {
            return Err(LightingError::BrightnessOutOfRange {
                value: percent,
                max: MAX_BRIGHTNESS,
            });
        }
        Ok(Self(percent))
    }

    /// A brightness fixed at compile time.
    ///
    /// For a literal the build can check, so an out-of-range constant fails the
    /// build rather than being carried at runtime. The alternative in use before
    /// this was `new(..).unwrap_or(OFF)`, which turned a wrong constant into a
    /// probe that sends nothing while its record claims a level: a fabricated
    /// default in the one place this product exists to keep honest.
    ///
    /// Only ever call this in a `const` binding. Reached at runtime with a value
    /// out of range it panics, which is why nothing here takes a variable.
    pub const fn from_percent(percent: u8) -> Self {
        assert!(percent <= MAX_BRIGHTNESS, "brightness is a percentage");
        Self(percent)
    }

    pub fn percent(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Brightness {
    type Error = LightingError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Brightness> for u8 {
    fn from(value: Brightness) -> Self {
        value.0
    }
}

wire_enum! {
    /// The animated effects this product exposes.
    ///
    /// The list is short on purpose: an effect appears here only once the write
    /// probe has run its exact command on the owned controller. Adding a
    /// variant without that evidence is forbidden.
    pub enum LightingEffect {
        Breathing = "breathing", "Breathing",
        SpectrumWave = "spectrum_wave", "Spectrum wave",
    }
}

impl LightingEffect {
    /// How many colors the effect accepts, inclusive.
    ///
    /// Spectrum wave cycles the whole spectrum and takes none: offering a color
    /// control for it would suggest an input the firmware ignores.
    pub fn color_range(self) -> (usize, usize) {
        match self {
            Self::Breathing => (1, 8),
            Self::SpectrumWave => (0, 0),
        }
    }

    /// Whether the firmware honors a direction for this effect.
    pub fn accepts_direction(self) -> bool {
        match self {
            Self::Breathing => false,
            Self::SpectrumWave => true,
        }
    }
}

wire_enum! {
    /// The five animation speeds the controller accepts.
    pub enum EffectSpeed {
        Slowest = "slowest", "Slowest",
        Slower = "slower", "Slower",
        Normal = "normal", "Normal",
        Faster = "faster", "Faster",
        Fastest = "fastest", "Fastest",
    }
}

impl EffectSpeed {
    /// Index into the firmware's speed table, 0 slowest through 4 fastest.
    ///
    /// Written out rather than derived from the position in `ALL`. This is the
    /// firmware's numbering, not this product's ordering, and the two are only
    /// equal today; deriving one from the other would make a reordering of the
    /// select silently renumber what the controller is sent. The agreement is
    /// asserted instead, by `the_speeds_index_the_firmware_table_in_order`.
    pub fn index(self) -> usize {
        match self {
            Self::Slowest => 0,
            Self::Slower => 1,
            Self::Normal => 2,
            Self::Faster => 3,
            Self::Fastest => 4,
        }
    }
}

wire_enum! {
    /// Which way an animated effect travels along the strip.
    pub enum EffectDirection {
        Forward = "forward", "Forward",
        Backward = "backward", "Backward",
    }
}

/// What one channel should display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LightingProgram {
    /// Every LED on the channel dark.
    Off,
    /// One constant color across the channel.
    Fixed { color: Rgb, brightness: Brightness },
    /// A validated animation.
    Effect {
        effect: LightingEffect,
        /// As many colors as the effect accepts, and no more.
        colors: Vec<Rgb>,
        brightness: Brightness,
        speed: EffectSpeed,
        direction: EffectDirection,
    },
}

impl LightingProgram {
    /// Capability that must be writable before this program can be applied.
    pub fn required_capability(&self) -> CapabilityId {
        match self {
            Self::Off | Self::Fixed { .. } => CapabilityId::RgbFixedColor,
            Self::Effect { .. } => CapabilityId::RgbEffects,
        }
    }

    /// One line describing the program, for diagnostics and the outcome note.
    pub fn summary(&self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::Fixed { color, brightness } => {
                format!("fixed #{} at {}%", color.to_hex(), brightness.percent())
            }
            Self::Effect {
                effect,
                speed,
                direction,
                ..
            } => {
                if effect.accepts_direction() {
                    format!(
                        "{} at {} speed, {}",
                        effect.label(),
                        speed.label().to_lowercase(),
                        direction.label().to_lowercase()
                    )
                } else {
                    format!(
                        "{} at {} speed",
                        effect.label(),
                        speed.label().to_lowercase()
                    )
                }
            }
        }
    }
}

/// A program addressed to one channel of the controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingCommand {
    /// One-based channel index, matching the label on the controller.
    pub channel: u8,
    pub program: LightingProgram,
}

/// Why a lighting value or command was refused.
///
/// Each variant carries what the control needs to point at the invalid field
/// without restating the bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LightingError {
    #[error("Enter {expected} hexadecimal digits, for example 7C5CFF. Got {actual}.")]
    ColorDigits { expected: usize, actual: usize },
    #[error("Only the digits 0-9 and the letters A-F are accepted, got {value}.")]
    ColorAlphabet { value: String },
    #[error("Brightness {value} is outside the accepted range 0-{max}.")]
    BrightnessOutOfRange { value: u8, max: u8 },
    #[error("Channel {channel} does not exist. This controller exposes {available}.")]
    ChannelUnknown { channel: u8, available: u8 },
    #[error("{effect} accepts {min}-{max} colors, got {actual}.")]
    ColorCount {
        effect: String,
        min: usize,
        max: usize,
        actual: usize,
    },
    #[error("{effect} ignores direction on this firmware, so it must stay forward.")]
    DirectionUnsupported { effect: String },
    #[error(
        "Commands to channel {channel} are limited to one every {minimum_interval_ms} ms; the last one was {elapsed_ms} ms ago."
    )]
    CadenceTooFast {
        channel: u8,
        minimum_interval_ms: u64,
        elapsed_ms: u64,
    },
}

impl LightingError {
    /// The channel a rejection points at, when it points at one.
    pub fn channel(&self) -> Option<u8> {
        match self {
            Self::ChannelUnknown { channel, .. } | Self::CadenceTooFast { channel, .. } => {
                Some(*channel)
            }
            _ => None,
        }
    }
}

/// Validate a program on its own, before the channel is even consulted.
pub fn validate_program(program: &LightingProgram) -> Result<(), LightingError> {
    match program {
        LightingProgram::Off => Ok(()),
        // A brightness that exists cannot be out of range: `Brightness` refuses
        // to hold one, on the wire as well as in memory.
        LightingProgram::Fixed { .. } => Ok(()),
        LightingProgram::Effect {
            effect,
            colors,
            direction,
            ..
        } => {
            let (min, max) = effect.color_range();
            if !(min..=max).contains(&colors.len()) {
                return Err(LightingError::ColorCount {
                    effect: effect.label().to_string(),
                    min,
                    max,
                    actual: colors.len(),
                });
            }
            if *direction != EffectDirection::Forward && !effect.accepts_direction() {
                return Err(LightingError::DirectionUnsupported {
                    effect: effect.label().to_string(),
                });
            }
            Ok(())
        }
    }
}

/// Validate a command against a controller exposing `channel_count` channels.
///
/// `channel_count` comes from the device's own answer, never from a constant,
/// so a controller with a different topology refuses the command instead of
/// addressing a channel that is not there.
pub fn validate_command(command: &LightingCommand, channel_count: u8) -> Result<(), LightingError> {
    if command.channel == 0 || command.channel > channel_count {
        return Err(LightingError::ChannelUnknown {
            channel: command.channel,
            available: channel_count,
        });
    }
    validate_program(&command.program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_six_digit_color_round_trips() {
        let color = Rgb::parse_hex("7C5CFF").unwrap();
        assert_eq!(color, Rgb::new(0x7c, 0x5c, 0xff));
        assert_eq!(color.to_hex(), "7C5CFF");
        assert_eq!(Rgb::parse_hex("#7c5cff").unwrap(), color);
        assert_eq!(Rgb::parse_hex("  7C5CFF ").unwrap(), color);
    }

    #[test]
    fn an_invalid_color_names_the_exact_problem() {
        let short = Rgb::parse_hex("7C5C").unwrap_err();
        assert!(matches!(
            short,
            LightingError::ColorDigits { actual: 4, .. }
        ));
        assert!(
            short.to_string().contains("6 hexadecimal digits"),
            "{short}"
        );

        let bad = Rgb::parse_hex("ZZZZZZ").unwrap_err();
        assert!(matches!(bad, LightingError::ColorAlphabet { .. }));

        // A multi-byte character counts once, so the message cannot claim more
        // digits than the operator typed.
        let unicode = Rgb::parse_hex("éé").unwrap_err();
        assert!(matches!(
            unicode,
            LightingError::ColorDigits { actual: 2, .. }
        ));
    }

    #[test]
    fn brightness_cannot_hold_an_out_of_range_value() {
        assert_eq!(Brightness::new(100).unwrap().percent(), 100);
        assert_eq!(Brightness::new(0).unwrap().percent(), 0);
        assert!(Brightness::new(101).is_err());

        // Including when it arrives from the socket rather than from a call.
        let refused: Result<Brightness, _> = serde_json::from_str("101");
        assert!(refused.is_err());
        let accepted: Brightness = serde_json::from_str("42").unwrap();
        assert_eq!(accepted.percent(), 42);
    }

    #[test]
    fn brightness_scales_the_triplet_and_zero_means_dark() {
        let color = Rgb::new(200, 100, 50);
        assert_eq!(color.scaled(Brightness::FULL), color);
        assert_eq!(color.scaled(Brightness::OFF), Rgb::BLACK);
        assert_eq!(
            color.scaled(Brightness::new(50).unwrap()),
            Rgb::new(100, 50, 25)
        );
        // The scale never overflows a channel, whatever the inputs.
        assert_eq!(
            Rgb::new(255, 255, 255).scaled(Brightness::FULL),
            Rgb::new(255, 255, 255)
        );
    }

    #[test]
    fn mixing_lands_on_the_ends_it_was_given_and_is_symmetric_between_them() {
        let from = Rgb::new(0x00, 0x20, 0xff);
        let to = Rgb::new(0xff, 0xa0, 0x00);
        assert_eq!(from.mixed(to, 0.0), from, "nothing of the way is the start");
        assert_eq!(from.mixed(to, 1.0), to, "all of the way is the end");
        // The same point, approached from either end, rounds to the same color.
        assert_eq!(from.mixed(to, 0.25), to.mixed(from, 0.75));
        // And it is a straight line between them, channel by channel.
        assert_eq!(from.mixed(to, 0.5), Rgb::new(0x80, 0x60, 0x80));
    }

    #[test]
    fn the_probe_brightness_is_dim_but_visible() {
        // A constant assertion on purpose: this is the value the write probe
        // sends to real hardware, so the bound belongs with the constant.
        const _: () = assert!(
            PROBE_BRIGHTNESS > 0 && PROBE_BRIGHTNESS <= 25,
            "the write probe must be visible and stay low-brightness"
        );
        assert_eq!(
            Brightness::new(PROBE_BRIGHTNESS).map(Brightness::percent),
            Ok(PROBE_BRIGHTNESS)
        );
    }

    #[test]
    fn only_the_two_validated_effects_exist() {
        assert_eq!(LightingEffect::ALL.len(), 2);
        let keys: Vec<_> = LightingEffect::ALL.iter().map(|e| e.key()).collect();
        assert_eq!(keys, vec!["breathing", "spectrum_wave"]);
        assert_eq!(LightingEffect::from_key("rainbow_flow"), None);
    }

    /// The key a select control sends and the string serde puts on the wire are
    /// two separate hand-written mirrors of the same variant list. A drift
    /// between them is a control the daemon refuses every time it is used.
    #[test]
    fn every_lighting_key_is_the_name_serde_writes() {
        crate::keys::assert_keys_are_the_serde_names(
            &LightingEffect::ALL,
            LightingEffect::key,
            LightingEffect::from_key,
        );
        crate::keys::assert_keys_are_the_serde_names(
            &EffectSpeed::ALL,
            EffectSpeed::key,
            EffectSpeed::from_key,
        );
        crate::keys::assert_keys_are_the_serde_names(
            &EffectDirection::ALL,
            EffectDirection::key,
            EffectDirection::from_key,
        );
    }

    /// The index is what the firmware's speed table is addressed by, so the
    /// five speeds have to cover 0 through 4 exactly once and in order.
    #[test]
    fn the_speeds_index_the_firmware_table_in_order() {
        let indices: Vec<usize> = EffectSpeed::ALL.iter().map(|speed| speed.index()).collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn an_effect_refuses_a_color_count_it_does_not_accept() {
        let wave = LightingProgram::Effect {
            effect: LightingEffect::SpectrumWave,
            colors: vec![Rgb::new(255, 0, 0)],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        };
        let error = validate_program(&wave).unwrap_err();
        assert!(matches!(error, LightingError::ColorCount { .. }));

        let breathing_empty = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        };
        assert!(validate_program(&breathing_empty).is_err());

        let breathing_too_many = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![Rgb::BLACK; 9],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        };
        assert!(validate_program(&breathing_too_many).is_err());
    }

    #[test]
    fn a_direction_the_effect_ignores_is_refused_rather_than_silently_dropped() {
        let breathing = LightingProgram::Effect {
            effect: LightingEffect::Breathing,
            colors: vec![Rgb::new(255, 0, 0)],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Backward,
        };
        let error = validate_program(&breathing).unwrap_err();
        assert!(matches!(error, LightingError::DirectionUnsupported { .. }));

        let wave = LightingProgram::Effect {
            effect: LightingEffect::SpectrumWave,
            colors: vec![],
            brightness: Brightness::FULL,
            speed: EffectSpeed::Fastest,
            direction: EffectDirection::Backward,
        };
        assert!(validate_program(&wave).is_ok());
    }

    #[test]
    fn a_channel_outside_the_reported_topology_is_refused() {
        let command = LightingCommand {
            channel: 4,
            program: LightingProgram::Off,
        };
        let error = validate_command(&command, 3).unwrap_err();
        assert_eq!(error.channel(), Some(4));
        assert!(error.to_string().contains("exposes 3"), "{error}");

        // Channels are one-based, so zero is not an alias for the first one.
        let zero = LightingCommand {
            channel: 0,
            program: LightingProgram::Off,
        };
        assert!(validate_command(&zero, 3).is_err());

        for channel in 1..=3 {
            assert!(
                validate_command(
                    &LightingCommand {
                        channel,
                        program: LightingProgram::Off
                    },
                    3
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn each_program_names_the_capability_it_needs() {
        assert_eq!(
            LightingProgram::Off.required_capability(),
            CapabilityId::RgbFixedColor
        );
        assert_eq!(
            LightingProgram::Fixed {
                color: Rgb::BLACK,
                brightness: Brightness::FULL
            }
            .required_capability(),
            CapabilityId::RgbFixedColor
        );
        assert_eq!(
            LightingProgram::Effect {
                effect: LightingEffect::Breathing,
                colors: vec![Rgb::BLACK],
                brightness: Brightness::FULL,
                speed: EffectSpeed::Normal,
                direction: EffectDirection::Forward,
            }
            .required_capability(),
            CapabilityId::RgbEffects
        );
    }

    #[test]
    fn a_program_round_trips_without_carrying_protocol_bytes() {
        let program = LightingProgram::Effect {
            effect: LightingEffect::SpectrumWave,
            colors: vec![],
            brightness: Brightness::new(80).unwrap(),
            speed: EffectSpeed::Faster,
            direction: EffectDirection::Backward,
        };
        let json = serde_json::to_string(&program).unwrap();
        assert_eq!(
            serde_json::from_str::<LightingProgram>(&json).unwrap(),
            program
        );
        // A saved effect must not pin the configuration to one wire encoding,
        // so no packet byte may appear in the stored form.
        for protocol_byte in ["0x2a", "42,4", "report", "packet"] {
            assert!(!json.contains(protocol_byte), "{json}");
        }
    }

    #[test]
    fn an_unknown_command_field_is_refused_instead_of_ignored() {
        let frame = r#"{"channel":1,"program":{"mode":"off"},"force":true}"#;
        assert!(serde_json::from_str::<LightingCommand>(frame).is_err());
    }
}
