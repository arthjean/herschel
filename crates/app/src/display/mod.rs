// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The pending state of the LCD screen.
//!
//! Mirrors [`crate::lighting`]: everything here is an edit, and this structure
//! holds it. Choosing a color or picking a metric changes nothing else. What
//! turns it into a frame is the row going quiet, not a button, and the daemon
//! still accepts or refuses the preset that results.
//!
//! Colors are held as the six digits the operator typed rather than as parsed
//! values. That is what lets an incomplete entry stay on screen with its own
//! error while the preview keeps showing the last picture that was valid, which
//! is what an editor owes the operator instead of a field that silently
//! reverts.
//!
//! What the preview actually draws is compiled next door, in the `film`
//! module, and held rather than rebuilt per repaint.

mod film;

use kori_core::capability::LcdPanel;
use kori_core::display::{
    DisplayError, DisplayMode, DisplayPreset, LcdMetric, Orientation, ReadingSlot,
};
use kori_core::lighting::{Brightness, Rgb};
use kori_core::telemetry::TelemetrySnapshot;

pub use film::ImageFilm;
use film::{Identity, Picture};

/// One of the colors the editor exposes.
///
/// Band and Text are per slot because the two halves of the dial are configured
/// separately: Reading 1/2 and Text 1/2 each get their own, and a shared color
/// would make one of each pair unreachable. Each band also carries the color it
/// fades to, which only the layouts that draw a band long enough to shade ever
/// ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayColorField {
    ReadingOne,
    ReadingTwo,
    FadeOne,
    FadeTwo,
    TextOne,
    TextTwo,
    Background,
}

impl DisplayColorField {
    /// Paired by role rather than by slot: the editor lays the fields out two
    /// to a line, slot 1 left and slot 2 right, so the same role is read across
    /// and the slot is read down. This order is the traversal order.
    pub const ALL: [Self; 7] = [
        Self::ReadingOne,
        Self::ReadingTwo,
        Self::FadeOne,
        Self::FadeTwo,
        Self::TextOne,
        Self::TextTwo,
        Self::Background,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ReadingOne => "Band 1",
            Self::ReadingTwo => "Band 2",
            Self::FadeOne => "Fade 1",
            Self::FadeTwo => "Fade 2",
            Self::TextOne => "Text 1",
            Self::TextTwo => "Text 2",
            Self::Background => "Background",
        }
    }

    /// The reading slot this color belongs to, if it belongs to one.
    pub fn slot(self) -> Option<usize> {
        match self {
            Self::ReadingOne | Self::FadeOne | Self::TextOne => Some(0),
            Self::ReadingTwo | Self::FadeTwo | Self::TextTwo => Some(1),
            Self::Background => None,
        }
    }

    /// Stable identifier, used for the control's element id.
    pub fn key(self) -> &'static str {
        match self {
            Self::ReadingOne => "reading-1",
            Self::ReadingTwo => "reading-2",
            Self::FadeOne => "fade-1",
            Self::FadeTwo => "fade-2",
            Self::TextOne => "text-1",
            Self::TextTwo => "text-2",
            Self::Background => "background",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::ReadingOne => 0,
            Self::ReadingTwo => 1,
            Self::FadeOne => 2,
            Self::FadeTwo => 3,
            Self::TextOne => 4,
            Self::TextTwo => 5,
            Self::Background => 6,
        }
    }

    /// Whether this color is a band's second color.
    fn is_fade(self) -> bool {
        matches!(self, Self::FadeOne | Self::FadeTwo)
    }

    /// Whether this color is drawn at all in `mode`.
    ///
    /// A solid field and a static image use neither band nor text color, a
    /// single reading uses only the first slot's, and a layout that draws its
    /// bands solid has nothing to fade to. Those fields are absent rather than
    /// shown doing nothing.
    pub fn is_used_by(self, mode: DisplayMode) -> bool {
        match self.slot() {
            Some(slot) => slot < mode.reading_slots() && (!self.is_fade() || mode.gradates_band()),
            None => !mode.uses_image(),
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
    colors: [String; 7],
    pub orientation: Orientation,
    pub brightness: u8,
    /// Path of the static image, when the operator picked one.
    pub image_path: String,
}

impl Default for DisplayEditor {
    fn default() -> Self {
        Self::from_preset(&DisplayPreset::default_infographic())
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

    /// Set the brightness, staying inside the range the daemon accepts.
    ///
    /// The one setter, as on a lighting channel. A drag names a value rather
    /// than a number of steps, and the keyboard turns its step into a value
    /// before it arrives, so the range is enforced once here.
    pub fn set_brightness(&mut self, percent: u8) {
        self.brightness = percent.min(kori_core::lighting::MAX_BRIGHTNESS);
    }

    /// The preset this row would send, or the first reason it cannot be built.
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
                    reading_end: Some(self.parsed_color(DisplayColorField::FadeOne)?),
                    text: self.parsed_color(DisplayColorField::TextOne)?,
                },
                ReadingSlot {
                    metric: self.metrics[1],
                    reading: self.parsed_color(DisplayColorField::ReadingTwo)?,
                    reading_end: Some(self.parsed_color(DisplayColorField::FadeTwo)?),
                    text: self.parsed_color(DisplayColorField::TextTwo)?,
                },
            ],
            background: self.parsed_color(DisplayColorField::Background)?,
            logo: None,
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
                preset.readings[0].band_end().to_hex(),
                preset.readings[1].band_end().to_hex(),
                preset.readings[0].text.to_hex(),
                preset.readings[1].text.to_hex(),
                preset.background.to_hex(),
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

/// The whole editable state of the LCD screen: what is being typed, what can be
/// shown, and the picture that shows it.
///
/// The editor and the last valid preset are separate values on purpose. The
/// prior valid preview stays visible while a field is mid-edit, which is only
/// expressible with both. The cost of that split is that a mutation which
/// forgets to re-derive the preview leaves the window drawing a preset the
/// editor no longer holds, and this screen has five controls that mutate.
///
/// So [`DisplayScreen::edit`] is the only way in. The pairing is enforced by
/// construction rather than by remembering it at five call sites, which is
/// exactly the mistake the display-mode select made.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayScreen {
    editor: DisplayEditor,
    last_valid: DisplayPreset,
    picture: Picture,
    /// Why the picture could not be compiled, when it could not be.
    ///
    /// Kept beside it rather than derived from the preset: a path that parses
    /// perfectly can still name a file that is not there, and that is a refusal
    /// the operator has to read on the screen rather than discover as an empty
    /// preview.
    picture_error: Option<DisplayError>,
    /// Which compiled picture the running animation timer belongs to.
    film_generation: u64,
}

