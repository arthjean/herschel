// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The picture the preview draws, compiled once rather than per repaint.
//!
//! Both kinds of picture live here. Image mode plays the frames of a decoded
//! file; every other mode shows one frame rendered from the preset and the
//! readings. They used to be built in different places and cached to different
//! degrees: the film was keyed by what it depends on, and the rendered frame
//! was produced inside the element tree on every single repaint, which is once
//! per pointer move of any drag anywhere on the screen.
//!
//! One [`Identity`] now keys both, so a repaint draws bytes that already exist
//! and a picture is built exactly when something it depends on moved.

use std::path::PathBuf;
use std::time::Duration;

use kori_core::capability::LcdPanel;
use kori_core::display::{DisplayError, DisplayPreset, MetricSample, Orientation};
use kori_core::lighting::Rgb;
use kori_core::telemetry::TelemetrySnapshot;

/// Everything one compiled picture depends on.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Identity {
    panel: (u16, u16),
    source: Source,
}

#[derive(Debug, Clone, PartialEq)]
enum Source {
    /// What the frames of an image preset depend on, and nothing else.
    ///
    /// Deliberately narrower than the preset. `render_image_frames` reads the
    /// file, the orientation and the background, and the renderer reads
    /// `brightness` nowhere at all: it is a panel setting sent beside the
    /// picture rather than drawn into it. Keying on the whole preset would make
    /// one step of the brightness slider re-decode a GIF.
    Image {
        image: Option<PathBuf>,
        orientation: Orientation,
        background: Rgb,
    },
    /// The whole preset, and the readings the frame carries.
    ///
    /// The whole preset rather than the fields `render` happens to consult
    /// today: one frame costs about a millisecond, which is not worth keeping a
    /// second copy of the renderer's field list in step with. The image path
    /// takes the narrow key because a decode is worth that cost and this is
    /// not.
    Readings {
        preset: Box<DisplayPreset>,
        samples: [MetricSample; 2],
    },
}

impl Identity {
    /// Key the picture `preset` would produce on this panel, at these readings.
    ///
    /// `telemetry` is `None` before the first sample lands, which is drawn as
    /// the readings being unavailable rather than as no picture: the panel
    /// itself would show the same dashes.
    pub(super) fn of(
        preset: &DisplayPreset,
        panel: &LcdPanel,
        telemetry: Option<&TelemetrySnapshot>,
    ) -> Self {
        let source = if preset.mode.uses_image() {
            Source::Image {
                image: preset.image.clone(),
                orientation: preset.orientation,
                background: preset.background,
            }
        } else {
            Source::Readings {
                preset: Box::new(preset.clone()),
                samples: match telemetry {
                    Some(snapshot) => preset.samples(snapshot),
                    None => [
                        MetricSample::unavailable(preset.readings[0].metric),
                        MetricSample::unavailable(preset.readings[1].metric),
                    ],
                },
            }
        };
        Self {
            panel: (panel.width, panel.height),
            source,
        }
    }
}

/// One frame of a compiled animation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    png: Vec<u8>,
    delay: Duration,
}

/// The frames of an image preset, and which one the preview is on.
///
/// This is the same compile the daemon runs, from the same function, so the
/// preview and the glass agree frame for frame rather than only on the first
/// picture.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageFilm {
    identity: Identity,
    frames: Vec<Frame>,
    cursor: usize,
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

/// What the preview has to draw, and what it was built from.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Picture {
    /// Nothing: no panel has answered, or the preset could not be rendered.
    None,
    /// One rendered frame.
    Still { identity: Identity, png: Vec<u8> },
    /// The frames of an image preset.
    Film(ImageFilm),
}

impl Picture {
    /// What this picture was built from, when there is one.
    fn identity(&self) -> Option<&Identity> {
        match self {
            Self::None => None,
            Self::Still { identity, .. } => Some(identity),
            Self::Film(film) => Some(&film.identity),
        }
    }

    /// Whether this picture is already the one `identity` describes.
    pub(super) fn is(&self, identity: &Identity) -> bool {
        self.identity() == Some(identity)
    }

    /// The bytes the preview draws, if any.
    pub(super) fn png(&self) -> Option<&[u8]> {
        match self {
            Self::None => None,
            Self::Still { png, .. } => Some(png),
            Self::Film(film) => film.frame(),
        }
    }

    pub(super) fn film(&self) -> Option<&ImageFilm> {
        match self {
            Self::Film(film) => Some(film),
            Self::None | Self::Still { .. } => None,
        }
    }

    /// Show the next frame, and say whether an animation is playing.
    pub(super) fn advance(&mut self) -> bool {
        match self {
            Self::Film(film) if film.is_animated() => {
                film.advance();
                true
            }
            _ => false,
        }
    }
}

