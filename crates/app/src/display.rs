// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The pending state of the LCD screen.
//!
//! Mirrors [`crate::lighting`]: everything here is an edit that has not
//! happened yet. Typing a color, picking a metric or activating Rotate changes
//! this structure and nothing else. The panel is only touched when Apply is
//! activated and the daemon accepts the preset.
//!
//! Colors are held as the six digits the operator typed rather than as parsed
//! values. That is what lets an incomplete entry stay on screen with its own
//! error while the preview keeps showing the last picture that was valid, which
//! is what US-017 asks for instead of a field that silently reverts.

use nzxt_core::display::{
    DisplayError, DisplayMode, DisplayPreset, LcdMetric, Orientation, ReadingSlot,
};
use nzxt_core::lighting::{Brightness, Rgb};

/// Percentage a single press moves the brightness.
pub const BRIGHTNESS_STEP: u8 = 5;

/// One of the six colors the editor exposes.
///
/// Reading and Text are per slot because the two halves of the dial are
/// configured separately: US-017 names Reading 1/2 and Text 1/2, and a shared
/// color would make one of each pair unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayColorField {
    ReadingOne,
    ReadingTwo,
    TextOne,
    TextTwo,
    Background,
    Logo,
}

impl DisplayColorField {
    pub const ALL: [Self; 6] = [
        Self::ReadingOne,
        Self::TextOne,
        Self::ReadingTwo,
        Self::TextTwo,
        Self::Background,
        Self::Logo,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadingOne => "Reading 1",
            Self::ReadingTwo => "Reading 2",
            Self::TextOne => "Text 1",
            Self::TextTwo => "Text 2",
            Self::Background => "Background",
            Self::Logo => "Logo",
        }
    }

    /// Stable identifier, used for the control's element id.
    pub fn key(self) -> &'static str {
        match self {
            Self::ReadingOne => "reading-1",
            Self::ReadingTwo => "reading-2",
            Self::TextOne => "text-1",
            Self::TextTwo => "text-2",
            Self::Background => "background",
            Self::Logo => "logo",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::ReadingOne => 0,
            Self::ReadingTwo => 1,
            Self::TextOne => 2,
            Self::TextTwo => 3,
            Self::Background => 4,
            Self::Logo => 5,
        }
    }

    /// Whether this color is drawn at all in `mode`.
    ///
    /// A solid field and a static image use neither reading nor caption color,
    /// so those fields are absent rather than shown doing nothing.
    pub fn is_used_by(self, mode: DisplayMode) -> bool {
        match self {
            Self::Background => !mode.uses_image(),
            Self::Logo => !mode.uses_image(),
            _ => mode.uses_readings(),
        }
    }
}

/// The pending state of the whole screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayEditor {
    pub mode: DisplayMode,
    /// Metric shown in each reading slot, in the order they are drawn.
    pub metrics: [LcdMetric; 2],
    /// Six hexadecimal digits per color, as typed, without a leading `#`.
    colors: [String; 6],
    pub orientation: Orientation,
    pub brightness: u8,
    /// Path of the static image, when the operator picked one.
    pub image_path: String,
}

impl Default for DisplayEditor {
    fn default() -> Self {
        let preset = DisplayPreset::default_infographic();
        Self {
            mode: preset.mode,
            metrics: [preset.readings[0].metric, preset.readings[1].metric],
            colors: [
                preset.readings[0].reading.to_hex(),
                preset.readings[1].reading.to_hex(),
                preset.readings[0].text.to_hex(),
                preset.readings[1].text.to_hex(),
                preset.background.to_hex(),
                preset.logo.to_hex(),
            ],
            orientation: preset.orientation,
            brightness: preset.brightness.percent(),
            image_path: String::new(),
        }
    }
}

impl DisplayEditor {
    /// The digits currently in one field.
    pub fn color_text(&self, field: DisplayColorField) -> &str {
        &self.colors[field.index()]
    }

    /// Replace one field's digits.
    pub fn set_color_text(&mut self, field: DisplayColorField, value: impl Into<String>) {
        self.colors[field.index()] = value.into();
    }

    /// The color one field holds, or why it cannot be used.
    pub fn parsed_color(&self, field: DisplayColorField) -> Result<Rgb, DisplayError> {
        Rgb::parse_hex(self.color_text(field)).map_err(|source| DisplayError::Color {
            field: field.label().to_string(),
            source,
        })
    }

    /// Advance the rotation by one validated increment.
    pub fn rotate(&mut self) {
        self.orientation = self.orientation.rotated();
    }

