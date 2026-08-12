// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! One [`DisplayPreset`] in, one exact framebuffer out.
//!
//! The editor's preview and the bytes the panel receives come from the same
//! description. This crate is how: the GPUI client renders a
//! preset to see it, the daemon renders the same preset to send it, and neither
//! extracts pixels from the other. A difference between what the window shows
//! and what the panel shows could then only be a difference in the preset.
//!
//! Nothing here opens a device or knows a report identifier. The only input
//! from outside the preset is a telemetry sample and, in image mode, a file the
//! operator picked.
//!
//! Five modules, each owning one thing: `canvas` puts coverage on pixels,
//! `text` sets a string in the embedded face, `dial` lays out the two reading
//! layouts, `picture` turns a file the operator picked into pixels, and `frame`
//! holds the panel's geometry and the two byte forms a finished picture leaves
//! in. This file is the seam between them and nothing else.

mod canvas;
mod dial;
mod frame;
mod picture;
mod text;

#[cfg(test)]
mod fixtures;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use frame::{AnimationFrame, Framebuffer};

use kori_core::capability::LcdPanel;
use kori_core::display::{DisplayError, DisplayMode, DisplayPreset, MetricSample};

use canvas::Canvas;

/// Render `preset` against `samples` at the panel's exact geometry.
///
/// The samples are only consulted by the modes that draw readings; a solid
/// field or a static image ignores them, so a panel showing a picture keeps
/// showing it when a collector drops out.
///
/// One call is one picture, and in [`DisplayMode::Image`] that includes reading
/// the file and decoding it again. Cheap for the modes that draw readings,
/// which is what a per-tick loop should call; for a picture, what belongs in a
/// loop is [`render_image_frames`] once and its result held. Both callers do
/// that already, and this arm exists so that the two agree: it is what proves
/// the compiled first frame and the drawn one are the same bytes.
pub fn render(
    preset: &DisplayPreset,
    samples: &[MetricSample; 2],
    panel: &LcdPanel,
) -> Result<Framebuffer, DisplayError> {
    preset.validate()?;
    let (width, height) = frame::layout_size(panel, preset.orientation)?;

    let mut canvas = Canvas::filled(width, height, preset.background);
    match preset.mode {
        DisplayMode::Solid => {}
        DisplayMode::Image => picture::draw(&mut canvas, &picture::still(preset)?),
        DisplayMode::DualInfographic => dial::infographic(&mut canvas, preset, samples),
        DisplayMode::SingleReading => dial::single(&mut canvas, preset, &samples[0]),
    }

    Ok(Framebuffer::from_canvas(canvas, preset.orientation))
}

