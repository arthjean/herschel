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

use std::path::PathBuf;
use std::time::Duration;

use kori_core::capability::LcdPanel;
use kori_core::display::{
    DisplayError, DisplayMode, DisplayPreset, LcdMetric, Orientation, ReadingSlot,
};
use kori_core::lighting::{Brightness, Rgb};

/// Percentage a single press moves the brightness.
pub const BRIGHTNESS_STEP: u8 = 5;

/// One of the colors the editor exposes.
///
/// Band and Text are per slot because the two halves of the dial are configured
/// separately: Reading 1/2 and Text 1/2 each get their own, and a shared color
/// would make one of each pair unreachable. Each band also carries the color it fades
/// to, which only the layouts that draw a band long enough to shade ever ask
/// for.
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
        let preset = DisplayPreset::default_infographic();
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

    /// Move the brightness by whole steps, staying inside 0-100.
    pub fn adjust_brightness(&mut self, steps: i16) {
        let next = self.brightness as i16 + steps * BRIGHTNESS_STEP as i16;
        self.brightness = next.clamp(0, kori_core::lighting::MAX_BRIGHTNESS as i16) as u8;
    }

    /// Set the brightness outright, staying inside the same range.
    ///
    /// The slider names a value rather than a number of steps, and the range is
    /// enforced here so no caller has to know it.
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

/// The whole editable state of the LCD screen: what is being typed, and what
/// can be shown.
///
/// The two are separate values on purpose. The prior valid preview stays
/// visible while a field is mid-edit, which is only expressible
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
    film: Option<ImageFilm>,
    /// Why the picture could not be compiled, when it could not be.
    ///
    /// Kept beside the film rather than derived from the preset: a path that
    /// parses perfectly can still name a file that is not there, and that is a
    /// refusal the operator has to read on the screen rather than discover as
    /// an empty preview.
    film_error: Option<DisplayError>,
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
            film: None,
            film_error: None,
        }
    }
}

/// The picture an image preset names, already rendered.
///
/// Image mode is the one mode whose picture does not depend on telemetry, and
/// the one whose source is expensive: an animated GIF is a compressed stream
/// that has to be composited frame by frame. Rendering it the way the other
/// modes are rendered, on every repaint, would put that cost on every pointer
/// move and would still only ever show the first frame. So it is compiled when
/// the file is chosen and the preview plays the result.
///
/// This is the same compile the daemon runs, from the same function, so the
/// preview and the glass agree frame for frame rather than only on the first
/// picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFilm {
    identity: FilmIdentity,
    frames: Vec<FilmFrame>,
    cursor: usize,
}

