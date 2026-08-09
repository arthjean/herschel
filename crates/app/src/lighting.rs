// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The pending state of the Lighting screen.
//!
//! Mirrors [`crate::cooling`]: everything here is an edit that has not happened
//! yet. Typing a color or moving the brightness slider changes this structure
//! and nothing else. The controller is only touched when Apply is activated and
//! the daemon accepts the command.
//!
//! One rule is specific to lighting. Switching a channel to Off must not lose
//! the color it was showing, because Off is a mode the operator comes back
//! from. The editor therefore keeps the fixed color alive across an Off, which
//! is what US-014 means by the prior color remaining available for restoration.

use herschel_core::lighting::{
    Brightness, EffectDirection, EffectSpeed, LightingEffect, LightingError, LightingProgram, Rgb,
};

/// Percentage a single press moves the brightness.
pub const BRIGHTNESS_STEP: u8 = 5;

/// Color a channel starts on, before the operator picks one.
///
/// The project's own accent, so an untouched screen shows the product's palette
/// rather than a vendor's.
pub const DEFAULT_COLOR: &str = "6F4EF2";

/// What the screen is asking one channel to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingMode {
    Off,
    Fixed,
    Effect(LightingEffect),
}

impl LightingMode {
    /// Modes offered when effects have been validated, in menu order.
    pub fn all(effects_available: bool) -> Vec<Self> {
        let mut modes = vec![Self::Fixed, Self::Off];
        if effects_available {
            // An effect that is not proven on this controller is absent, not
            // disabled: a disabled control still tells the operator the feature
            // exists here, and US-015 requires that it does not.
            modes.extend(LightingEffect::ALL.map(Self::Effect));
        }
        modes
    }

    pub fn value(self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::Fixed => "fixed".to_string(),
            Self::Effect(effect) => effect.key().to_string(),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Off => "Off".to_string(),
            Self::Fixed => "Fixed color".to_string(),
            Self::Effect(effect) => effect.label().to_string(),
        }
    }

    pub fn from_value(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "fixed" => Some(Self::Fixed),
            other => LightingEffect::from_key(other).map(Self::Effect),
        }
    }

    /// Whether this mode uses the color field.
    pub fn uses_color(self) -> bool {
        match self {
            Self::Off => false,
            Self::Fixed => true,
            Self::Effect(effect) => effect.color_range().1 > 0,
        }
    }

    /// Whether this mode uses the brightness slider.
    ///
    /// Brightness is applied to the color triplet, so a mode carrying no color
    /// has nothing to dim and its slider would be a control that does nothing.
    pub fn uses_brightness(self) -> bool {
        self.uses_color()
    }

    pub fn uses_speed(self) -> bool {
        matches!(self, Self::Effect(_))
    }

    pub fn uses_direction(self) -> bool {
        matches!(self, Self::Effect(effect) if effect.accepts_direction())
    }
}

/// The pending state of one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelEditor {
    pub channel: u8,
    pub mode: LightingMode,
    /// Six hexadecimal digits as typed, without a leading `#`.
    ///
    /// Held as text rather than as an [`Rgb`] so an incomplete entry can stay
    /// on screen with its own error instead of being silently replaced.
    pub color: String,
    pub brightness: u8,
    pub speed: EffectSpeed,
    pub direction: EffectDirection,
}

impl ChannelEditor {
    pub fn new(channel: u8) -> Self {
        Self {
            channel,
            mode: LightingMode::Fixed,
            color: DEFAULT_COLOR.to_string(),
            brightness: 100,
            speed: EffectSpeed::Normal,
            direction: EffectDirection::Forward,
        }
    }

    /// The color currently typed, or why it cannot be used.
    pub fn parsed_color(&self) -> Result<Rgb, LightingError> {
        Rgb::parse_hex(&self.color)
    }

    /// The program Apply would send, or the first reason it cannot be built.
    ///
    /// Off never consults the color field: a channel can always be turned off,
    /// including while an unfinished color sits in the input.
    pub fn program(&self) -> Result<LightingProgram, LightingError> {
        let brightness = Brightness::new(self.brightness)?;
        match self.mode {
            LightingMode::Off => Ok(LightingProgram::Off),
            LightingMode::Fixed => Ok(LightingProgram::Fixed {
                color: self.parsed_color()?,
                brightness,
            }),
            LightingMode::Effect(effect) => {
                let (min, max) = effect.color_range();
                let colors = if max == 0 {
                    Vec::new()
                } else {
                    let color = self.parsed_color()?;
                    vec![color; min.max(1)]
                };
                Ok(LightingProgram::Effect {
                    effect,
                    colors,
                    brightness,
                    // A direction the effect ignores is never sent, so the
                    // daemon cannot refuse a value the screen let through.
                    direction: if effect.accepts_direction() {
                        self.direction
                    } else {
                        EffectDirection::Forward
                    },
                    speed: self.speed,
                })
            }
        }
    }