    /// Move the brightness by whole steps, staying inside 0-100.
    pub fn adjust_brightness(&mut self, steps: i16) {
        let next = self.brightness as i16 + steps * BRIGHTNESS_STEP as i16;
        self.brightness = next.clamp(0, nzxt_core::lighting::MAX_BRIGHTNESS as i16) as u8;
    }

    /// Set the brightness outright, staying inside the same range.
    ///
    /// The slider names a value rather than a number of steps, and the range is
    /// enforced here so no caller has to know it.
    pub fn set_brightness(&mut self, percent: u8) {
        self.brightness = percent.min(nzxt_core::lighting::MAX_BRIGHTNESS);
    }

    /// The preset Apply would send, or the first reason it cannot be built.
    ///
    /// Every color is parsed, including the ones the current mode does not
    /// draw. That is deliberate: a mode switch must not turn an entry the
    /// operator has not finished into a frame, and the error names the field
    /// rather than the mode.
    pub fn preset(&self) -> Result<DisplayPreset, DisplayError> {
        let preset = DisplayPreset {
            mode: self.mode,
            readings: [
                ReadingSlot {
                    metric: self.metrics[0],
                    reading: self.parsed_color(DisplayColorField::ReadingOne)?,
                    text: self.parsed_color(DisplayColorField::TextOne)?,
                },
                ReadingSlot {
                    metric: self.metrics[1],
                    reading: self.parsed_color(DisplayColorField::ReadingTwo)?,
                    text: self.parsed_color(DisplayColorField::TextTwo)?,
                },
            ],
            background: self.parsed_color(DisplayColorField::Background)?,
            logo: self.parsed_color(DisplayColorField::Logo)?,
            orientation: self.orientation,
            brightness: Brightness::new(self.brightness).map_err(|source| DisplayError::Color {
                field: "Brightness".to_string(),
                source,
            })?,
            image: self
                .mode
                .uses_image()
                .then(|| std::path::PathBuf::from(self.image_path.trim()))
                .filter(|path| !path.as_os_str().is_empty()),
        };
        preset.validate()?;
        Ok(preset)
    }

    /// Load an editor from a preset the daemon reported.
    pub fn from_preset(preset: &DisplayPreset) -> Self {
        Self {
            mode: preset.mode,
            metrics: [preset.readings[0].metric, preset.readings[1].metric],
            colors: [
                preset.readings[0].reading.to_hex(),
                preset.readings[1].reading.to_hex(),
                preset.readings[0].text.to_hex(),
                preset.readings[1].text.to_hex(),
                preset.background.to_hex(),
                preset.logo.to_hex(),
            ],
            orientation: preset.orientation,
            brightness: preset.brightness.percent(),
            image_path: preset
                .image
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        }
    }
}

/// The whole editable state of the LCD screen: what is being typed, and what
/// can be shown.
///
/// The two are separate values on purpose. US-017 requires the prior valid
/// preview to stay visible while a field is mid-edit, which is only expressible
/// with both. The cost of that split is that a mutation which forgets to
/// re-derive the preview leaves the window drawing a preset the editor no
/// longer holds, and this screen has five controls that mutate.
///
/// So [`DisplayScreen::edit`] is the only way in. The pairing is enforced by
/// construction rather than by remembering it at five call sites, which is
/// exactly the mistake the display-mode select made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayScreen {
    editor: DisplayEditor,
    last_valid: DisplayPreset,
}

impl Default for DisplayScreen {
    fn default() -> Self {
        let editor = DisplayEditor::default();
        let last_valid = editor
            .preset()
            .unwrap_or_else(|_| DisplayPreset::default_infographic());
        Self { editor, last_valid }
    }
}

impl DisplayScreen {
    /// What the operator is currently typing.
    pub fn editor(&self) -> &DisplayEditor {
        &self.editor
    }

    /// Change the editor and re-derive the preview in one step.
    ///
    /// An edit that leaves the entry incomplete keeps the last picture that was
    /// whole, so a half-typed color does not blank the preview.
    pub fn edit(&mut self, edit: impl FnOnce(&mut DisplayEditor)) {
        edit(&mut self.editor);
        if let Ok(preset) = self.editor.preset() {
            self.last_valid = preset;
        }
    }

    /// The preset the preview should draw.
    pub fn preview(&self) -> &DisplayPreset {
        &self.last_valid
    }