impl Default for DisplayScreen {
    fn default() -> Self {
        let editor = DisplayEditor::default();
        let last_valid = editor
            .preset()
            .unwrap_or_else(|_| DisplayPreset::default_infographic());
        Self {
            editor,
            last_valid,
            picture: Picture::None,
            picture_error: None,
            film_generation: 0,
        }
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

    /// Load the whole screen from a preset the daemon reported committed.
    ///
    /// The panel reads nothing back, so the daemon's record of the last preset
    /// it committed is the only evidence of what is on the glass. A client that
    /// opens on its own defaults shows a mode the panel is not running, which
    /// is the fabricated default this product refuses everywhere else.
    ///
    /// Goes through [`Self::edit`] like every other mutation, so the preview
    /// moves with the editor.
    pub fn adopt(&mut self, preset: &DisplayPreset) {
        self.edit(|editor| *editor = DisplayEditor::from_preset(preset));
    }

    /// The preset the preview draws.
    pub fn preview(&self) -> &DisplayPreset {
        &self.last_valid
    }

    /// The preset this row would send, or the first reason it cannot be built.
    pub fn preset(&self) -> Result<DisplayPreset, DisplayError> {
        self.editor.preset()
    }

    /// The frame the preview element should draw, if there is one.
    pub fn picture(&self) -> Option<&[u8]> {
        self.picture.png()
    }

    /// Why there is none, when a picture was attempted and failed.
    pub fn picture_error(&self) -> Option<&DisplayError> {
        self.picture_error.as_ref()
    }

    /// The compiled film, when the preview is playing one.
    pub fn film(&self) -> Option<&ImageFilm> {
        self.picture.film()
    }

    /// Show the next frame of the animation, and say whether one is playing.
    pub fn advance_film(&mut self) -> bool {
        self.picture.advance()
    }

    /// The generation a newly armed animation timer belongs to.
    ///
    /// Every call takes a fresh one and every timer carries the one it was
    /// spawned under, so a picture that has been replaced leaves timers that
    /// fire into nothing rather than a second clock advancing the new one at
    /// twice its cadence.
    pub fn next_film_generation(&mut self) -> u64 {
        self.film_generation = self.film_generation.wrapping_add(1);
        self.film_generation
    }

    pub fn film_generation(&self) -> u64 {
        self.film_generation
    }

    /// Build the picture the preview needs, unless it is already held.
    ///
    /// Cheap when nothing it depends on changed: the picture carries its own
    /// key, so this is one comparison per call and a render or a decode only
    /// when something the frame is made of actually moved. It runs on every
    /// telemetry sample and after every edit, which is exactly when that can
    /// happen, and never from a repaint.
    ///
    /// `panel` is `None` until one answers. There is no picture to build
    /// against a panel that has not said how big it is, and inventing a size
    /// would put frames of a fabricated geometry in front of the operator.
    ///
    /// Returns whether the picture was replaced, which is what tells the caller
    /// whether to arm an animation timer. This runs on every sample, and a
    /// timer armed on a sample that changed nothing takes a fresh generation,
    /// which stops the chain already running and costs the animation a frame
    /// every second.
    pub fn sync_picture(
        &mut self,
        panel: Option<&LcdPanel>,
        telemetry: Option<&TelemetrySnapshot>,
    ) -> bool {
        let Some(panel) = panel.filter(|panel| panel.width > 0 && panel.height > 0) else {
            let cleared = self.picture.png().is_some() || self.picture_error.is_some();
            self.picture = Picture::None;
            self.picture_error = None;
            return cleared;
        };

        let wanted = Identity::of(&self.last_valid, panel, telemetry);
        if self.picture.is(&wanted) {
            return false;
        }

        // Dropped before the new one is built, so a file that fails to decode
        // leaves an empty preview naming the reason rather than the previous
        // operator's picture with a warning under it.
        self.picture = Picture::None;
        self.picture_error = None;
        match film::compile(&self.last_valid, panel, wanted) {
            Ok(picture) => self.picture = picture,
            Err(error) => self.picture_error = Some(error),
        }
        true
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
        // One distinct value per field: nothing shares a slot.
        let values: Vec<String> = DisplayColorField::ALL
            .into_iter()
            .map(|field| editor.color_text(field).to_string())
            .collect();
        let unique: std::collections::HashSet<&String> = values.iter().collect();
        assert_eq!(unique.len(), DisplayColorField::ALL.len(), "{values:?}");

        // Each field's value is read back from the slot of the preset it is
        // supposed to feed. Derived from where the field sits in `ALL` rather
        // than from a literal, so reordering the grid cannot silently swap two
        // colors and still pass.
        let position = |wanted: DisplayColorField| {
            DisplayColorField::ALL
                .into_iter()
                .position(|field| field == wanted)
                .unwrap() as u8
        };
        let preset = editor.preset().unwrap();
        for (field, written) in [
            (DisplayColorField::ReadingOne, preset.readings[0].reading),
            (DisplayColorField::FadeOne, preset.readings[0].band_end()),
            (DisplayColorField::TextOne, preset.readings[0].text),
            (DisplayColorField::ReadingTwo, preset.readings[1].reading),
            (DisplayColorField::FadeTwo, preset.readings[1].band_end()),
            (DisplayColorField::TextTwo, preset.readings[1].text),
            (DisplayColorField::Background, preset.background),
        ] {
            assert_eq!(
                written.r,
                position(field),
                "{} reached the wrong slot of the preset",
                field.label()
            );
        }
        assert_eq!(
            preset.logo, None,
            "the panel carries no wordmark, so the preset writes no color for one"
        );
    }

    #[test]
    fn an_incomplete_color_names_its_own_field_and_stops_the_frame() {
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
        assert!(screen.preset().is_err(), "and nothing can be sent");

        // Finishing the entry moves it again.
        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "0A0B0C"));
        assert_eq!(screen.preview().background, Rgb::new(0x0a, 0x0b, 0x0c));
    }

    #[test]
    fn every_editor_change_moves_the_preview_with_it() {
        // The defect this pins: the display-mode select changed the editor and
        // left the preview drawing the previous mode, so the window showed an
        // infographic while the row would have sent a solid field. Every
        // control on the screen mutates through one entry point now, and this
        // walks each kind of change that entry point has to carry.
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

        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "123456"));
        assert_eq!(screen.preview().background, Rgb::new(0x12, 0x34, 0x56));

        screen.edit(|editor| editor.orientation = Orientation::Deg90);
        assert_eq!(screen.preview().orientation, Orientation::Deg90);

        screen.edit(|editor| editor.set_brightness(90));
        assert_eq!(screen.preview().brightness.percent(), 90);

        // Whatever the route in, the preview is the preset the row would send.
        assert_eq!(&screen.preset().unwrap(), screen.preview());
    }

    #[test]
    fn the_screen_opens_on_the_preset_the_daemon_reports_committed() {
        // What the panel is running outlives the window, so a client that opens
        // while the daemon streams a picture shows that picture rather than the
        // arrangement it ships with.
        let mut screen = DisplayScreen::default();
        let mut committed = DisplayPreset::default_infographic();
        committed.mode = DisplayMode::SingleReading;
        committed.readings[0].metric = LcdMetric::LiquidTemperature;
        committed.background = Rgb::new(0x11, 0x22, 0x33);
        committed.orientation = Orientation::Deg180;
        committed.brightness = Brightness::new(40).unwrap();
        assert_ne!(screen.preset().unwrap(), committed);

        screen.adopt(&committed);

        assert_eq!(screen.editor().mode, DisplayMode::SingleReading);
        assert_eq!(
            screen.editor().color_text(DisplayColorField::Background),
            "112233"
        );
        assert_eq!(screen.editor().brightness, 40);
        // The preview follows, and the row would send back exactly what the
        // panel is already showing, so adopting sends nothing.
        assert_eq!(screen.preview(), &committed);
        assert_eq!(screen.preset().unwrap(), committed);
    }

    #[test]
    fn brightness_never_leaves_the_range_the_daemon_accepts() {
        let mut editor = DisplayEditor::default();
        editor.set_brightness(63);
        assert_eq!(editor.brightness, 63);
        editor.set_brightness(0);
        assert_eq!(editor.brightness, 0);
        editor.set_brightness(200);
        assert_eq!(editor.brightness, kori_core::lighting::MAX_BRIGHTNESS);
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
        // The paired layout draws both slots and shades neither, so it asks for
        // every color except the two a band would fade to.
        for field in DisplayColorField::ALL {
            assert_eq!(
                field.is_used_by(DisplayMode::DualInfographic),
                !matches!(
                    field,
                    DisplayColorField::FadeOne | DisplayColorField::FadeTwo
                ),
                "{field:?} on the infographic"
            );
        }
        // A single reading draws the first slot and not the second, and shades
        // its band, so the second slot is absent and the first fade is not.
        assert!(DisplayColorField::ReadingOne.is_used_by(DisplayMode::SingleReading));
        assert!(DisplayColorField::FadeOne.is_used_by(DisplayMode::SingleReading));
        assert!(DisplayColorField::TextOne.is_used_by(DisplayMode::SingleReading));
        assert!(!DisplayColorField::ReadingTwo.is_used_by(DisplayMode::SingleReading));
        assert!(!DisplayColorField::FadeTwo.is_used_by(DisplayMode::SingleReading));
        assert!(!DisplayColorField::TextTwo.is_used_by(DisplayMode::SingleReading));
        assert!(DisplayColorField::Background.is_used_by(DisplayMode::SingleReading));
        // A solid field has no reading and no caption to color, only its own
        // color, which is the whole picture.
        assert!(DisplayColorField::Background.is_used_by(DisplayMode::Solid));
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
    fn every_field_names_both_the_slot_it_belongs_to_and_what_it_paints() {
        // Each field carries its whole name, because nothing around it says
        // which slot it belongs to any more: the grid puts slot 1 left and slot
        // 2 right, and the label is what tells the two apart when read aloud or
        // by a screen reader.
        for field in DisplayColorField::ALL {
            let label = field.label();
            match field.slot() {
                Some(slot) => assert!(
                    label.ends_with(&(slot + 1).to_string()),
                    "{label} belongs to slot {slot} but does not say so"
                ),
                None => assert!(
                    !label.ends_with('1') && !label.ends_with('2'),
                    "{label} belongs to no slot but is numbered like one"
                ),
            }
        }
        assert_eq!(DisplayColorField::ReadingOne.label(), "Band 1");
        assert_eq!(DisplayColorField::FadeOne.label(), "Fade 1");
        assert_eq!(DisplayColorField::TextTwo.label(), "Text 2");
    }

    #[test]
    fn the_fields_are_ordered_so_each_role_pairs_across_one_line() {
        // Two fields to a line, the same role on both: the grid is read across
        // for the role and down for the slot. The screen renders them in this
        // order and Tab follows it, so it is the order that is asserted.
        let labels: Vec<&str> = DisplayColorField::ALL
            .into_iter()
            .map(DisplayColorField::label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "Band 1",
                "Band 2",
                "Fade 1",
                "Fade 2",
                "Text 1",
                "Text 2",
                "Background"
            ]
        );
        // Every line but the last holds one slot-1 field and its slot-2 twin.
        for pair in labels[..6].chunks(2) {
            assert_eq!(
                pair[0].trim_end_matches('1'),
                pair[1].trim_end_matches('2'),
                "{pair:?} are not the same role"
            );
        }
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

    /// The panel the picture tests compile against.
    fn panel() -> LcdPanel {
        LcdPanel {
            width: 240,
            height: 240,
            shape: kori_core::capability::LcdPanelShape::Square,
            pixel_format: "RGB565 big-endian".to_string(),
            frame_bytes: 240 * 240 * 2,
            bulk_endpoint: 0x02,
            bulk_interface: 0,
        }
    }

    /// A screen in image mode pointing at a GIF of `frames` solid colors.
    fn screen_showing(name: &str, frames: usize) -> (DisplayScreen, std::path::PathBuf) {
        let directory = kori_lcd_renderer::testing::scratch(name).unwrap();
        let path = directory.join("picture.gif");
        let pictures: Vec<(Rgb, u16)> = (0..frames)
            .map(|index| (Rgb::new((index % 32) as u8 * 8, 0x40, 0x80), 10))
            .collect();
        kori_lcd_renderer::testing::write_gif(&path, 16, &pictures).unwrap();

        let mut screen = DisplayScreen::default();
        screen.edit(|editor| {
            editor.mode = DisplayMode::Image;
            editor.image_path = path.display().to_string();
        });
        (screen, path)
    }

    #[test]
    fn an_animated_picture_compiles_to_a_film_the_preview_can_play() {
        let (mut screen, _path) = screen_showing("film-animated", 3);
        screen.sync_picture(Some(&panel()), None);

        let film = screen.film().expect("the picture compiled");
        assert_eq!(film.frame_count(), 3);
        assert!(film.is_animated());
        assert!(screen.picture().is_some(), "there is something to draw");
        // Three frames at a tenth of a second each, once the transport floor
        // has had its say on none of them.
        assert_eq!(film.duration(), std::time::Duration::from_millis(300));
        assert_eq!(screen.picture_error(), None);
    }

    #[test]
    fn a_still_picture_compiles_to_a_film_that_does_not_play() {
        let (mut screen, _path) = screen_showing("film-still", 1);
        screen.sync_picture(Some(&panel()), None);

        let film = screen.film().expect("the picture compiled");
        assert_eq!(film.frame_count(), 1);
        assert!(!film.is_animated());
        assert!(
            !screen.advance_film(),
            "a still picture has no timer to arrange"
        );
    }

    #[test]
    fn the_cursor_walks_the_frames_and_returns_to_the_first() {
        let (mut screen, _path) = screen_showing("film-cursor", 3);
        screen.sync_picture(Some(&panel()), None);

        let first = screen.picture().map(<[u8]>::to_vec);
        assert!(screen.advance_film());
        let second = screen.picture().map(<[u8]>::to_vec);
        assert_ne!(first, second, "the preview moved to another picture");

        assert!(screen.advance_film());
        assert!(screen.advance_film());
        assert_eq!(
            screen.picture().map(<[u8]>::to_vec),
            first,
            "three steps through three frames is back where it started"
        );
    }

    #[test]
    fn a_second_sync_with_nothing_changed_does_not_decode_the_file_again() {
        // The key is what keeps the brightness slider from re-decoding a GIF on
        // every step. Asserted through the cursor, because a rebuilt film is
        // one that went back to its first frame.
        let (mut screen, _path) = screen_showing("film-identity", 3);
        screen.sync_picture(Some(&panel()), None);
        assert!(screen.advance_film());
        let showing = screen.picture().map(<[u8]>::to_vec);

        screen.edit(|editor| editor.set_brightness(40));
        screen.sync_picture(Some(&panel()), None);
        assert_eq!(
            screen.picture().map(<[u8]>::to_vec),
            showing,
            "an edit the frames do not depend on left the film where it was"
        );

        // An edit they do depend on rebuilds it.
        screen.edit(|editor| editor.orientation = Orientation::Deg90);
        screen.sync_picture(Some(&panel()), None);
        assert_eq!(
            screen.film().map(ImageFilm::frame_count),
            Some(3),
            "the rebuilt film still carries every frame"
        );
    }

    /// The defect this pins: the still preview used to be rendered inside the
    /// element tree, so every pointer move of any drag on the screen paid a
    /// full panel-sized rasterization and a PNG encode.
    #[test]
    fn a_dial_is_rendered_once_and_held_rather_than_rebuilt() {
        let mut screen = DisplayScreen::default();
        screen.sync_picture(Some(&panel()), None);
        let drawn = screen.picture().map(<[u8]>::to_vec);
        assert!(drawn.is_some(), "a dial renders without any telemetry");
        assert_eq!(screen.picture_error(), None);

        // Syncing again with nothing moved keeps the exact bytes: the picture
        // carries its own key, so this costs one comparison.
        screen.sync_picture(Some(&panel()), None);
        assert_eq!(screen.picture().map(<[u8]>::to_vec), drawn);

        // An edit the frame is made of produces a different one.
        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "0A0B0C"));
        screen.sync_picture(Some(&panel()), None);
        assert_ne!(screen.picture().map(<[u8]>::to_vec), drawn);
    }

    #[test]
    fn a_file_that_cannot_be_read_names_itself_and_leaves_no_picture() {
        let mut screen = DisplayScreen::default();
        screen.edit(|editor| {
            editor.mode = DisplayMode::Image;
            editor.image_path = "/nonexistent/kori/absent.gif".to_string();
        });
        screen.sync_picture(Some(&panel()), None);

        assert!(
            screen.picture().is_none(),
            "no picture is better than a stale one"
        );
        assert!(matches!(
            screen.picture_error(),
            Some(DisplayError::ImageUndecodable { .. })
        ));
    }

    #[test]
    fn a_panel_that_has_not_answered_compiles_nothing_rather_than_guessing_a_size() {
        // The frames are sized against the glass. Until one answers there is no
        // size, and inventing one would put a picture of a fabricated geometry
        // in front of the operator while the row above says no panel answered.
        let (mut screen, _path) = screen_showing("film-no-panel", 3);
        screen.sync_picture(None, None);
        assert!(screen.picture().is_none());
        assert_eq!(
            screen.picture_error(),
            None,
            "an absent panel is not a file that failed to decode"
        );

        // The same screen compiles as soon as one does.
        screen.sync_picture(Some(&panel()), None);
        assert_eq!(screen.film().map(ImageFilm::frame_count), Some(3));
    }

    #[test]
    fn leaving_image_mode_drops_the_film_and_its_error() {
        let (mut screen, _path) = screen_showing("film-left", 2);
        screen.sync_picture(Some(&panel()), None);
        assert!(screen.film().is_some());

        screen.edit(|editor| editor.mode = DisplayMode::Solid);
        screen.sync_picture(Some(&panel()), None);
        assert!(screen.film().is_none(), "a solid field plays nothing");
        assert_eq!(screen.picture_error(), None);
        assert!(
            screen.picture().is_some(),
            "and the solid field is drawn in its place"
        );
    }

    /// A sample that changed nothing must not re-arm the animation.
    ///
    /// The defect this pins: building the picture moved out of the repaint and
    /// into the sample, and arming the timer alongside it took a fresh
    /// generation every second, which stopped the chain already running and
    /// cost the animation a frame each time.
    #[test]
    fn only_a_replaced_picture_reports_that_the_timer_should_be_armed() {
        let (mut screen, _path) = screen_showing("film-rearm", 3);
        assert!(
            screen.sync_picture(Some(&panel()), None),
            "the first compile is a new picture"
        );
        assert!(
            !screen.sync_picture(Some(&panel()), None),
            "a sample that changed nothing must leave the running timer alone"
        );

        screen.edit(|editor| editor.set_brightness(30));
        assert!(
            !screen.sync_picture(Some(&panel()), None),
            "and neither must an edit the frames do not depend on"
        );

        screen.edit(|editor| editor.orientation = Orientation::Deg90);
        assert!(
            screen.sync_picture(Some(&panel()), None),
            "an edit they do depend on rebuilds, and the new film needs a timer"
        );

        // A panel that goes away clears the picture, which is a change too, and
        // then stays quiet.
        assert!(screen.sync_picture(None, None));
        assert!(!screen.sync_picture(None, None));
    }

    #[test]
    fn every_armed_timer_takes_a_generation_of_its_own() {
        let mut screen = DisplayScreen::default();
        let first = screen.next_film_generation();
        assert_eq!(screen.film_generation(), first);
        assert_ne!(
            screen.next_film_generation(),
            first,
            "a timer left over from a replaced picture must fire into nothing"
        );
    }
}