    /// Move the brightness by whole steps, staying inside 0-100.
    pub fn adjust_brightness(&mut self, steps: i16) {
        let next = self.brightness as i16 + steps * BRIGHTNESS_STEP as i16;
        self.brightness = next.clamp(0, herschel_core::lighting::MAX_BRIGHTNESS as i16) as u8;
    }

    /// Set the brightness outright, staying inside the same range.
    ///
    /// This is what a slider needs: a drag names a value rather than a number
    /// of steps, and the range is enforced here so no caller has to know it.
    pub fn set_brightness(&mut self, percent: u8) {
        self.brightness = percent.min(herschel_core::lighting::MAX_BRIGHTNESS);
    }
}

/// Every channel's pending state, plus what the daemon last confirmed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LightingEditor {
    channels: Vec<ChannelEditor>,
    /// The channel the screen is editing.
    pub selected: u8,
}

impl LightingEditor {
    /// Rebuild the editor for the channels the controller reported.
    ///
    /// Existing edits survive a refresh: a channel that is still there keeps
    /// what the operator typed, so a telemetry tick cannot discard an entry
    /// mid-edit. A channel that has gone away takes its pending edit with it.
    pub fn sync(&mut self, channels: &[u8]) {
        let mut next: Vec<ChannelEditor> = channels
            .iter()
            .map(|channel| {
                self.channels
                    .iter()
                    .find(|editor| editor.channel == *channel)
                    .cloned()
                    .unwrap_or_else(|| ChannelEditor::new(*channel))
            })
            .collect();
        next.sort_by_key(|editor| editor.channel);
        self.channels = next;

        if !channels.contains(&self.selected) {
            self.selected = channels.first().copied().unwrap_or(0);
        }
    }

    pub fn channels(&self) -> &[ChannelEditor] {
        &self.channels
    }

    pub fn channel(&self, channel: u8) -> Option<&ChannelEditor> {
        self.channels
            .iter()
            .find(|editor| editor.channel == channel)
    }

    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelEditor> {
        self.channels
            .iter_mut()
            .find(|editor| editor.channel == channel)
    }

    pub fn selected(&self) -> Option<&ChannelEditor> {
        self.channel(self.selected)
    }

    pub fn selected_mut(&mut self) -> Option<&mut ChannelEditor> {
        let selected = self.selected;
        self.channel_mut(selected)
    }