/// Everything the compiled frames depend on.
///
/// Anything else about the preset can change without the file being read again,
/// which is what keeps the brightness stepper from re-decoding a GIF.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FilmIdentity {
    image: PathBuf,
    orientation: Orientation,
    background: Rgb,
    panel: (u16, u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FilmFrame {
    png: Vec<u8>,
    delay: Duration,
}

impl ImageFilm {
    /// The frame the preview should be showing.
    pub fn frame(&self) -> Option<&[u8]> {
        self.frames
            .get(self.cursor)
            .map(|frame| frame.png.as_slice())
    }

    /// How long that frame stays up.
    pub fn delay(&self) -> Option<Duration> {
        self.frames.get(self.cursor).map(|frame| frame.delay)
    }

    /// Whether there is anything to play.
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// How long one pass through the whole animation takes.
    pub fn duration(&self) -> Duration {
        self.frames.iter().map(|frame| frame.delay).sum()
    }

    /// Move to the next frame, wrapping at the end.
    fn advance(&mut self) {
        if !self.frames.is_empty() {
            self.cursor = (self.cursor + 1) % self.frames.len();
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
    /// opens on its own defaults shows a mode the panel is not running, which is
    /// the fabricated default this product refuses everywhere else.
    ///
    /// Goes through [`Self::edit`] like every other mutation, so the preview
    /// moves with the editor.
    pub fn adopt(&mut self, preset: &DisplayPreset) {
        self.edit(|editor| *editor = DisplayEditor::from_preset(preset));
    }

    /// The preset the preview should draw.
    pub fn preview(&self) -> &DisplayPreset {
        &self.last_valid
    }

    /// The preset this row would send, or the first reason it cannot be built.
    pub fn preset(&self) -> Result<DisplayPreset, DisplayError> {
        self.editor.preset()
    }

    /// The compiled picture, when the preview is showing one.
    pub fn film(&self) -> Option<&ImageFilm> {
        self.film.as_ref()
    }

    /// Why the picture could not be compiled, when it could not be.
    pub fn film_error(&self) -> Option<&DisplayError> {
        self.film_error.as_ref()
    }

    /// Show the next frame of the animation, and say whether one is playing.
    pub fn advance_film(&mut self) -> bool {
        match self.film.as_mut().filter(|film| film.is_animated()) {
            Some(film) => {
                film.advance();
                true
            }
            None => false,
        }
    }

    /// Compile the picture the preview needs, unless it is already held.
    ///
    /// Called after the edits that can change which file is shown, not on every
    /// repaint: the identity carries everything the frames depend on, so a
    /// second call with nothing changed costs one comparison and no decode.
    /// `panel` is `None` until one answers. There is no picture to compile
    /// against a panel that has not said how big it is, and inventing a size
    /// would put frames of a fabricated geometry in front of the operator.
    pub fn sync_film(&mut self, panel: Option<&LcdPanel>) {
        let preset = &self.last_valid;
        let Some((panel, image)) = panel
            .filter(|panel| panel.width > 0 && panel.height > 0)
            .filter(|_| preset.mode.uses_image())
            .zip(preset.image.clone())
        else {
            self.film = None;
            self.film_error = None;
            return;
        };

        let identity = FilmIdentity {
            image,
            orientation: preset.orientation,
            background: preset.background,
            panel: (panel.width, panel.height),
        };
        if self
            .film
            .as_ref()
            .is_some_and(|film| film.identity == identity)
        {
            return;
        }

        // Dropped before the new one is built, so a file that fails to decode
        // leaves an empty preview naming the reason rather than the previous
        // operator's picture with a warning under it.
        self.film = None;
        self.film_error = None;
        match compile(preset, panel) {
            Ok(frames) => {
                self.film = Some(ImageFilm {
                    identity,
                    frames,
                    cursor: 0,
                })
            }
            Err(error) => self.film_error = Some(error),
        }
    }
}

/// Render every frame of the preset's picture, as the preview displays them.
fn compile(preset: &DisplayPreset, panel: &LcdPanel) -> Result<Vec<FilmFrame>, DisplayError> {
    let rendered = kori_lcd_renderer::render_image_frames(preset, panel)?;
    let mut frames = Vec::with_capacity(rendered.len());
    for frame in rendered {
        frames.push(FilmFrame {
            png: frame.frame.to_png()?,
            delay: frame.delay,
        });
    }
    Ok(frames)
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
        // infographic while the row would have sent a solid field. Every control
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

        screen.edit(|editor| editor.set_color_text(DisplayColorField::Background, "123456"));
        assert_eq!(screen.preview().background, Rgb::new(0x12, 0x34, 0x56));

        screen.edit(|editor| editor.orientation = Orientation::Deg90);
        assert_eq!(screen.preview().orientation, Orientation::Deg90);

        screen.edit(|editor| editor.adjust_brightness(-2));
        assert_eq!(
            screen.preview().brightness.percent(),
            100 - 2 * BRIGHTNESS_STEP
        );

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

    /// The panel the film tests compile against.
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
        screen.sync_film(Some(&panel()));

        let film = screen.film().expect("the picture compiled");
        assert_eq!(film.frame_count(), 3);
        assert!(film.is_animated());
        assert!(film.frame().is_some(), "there is something to draw");
        // Three frames at a tenth of a second each, once the transport floor
        // has had its say on none of them.
        assert_eq!(film.duration(), Duration::from_millis(300));
        assert_eq!(screen.film_error(), None);
    }

    #[test]
    fn a_still_picture_compiles_to_a_film_that_does_not_play() {
        let (mut screen, _path) = screen_showing("film-still", 1);
        screen.sync_film(Some(&panel()));

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
        screen.sync_film(Some(&panel()));

        let first = screen.film().and_then(ImageFilm::frame).map(<[u8]>::to_vec);
        assert!(screen.advance_film());
        let second = screen.film().and_then(ImageFilm::frame).map(<[u8]>::to_vec);
        assert_ne!(first, second, "the preview moved to another picture");

        assert!(screen.advance_film());
        assert!(screen.advance_film());
        assert_eq!(
            screen.film().and_then(ImageFilm::frame).map(<[u8]>::to_vec),
            first,
            "three steps through three frames is back where it started"
        );
    }

    #[test]
    fn a_second_sync_with_nothing_changed_does_not_decode_the_file_again() {
        // The identity is what keeps the brightness stepper from re-decoding a
        // GIF on every press. Asserted through the cursor, because a rebuilt
        // film is one that went back to its first frame.
        let (mut screen, _path) = screen_showing("film-identity", 3);
        screen.sync_film(Some(&panel()));
        assert!(screen.advance_film());
        let showing = screen.film().and_then(ImageFilm::frame).map(<[u8]>::to_vec);

        screen.edit(|editor| editor.brightness = 40);
        screen.sync_film(Some(&panel()));
        assert_eq!(
            screen.film().and_then(ImageFilm::frame).map(<[u8]>::to_vec),
            showing,
            "an edit the frames do not depend on left the film where it was"
        );

        // An edit they do depend on rebuilds it.
        screen.edit(|editor| editor.orientation = Orientation::Deg90);
        screen.sync_film(Some(&panel()));
        assert_eq!(
            screen.film().map(ImageFilm::frame_count),
            Some(3),
            "the rebuilt film still carries every frame"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_names_itself_and_leaves_no_picture() {
        let mut screen = DisplayScreen::default();
        screen.edit(|editor| {
            editor.mode = DisplayMode::Image;
            editor.image_path = "/nonexistent/kori/absent.gif".to_string();
        });
        screen.sync_film(Some(&panel()));

        assert!(
            screen.film().is_none(),
            "no picture is better than a stale one"
        );
        assert!(matches!(
            screen.film_error(),
            Some(DisplayError::ImageUndecodable { .. })
        ));
    }

    #[test]
    fn a_panel_that_has_not_answered_compiles_nothing_rather_than_guessing_a_size() {
        // The frames are sized against the glass. Until one answers there is no
        // size, and inventing one would put a picture of a fabricated geometry
        // in front of the operator while the row above says no panel answered.
        let (mut screen, _path) = screen_showing("film-no-panel", 3);
        screen.sync_film(None);
        assert!(screen.film().is_none());
        assert_eq!(
            screen.film_error(),
            None,
            "an absent panel is not a file that failed to decode"
        );

        // The same screen compiles as soon as one does.
        screen.sync_film(Some(&panel()));
        assert_eq!(screen.film().map(ImageFilm::frame_count), Some(3));
    }

    #[test]
    fn leaving_image_mode_drops_the_film_and_its_error() {
        let (mut screen, _path) = screen_showing("film-left", 2);
        screen.sync_film(Some(&panel()));
        assert!(screen.film().is_some());

        screen.edit(|editor| editor.mode = DisplayMode::Solid);
        screen.sync_film(Some(&panel()));
        assert!(screen.film().is_none());
        assert_eq!(screen.film_error(), None);
    }
}