    /// The preset Apply would send, or the first reason it cannot be built.
    pub fn preset(&self) -> Result<DisplayPreset, DisplayError> {
        self.editor.preset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_editor_produces_the_preset_the_panel_starts_on() {
        let editor = DisplayEditor::default();
        assert_eq!(
            editor.preset().unwrap(),
            DisplayPreset::default_infographic()
        );
    }

    #[test]
    fn an_editor_round_trips_through_a_preset() {
        let mut preset = DisplayPreset::default_infographic();
        preset.orientation = Orientation::Deg270;
        preset.readings[1].metric = LcdMetric::LiquidTemperature;
        preset.brightness = Brightness::new(35).unwrap();

        let editor = DisplayEditor::from_preset(&preset);
        assert_eq!(editor.preset().unwrap(), preset);
    }

    #[test]
    fn every_color_field_is_separately_addressable() {
        let mut editor = DisplayEditor::default();
        for (index, field) in DisplayColorField::ALL.into_iter().enumerate() {
            editor.set_color_text(field, format!("{index:02X}0000"));
        }
        // Six fields, six distinct values: nothing shares a slot.
        let values: Vec<String> = DisplayColorField::ALL
            .into_iter()
            .map(|field| editor.color_text(field).to_string())
            .collect();
        let unique: std::collections::HashSet<&String> = values.iter().collect();
        assert_eq!(unique.len(), 6, "{values:?}");

        let preset = editor.preset().unwrap();
        assert_eq!(preset.readings[0].reading.r, 0x00);
        assert_eq!(preset.readings[0].text.r, 0x01);
        assert_eq!(preset.readings[1].reading.r, 0x02);
        assert_eq!(preset.readings[1].text.r, 0x03);
        assert_eq!(preset.background.r, 0x04);
        assert_eq!(preset.logo.r, 0x05);
    }

    #[test]
    fn an_incomplete_color_names_its_own_field_and_blocks_apply() {
        let mut editor = DisplayEditor::default();
        editor.set_color_text(DisplayColorField::TextTwo, "12A");

        match editor.preset().unwrap_err() {
            DisplayError::Color { field, .. } => assert_eq!(field, "Text 2"),
            other => panic!("expected a color refusal naming its field, got {other:?}"),
        }

        // A digit outside the alphabet is refused the same way.
        editor.set_color_text(DisplayColorField::TextTwo, "12345Z");
        assert!(matches!(editor.preset(), Err(DisplayError::Color { .. })));

        editor.set_color_text(DisplayColorField::TextTwo, "123456");
        assert!(editor.preset().is_ok());
    }

    #[test]
    fn the_preview_keeps_the_last_valid_picture_while_a_field_is_mid_edit() {
        let mut screen = DisplayScreen::default();

        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "0A0A0A"));
        let good = screen.preview().clone();
        assert_eq!(good.background, Rgb::new(0x0a, 0x0a, 0x0a));