    /// Move the edited channel, wrapping at both ends.
    pub fn select(&mut self, channel: u8) {
        if self.channels.iter().any(|editor| editor.channel == channel) {
            self.selected = channel;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_color_becomes_a_program_and_an_invalid_one_does_not() {
        let mut editor = ChannelEditor::new(1);
        assert!(matches!(
            editor.program(),
            Ok(LightingProgram::Fixed { .. })
        ));

        editor.color = "12345".to_string();
        let error = editor.program().unwrap_err();
        assert!(matches!(
            error,
            LightingError::ColorDigits { actual: 5, .. }
        ));

        // Off is always available, even while the color field is unusable.
        editor.mode = LightingMode::Off;
        assert_eq!(editor.program().unwrap(), LightingProgram::Off);
    }

    #[test]
    fn switching_to_off_and_back_keeps_the_color_the_operator_chose() {
        let mut editor = ChannelEditor::new(2);
        editor.color = "FF8800".to_string();
        editor.brightness = 40;

        editor.mode = LightingMode::Off;
        assert_eq!(editor.program().unwrap(), LightingProgram::Off);

        editor.mode = LightingMode::Fixed;
        match editor.program().unwrap() {
            LightingProgram::Fixed { color, brightness } => {
                assert_eq!(color.to_hex(), "FF8800");
                assert_eq!(brightness.percent(), 40);
            }
            other => panic!("expected the prior fixed color, got {other:?}"),
        }
    }

    #[test]
    fn brightness_never_leaves_its_range_however_many_presses_arrive() {
        let mut editor = ChannelEditor::new(1);
        editor.adjust_brightness(100);
        assert_eq!(editor.brightness, 100);
        editor.adjust_brightness(-1000);
        assert_eq!(editor.brightness, 0);
        editor.adjust_brightness(1);
        assert_eq!(editor.brightness, BRIGHTNESS_STEP);
        assert!(editor.program().is_ok());
    }

    #[test]
    fn a_dragged_brightness_lands_inside_the_range_the_daemon_accepts() {
        let mut editor = ChannelEditor::new(1);
        editor.set_brightness(37);
        assert_eq!(editor.brightness, 37);
        // A slider can only produce a value it was given, but the range is
        // enforced here rather than trusted from the caller, which is the same
        // rule the daemon applies to this client.
        editor.set_brightness(255);
        assert_eq!(editor.brightness, herschel_core::lighting::MAX_BRIGHTNESS);
        assert!(editor.program().is_ok());
    }

    #[test]
    fn effects_are_absent_until_they_are_available() {
        let without = LightingMode::all(false);
        assert_eq!(without, vec![LightingMode::Fixed, LightingMode::Off]);
        assert!(
            !without
                .iter()
                .any(|mode| matches!(mode, LightingMode::Effect(_))),
            "an unproven effect must not appear at all"
        );

        let with = LightingMode::all(true);
        assert_eq!(with.len(), 4);
        assert!(with.contains(&LightingMode::Effect(LightingEffect::Breathing)));
        assert!(with.contains(&LightingMode::Effect(LightingEffect::SpectrumWave)));
    }

    #[test]
    fn each_mode_exposes_only_the_controls_it_uses() {
        assert!(LightingMode::Fixed.uses_color());
        assert!(LightingMode::Fixed.uses_brightness());
        assert!(!LightingMode::Fixed.uses_speed());
        assert!(!LightingMode::Fixed.uses_direction());

        assert!(!LightingMode::Off.uses_color());
        assert!(!LightingMode::Off.uses_brightness());

        let breathing = LightingMode::Effect(LightingEffect::Breathing);
        assert!(breathing.uses_color());
        assert!(breathing.uses_speed());
        assert!(
            !breathing.uses_direction(),
            "breathing has no direction on this firmware, so it shows no control for one"
        );

        let wave = LightingMode::Effect(LightingEffect::SpectrumWave);
        assert!(!wave.uses_color(), "the wave cycles the whole spectrum");
        assert!(!wave.uses_brightness(), "there is no triplet to dim");
        assert!(wave.uses_speed());
        assert!(wave.uses_direction());
    }

    #[test]
    fn an_effect_only_carries_the_colors_it_accepts() {
        let mut editor = ChannelEditor::new(1);
        editor.mode = LightingMode::Effect(LightingEffect::SpectrumWave);
        editor.direction = EffectDirection::Backward;
        match editor.program().unwrap() {
            LightingProgram::Effect {
                colors, direction, ..
            } => {
                assert!(colors.is_empty());
                assert_eq!(direction, EffectDirection::Backward);
            }
            other => panic!("expected an effect, got {other:?}"),
        }

        editor.mode = LightingMode::Effect(LightingEffect::Breathing);
        match editor.program().unwrap() {
            LightingProgram::Effect {
                colors, direction, ..
            } => {
                assert_eq!(colors.len(), 1);
                assert_eq!(
                    direction,
                    EffectDirection::Forward,
                    "a direction breathing ignores is never sent"
                );
            }
            other => panic!("expected an effect, got {other:?}"),
        }
    }

    #[test]
    fn every_program_the_editor_builds_passes_the_daemons_own_validation() {
        for mode in LightingMode::all(true) {
            for brightness in [0, 1, 50, 100] {
                let mut editor = ChannelEditor::new(1);
                editor.mode = mode;
                editor.brightness = brightness;
                editor.direction = EffectDirection::Backward;
                let program = editor.program().expect("a default editor is valid");
                assert!(
                    herschel_core::lighting::validate_program(&program).is_ok(),
                    "{mode:?} at {brightness}% produced {program:?}"
                );
            }
        }
    }

    #[test]
    fn syncing_keeps_pending_edits_and_drops_channels_that_went_away() {
        let mut editor = LightingEditor::default();
        editor.sync(&[1, 2, 3]);
        assert_eq!(editor.channels().len(), 3);
        assert_eq!(editor.selected, 1);

        if let Some(channel) = editor.channel_mut(2) {
            channel.color = "ABCDEF".to_string();
        }
        editor.select(2);

        // A refresh must not discard what the operator typed.
        editor.sync(&[1, 2, 3]);
        assert_eq!(editor.channel(2).map(|c| c.color.as_str()), Some("ABCDEF"));
        assert_eq!(editor.selected, 2);

        // A channel that disappeared takes its edit and the selection with it.
        editor.sync(&[1]);
        assert_eq!(editor.channels().len(), 1);
        assert!(editor.channel(2).is_none());
        assert_eq!(editor.selected, 1);

        // And a controller reporting nothing leaves nothing selected.
        editor.sync(&[]);
        assert!(editor.channels().is_empty());
        assert!(editor.selected().is_none());
    }
}