/// Build the picture `identity` describes.
pub(super) fn compile(
    preset: &DisplayPreset,
    panel: &LcdPanel,
    identity: Identity,
) -> Result<Picture, DisplayError> {
    // Read out of the key before it is moved into the picture it labels: the
    // readings are what decide which of the two renderers is called.
    let readings = match &identity.source {
        Source::Readings { samples, .. } => Some(*samples),
        Source::Image { .. } => None,
    };

    match readings {
        Some(samples) => {
            let png = kori_lcd_renderer::render(preset, &samples, panel)?.to_png()?;
            Ok(Picture::Still { identity, png })
        }
        None => {
            let rendered = kori_lcd_renderer::render_image_frames(preset, panel)?;
            let mut frames = Vec::with_capacity(rendered.len());
            for frame in rendered {
                frames.push(Frame {
                    png: frame.frame.to_png()?,
                    delay: frame.delay,
                });
            }
            Ok(Picture::Film(ImageFilm {
                identity,
                frames,
                cursor: 0,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::capability::LcdPanelShape;
    use kori_core::display::DisplayMode;
    use kori_core::lighting::Brightness;

    fn panel() -> LcdPanel {
        LcdPanel {
            width: 240,
            height: 240,
            shape: LcdPanelShape::Square,
            pixel_format: "RGB565 big-endian".to_string(),
            frame_bytes: 240 * 240 * 2,
            bulk_endpoint: 0x02,
            bulk_interface: 0,
        }
    }

    fn image_preset() -> DisplayPreset {
        DisplayPreset {
            mode: DisplayMode::Image,
            image: Some(PathBuf::from("/home/a/loop.gif")),
            ..DisplayPreset::default_infographic()
        }
    }

    /// Stepping the brightness must not re-decode a file.
    ///
    /// The renderer reads `brightness` nowhere: it is a panel setting sent
    /// beside the picture rather than drawn into it. The image key says so,
    /// because the alternative is an LZW pass and a resize per frame every time
    /// the slider settles.
    #[test]
    fn stepping_the_brightness_never_re_decodes_a_file() {
        let panel = panel();
        let preset = image_preset();
        let dimmed = DisplayPreset {
            brightness: Brightness::new(20).expect("20% is inside the range"),
            ..preset.clone()
        };
        assert_eq!(
            Identity::of(&preset, &panel, None),
            Identity::of(&dimmed, &panel, None)
        );
    }

    /// The dial takes the opposite trade, deliberately.
    ///
    /// Its key is the whole preset, so a settled brightness edit costs one
    /// re-render of about a millisecond. That is the price of not keeping a
    /// second copy of the renderer's field list in step with the renderer: on
    /// this path the work is a rasterization, not a decode, and it is paid once
    /// per settled edit rather than per repaint.
    #[test]
    fn a_dial_keys_on_the_whole_preset_rather_than_on_a_field_list() {
        let panel = panel();
        let preset = DisplayPreset::default_infographic();
        let dimmed = DisplayPreset {
            brightness: Brightness::new(20).expect("20% is inside the range"),
            ..preset.clone()
        };
        assert_ne!(
            Identity::of(&preset, &panel, None),
            Identity::of(&dimmed, &panel, None)
        );
    }

    #[test]
    fn anything_the_picture_is_made_of_moves_the_key() {
        let panel = panel();
        let preset = DisplayPreset::default_infographic();
        let base = Identity::of(&preset, &panel, None);

        let recolored = DisplayPreset {
            background: Rgb::new(0x11, 0x22, 0x33),
            ..preset.clone()
        };
        assert_ne!(base, Identity::of(&recolored, &panel, None));

        let turned = DisplayPreset {
            orientation: Orientation::Deg90,
            ..preset.clone()
        };
        assert_ne!(base, Identity::of(&turned, &panel, None));

        // And so does the glass it is rendered for.
        let wider = LcdPanel {
            width: 320,
            ..panel.clone()
        };
        assert_ne!(base, Identity::of(&preset, &wider, None));
    }

    /// The readings are part of the still picture and no part of a film, so a
    /// telemetry tick redraws a dial and leaves an animation alone.
    #[test]
    fn a_new_reading_redraws_a_dial_and_leaves_a_film_alone() {
        let panel = panel();
        let dial = DisplayPreset::default_infographic();
        assert!(matches!(
            Identity::of(&dial, &panel, None).source,
            Source::Readings { .. }
        ));
        assert!(matches!(
            Identity::of(&image_preset(), &panel, None).source,
            Source::Image { .. }
        ));
    }
}