        // Half a color is not a picture, so the preview does not change.
        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "0A0"));
        assert_eq!(
            screen.preview(),
            &good,
            "the prior valid preview must stay visible"
        );
        assert!(screen.preset().is_err(), "and Apply stays refused");

        // Finishing the entry moves it again.
        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "0A0B0C"));
        assert_eq!(screen.preview().background, Rgb::new(0x0a, 0x0b, 0x0c));
    }

    #[test]
    fn every_editor_change_moves_the_preview_with_it() {
        // The defect this pins: the display-mode select changed the editor and
        // left the preview drawing the previous mode, so the window showed an
        // infographic while Apply would have sent a solid field. Every control
        // on the screen mutates through one entry point now, and this walks
        // each kind of change that entry point has to carry.
        let mut screen = DisplayScreen::default();

        screen.edit(|editor| editor.mode = DisplayMode::Solid);
        assert_eq!(
            screen.preview().mode,
            DisplayMode::Solid,
            "the preview must follow the mode select"
        );

        screen.edit(|editor| editor.metrics[1] = LcdMetric::LiquidTemperature);
        assert_eq!(
            screen.preview().readings[1].metric,
            LcdMetric::LiquidTemperature
        );

        screen.edit(|editor| editor.set_color_text(DisplayColorField::Logo, "123456"));
        assert_eq!(screen.preview().logo, Rgb::new(0x12, 0x34, 0x56));

        screen.edit(DisplayEditor::rotate);
        assert_eq!(screen.preview().orientation, Orientation::Deg90);

        screen.edit(|editor| editor.adjust_brightness(-2));
        assert_eq!(
            screen.preview().brightness.percent(),
            100 - 2 * BRIGHTNESS_STEP
        );

        // Whatever the route in, the preview is the preset Apply would send.
        assert_eq!(&screen.preset().unwrap(), screen.preview());
    }

    #[test]
    fn rotation_walks_the_validated_increment_and_returns_to_zero() {
        let mut editor = DisplayEditor::default();
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(editor.orientation.degrees());
            editor.rotate();
        }
        assert_eq!(seen, vec![0, 90, 180, 270]);
        assert_eq!(editor.orientation, Orientation::Deg0);
        assert_eq!(editor.preset().unwrap().orientation, Orientation::Deg0);
    }

    #[test]
    fn brightness_never_leaves_its_range_however_many_presses_arrive() {
        let mut editor = DisplayEditor::default();
        editor.adjust_brightness(100);
        assert_eq!(editor.brightness, 100);
        editor.adjust_brightness(-1000);
        assert_eq!(editor.brightness, 0);
        editor.adjust_brightness(1);
        assert_eq!(editor.brightness, BRIGHTNESS_STEP);
        assert!(editor.preset().is_ok());

        // The slider names a value outright, and the same range applies to it.
        editor.set_brightness(63);
        assert_eq!(editor.brightness, 63);
        editor.set_brightness(200);
        assert_eq!(editor.brightness, nzxt_core::lighting::MAX_BRIGHTNESS);
        assert!(editor.preset().is_ok());
    }

    #[test]
    fn image_mode_needs_a_file_and_says_so_on_the_image_field() {
        let mut editor = DisplayEditor {
            mode: DisplayMode::Image,
            ..DisplayEditor::default()
        };
        let error = editor.preset().unwrap_err();
        assert_eq!(error, DisplayError::ImagePathMissing);
        assert_eq!(error.field(), Some("image"));

        // Whitespace is not a path.
        editor.image_path = "   ".to_string();
        assert_eq!(editor.preset().unwrap_err(), DisplayError::ImagePathMissing);

        editor.image_path = "/home/a/wallpaper.png".to_string();
        assert_eq!(
            editor.preset().unwrap().image,
            Some(std::path::PathBuf::from("/home/a/wallpaper.png"))
        );
    }

    #[test]
    fn a_path_typed_for_image_mode_does_not_travel_with_another_mode() {
        let editor = DisplayEditor {
            image_path: "/home/a/wallpaper.png".to_string(),
            ..DisplayEditor::default()
        };
        assert_eq!(
            editor.preset().unwrap().image,
            None,
            "the infographic must not carry a file it never opens"
        );
    }

    #[test]
    fn each_mode_exposes_only_the_colors_it_draws() {
        for field in DisplayColorField::ALL {
            assert!(
                field.is_used_by(DisplayMode::DualInfographic),
                "{field:?} should be editable on the infographic"
            );
        }
        // A solid field has no reading and no caption to color.
        assert!(DisplayColorField::Background.is_used_by(DisplayMode::Solid));
        assert!(DisplayColorField::Logo.is_used_by(DisplayMode::Solid));
        assert!(!DisplayColorField::ReadingOne.is_used_by(DisplayMode::Solid));
        assert!(!DisplayColorField::TextTwo.is_used_by(DisplayMode::Solid));
        // A static image covers the whole panel, so none of them are.
        assert!(
            DisplayColorField::ALL
                .iter()
                .all(|field| !field.is_used_by(DisplayMode::Image))
        );
    }

    #[test]
    fn the_six_fields_are_ordered_so_each_pair_sits_together() {
        // Reading 1 with Text 1, Reading 2 with Text 2, then the two that
        // belong to the panel rather than to a slot. The screen renders them in
        // this order, so it is the order that is asserted.
        let labels: Vec<&str> = DisplayColorField::ALL
            .into_iter()
            .map(DisplayColorField::label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "Reading 1",
                "Text 1",
                "Reading 2",
                "Text 2",
                "Background",
                "Logo"
            ]
        );
    }

    #[test]
    fn every_preset_the_editor_builds_passes_the_daemons_own_validation() {
        for mode in DisplayMode::ALL {
            for orientation in Orientation::ALL {
                let editor = DisplayEditor {
                    mode,
                    orientation,
                    image_path: "/home/a/wallpaper.png".to_string(),
                    ..DisplayEditor::default()
                };
                let preset = editor.preset().expect("a filled editor is valid");
                assert!(
                    preset.validate().is_ok(),
                    "{mode:?} at {orientation:?} produced a preset the daemon refuses"
                );
            }
        }
    }
}