/// Compile the file an image preset names into the frames the panel will play.
///
/// One entry for a still picture or a single-frame GIF, one per frame for an
/// animated one. The caller decides what that means: the daemon installs a
/// sequence of more than one as an animation and sends a single one as an
/// ordinary frame.
///
/// This is the whole decode. It happens once, when the operator picks a file,
/// and never again: what the tick loop then holds is a table of finished
/// framebuffers, so playing an animation costs a copy and a transfer rather
/// than an LZW pass and a resize per frame. That is also why the ceiling is a
/// frame count rather than a duration, and why it is
/// [`kori_core::display::MAX_ANIMATION_FRAMES`] frames of memory rather than a
/// stream.
///
/// The first frame agrees with [`render`] by construction: a still picture is
/// decoded by the same function, and a GIF's frame zero is blended over the
/// preset's own background here exactly as it is there.
pub fn render_image_frames(
    preset: &DisplayPreset,
    panel: &LcdPanel,
) -> Result<Vec<AnimationFrame>, DisplayError> {
    preset.validate()?;
    let (width, height) = frame::layout_size(panel, preset.orientation)?;

    picture::frames(preset, |picture, delay| {
        let mut canvas = Canvas::filled(width, height, preset.background);
        picture::draw(&mut canvas, picture);
        AnimationFrame {
            frame: Framebuffer::from_canvas(canvas, preset.orientation),
            delay,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{SIDE, at, drawn, image_preset, panel, samples};
    use kori_core::display::Orientation;
    use kori_core::lighting::Rgb;

    #[test]
    fn a_frame_is_exactly_the_panels_size_in_the_panels_format() {
        let preset = DisplayPreset::default_infographic();
        let frame = render(&preset, &samples(Some(50.0), Some(40.0)), &panel()).unwrap();

        assert_eq!(frame.width(), SIDE);
        assert_eq!(frame.height(), SIDE);
        assert_eq!(frame.to_rgb565_be().len(), panel().frame_bytes as usize);
    }

    #[test]
    fn a_solid_field_is_one_color_and_nothing_else() {
        // What makes it usable as a transport test frame: every pixel is the
        // color that was asked for, so an operator watching the panel is
        // reading the transport rather than the renderer. Telemetry is ignored
        // entirely, whatever the samples say, and nothing on the glass names
        // anybody.
        let background = Rgb::new(0x11, 0x22, 0x33);
        let preset = DisplayPreset::solid(background);
        let frame = render(&preset, &samples(Some(99.0), Some(99.0)), &panel()).unwrap();
        assert_eq!(
            drawn(&frame, background),
            0,
            "a solid field drew something over its own color"
        );
    }

    #[test]
    fn the_same_preset_and_sample_always_produce_the_same_bytes() {
        let preset = DisplayPreset::default_infographic();
        let once = render(&preset, &samples(Some(61.0), Some(48.0)), &panel()).unwrap();
        let twice = render(&preset, &samples(Some(61.0), Some(48.0)), &panel()).unwrap();
        assert_eq!(once.to_rgb565_be(), twice.to_rgb565_be());
    }

    #[test]
    fn the_orientation_in_the_preset_reaches_the_frame() {
        let preset = DisplayPreset::default_infographic();
        let of = |preset: &DisplayPreset| {
            render(preset, &samples(Some(70.0), Some(30.0)), &panel()).unwrap()
        };
        let upright = of(&preset);

        let mut turned_preset = preset.clone();
        turned_preset.orientation = Orientation::Deg90;
        let turned = of(&turned_preset);

        assert_ne!(upright, turned, "the picture must actually turn");
        assert_eq!((turned.width(), turned.height()), (SIDE, SIDE));
        // A pixel at the top of the upright picture is at the right of the
        // picture turned a quarter clockwise.
        assert_eq!(at(&upright, 120, 12), at(&turned, SIDE - 1 - 12, 120));

        // And four quarter turns come back to where they started, on a real
        // picture rather than on a synthetic one.
        let mut round_trip = preset.clone();
        for _ in 0..4 {
            round_trip.orientation = round_trip.orientation.rotated();
        }
        assert_eq!(of(&round_trip), upright);
    }

    #[test]
    fn image_mode_without_a_file_is_refused_before_anything_is_drawn() {
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        assert_eq!(
            render(&preset, &samples(None, None), &panel()),
            Err(DisplayError::ImagePathMissing)
        );
        assert_eq!(
            render_image_frames(&preset, &panel()),
            Err(DisplayError::ImagePathMissing)
        );
    }

    #[test]
    fn an_animated_gif_becomes_one_finished_frame_per_picture() {
        let directory = testing::scratch("animated").unwrap();
        let path = directory.join("three.gif");
        let colors = [
            Rgb::new(0xff, 0, 0),
            Rgb::new(0, 0xff, 0),
            Rgb::new(0, 0, 0xff),
        ];
        testing::write_gif(&path, 24, &colors.map(|color| (color, 10))).unwrap();

        let frames = render_image_frames(&image_preset(&path), &panel()).unwrap();
        assert_eq!(frames.len(), 3, "one entry per picture in the file");
        for (frame, expected) in frames.iter().zip(colors) {
            assert_eq!(frame.frame.width(), SIDE);
            assert_eq!(frame.frame.height(), SIDE);
            assert_eq!(
                frame.frame.to_rgb565_be().len(),
                panel().frame_bytes as usize,
                "every frame is exactly the size the transport takes"
            );
            // The picture fills the panel, so the color at the middle is the
            // frame's own rather than the preset's background.
            let middle = at(&frame.frame, SIDE / 2, SIDE / 2);
            assert_eq!(
                (middle.r >> 3, middle.g >> 2, middle.b >> 3),
                (expected.r >> 3, expected.g >> 2, expected.b >> 3),
                "frame {expected:?} did not survive to the framebuffer"
            );
        }
    }

    #[test]
    fn a_still_picture_compiles_to_the_one_frame_the_renderer_draws() {
        // One description, one output, on the compiled path too: the table the
        // daemon plays and the picture the per-tick renderer produces are the
        // same bytes, because one decodes through the other.
        let directory = testing::scratch("still").unwrap();
        let color = Rgb::new(0x20, 0x80, 0xc0);

        let gif = directory.join("one.gif");
        testing::write_gif(&gif, 16, &[(color, 10)]).unwrap();
        // A PNG takes the branch that is not a GIF, which is the one that used
        // to open the file a second time in order to draw it.
        let png = directory.join("one.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([color.r, color.g, color.b, 0xff]))
            .save(&png)
            .unwrap();

        for path in [gif, png] {
            let name = path.display().to_string();
            let preset = image_preset(&path);
            let frames = render_image_frames(&preset, &panel()).unwrap();
            assert_eq!(frames.len(), 1, "{name} holds still");

            let drawn = render(&preset, &samples(None, None), &panel()).unwrap();
            assert_eq!(
                frames[0].frame.to_rgb565_be(),
                drawn.to_rgb565_be(),
                "{name}: the compiled frame and the drawn one are not the same picture"
            );
        }
    }

    #[test]
    fn no_frame_is_laid_out_when_the_panel_geometry_is_unknown() {
        let directory = testing::scratch("nopanel").unwrap();
        let path = directory.join("any.gif");
        testing::write_gif(&path, 4, &[(Rgb::new(1, 2, 3), 10)]).unwrap();

        let mut unknown = panel();
        unknown.width = 0;
        assert_eq!(
            render_image_frames(&image_preset(&path), &unknown),
            Err(DisplayError::PanelUnknown)
        );
        assert_eq!(
            render(
                &DisplayPreset::default_infographic(),
                &samples(None, None),
                &unknown
            ),
            Err(DisplayError::PanelUnknown)
        );
    }

    #[test]
    fn a_preview_repaint_stays_inside_the_frame_budget() {
        // The preview gets 16.7 ms at P95, which is the budget for the shipped
        // build. An unoptimized build is an order of magnitude slower
        // for reasons that have nothing to do with this code, so it is held to
        // a ceiling that still catches a real regression rather than to a
        // number it was never meant to meet.
        let budget_us: u128 = if cfg!(debug_assertions) {
            60_000
        } else {
            16_700
        };

        let panel = panel();
        let preset = DisplayPreset::default_infographic();
        let mut timings = Vec::new();
        for step in 0..60 {
            // A moving reading, so no repaint can be served from a value that
            // happened to be identical to the previous one.
            let samples = samples(
                Some(30.0 + (step % 70) as f32),
                Some(25.0 + (step % 60) as f32),
            );
            let started = std::time::Instant::now();
            let frame = render(&preset, &samples, &panel).unwrap();
            let png = frame.to_png().unwrap();
            timings.push(started.elapsed().as_micros());
            assert!(!png.is_empty());
        }

        timings.sort_unstable();
        let p95 = timings[(timings.len() - 1) * 95 / 100];
        assert!(
            p95 <= budget_us,
            "a repaint took {p95} us at P95, above the {budget_us} us budget"
        );
    }
}
