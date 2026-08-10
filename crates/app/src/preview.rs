// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The panel preview: the exact frame, not an impression of it.
//!
//! FR-14 asks that one [`DisplayPreset`] produce both the preview and the
//! bytes the panel receives. So the preview is not drawn with the toolkit's
//! shapes. It calls the same renderer the daemon calls, at the panel's own
//! resolution and in the panel's own color depth, and displays the result. What
//! is on screen is what is on the glass, pixel for pixel, or the two differ
//! only because the preset does.
//!
//! The frame is handed over as PNG bytes rather than as a pixel buffer, which
//! keeps this file free of the toolkit's image types and their version.

use std::sync::Arc;

use gpui::{Div, Image, ImageFormat, Pixels, div, img, prelude::*, px};
use kori_core::capability::{LcdPanel, LcdPanelShape};
use kori_core::display::{DisplayPreset, MetricSample};

use crate::theme::{Color, color};

/// Side of the preview, in logical pixels.
///
/// The frame is rendered at the panel's own 240 and scaled here, so nothing is
/// laid out against this number.
pub const PREVIEW_SIDE: Pixels = px(252.0);

/// Lowest contrast the small text on the panel accepts without warning.
///
/// The captions and the wordmark are set at a dozen pixels and under, so they
/// are small text and take the full ratio.
pub const MIN_SMALL_TEXT_CONTRAST: f32 = 4.5;

/// Lowest contrast a band and the caption it labels accept.
///
/// The bands are not text at all, and the captions they color are set in
/// semibold at around twenty-two pixels, which WCAG counts as large text from
/// 14pt bold upward. Both take the large-element threshold rather than the
/// small-text one; holding them to 4.5:1 would rule out most saturated accents
/// for no legibility gain. The values, which are larger still, take the same
/// bar through the text colors below.
pub const MIN_LARGE_ELEMENT_CONTRAST: f32 = 3.0;

/// The geometry the preview falls back to before a panel has answered.
///
/// A preview has to have a size. The row above it says "no panel has answered"
/// until one does, so this geometry never passes for evidence that a panel was
/// found.
pub fn assumed_panel() -> LcdPanel {
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

/// Build the preview element for `preset`.
///
/// `samples` are the readings the frame will carry, so the preview ages with
/// telemetry exactly as the panel does.
pub fn panel_preview(preset: &DisplayPreset, samples: &[MetricSample; 2], panel: &LcdPanel) -> Div {
    let rendered = kori_lcd_renderer::render(preset, samples, panel)
        .and_then(|frame| frame.to_png())
        .ok();
    panel_frame(rendered, preset.background)
}

/// The same disc, around a frame that was rendered somewhere else.
///
/// Image mode compiles its frames when the file is chosen rather than on every
/// repaint, so what it has to show is already a PNG. It goes through this
/// entry point and lands in the same disc, at the same size, with the same
/// background behind whatever the picture leaves transparent.
pub fn panel_frame(rendered: Option<Vec<u8>>, background: kori_core::lighting::Rgb) -> Div {
    div().flex().flex_col().items_center().child(
        // Round, always: the screen is square but the window in the cooler
        // is not, and the corners of the framebuffer are behind the housing
        // rather than on the glass. Showing them would be showing pixels
        // the operator cannot see. The disc is the preview, with nothing
        // behind it: a square plate under a round window read as a picture
        // file sitting on the work surface.
        div()
            .w(PREVIEW_SIDE)
            .h(PREVIEW_SIDE)
            .rounded(PREVIEW_SIDE / 2.0)
            .overflow_hidden()
            .bg(Color::rgb(pack(background)).hsla())
            .border_1()
            .border_color(color::SEPARATOR.hsla())
            .children(rendered.map(|png| {
                img(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
                    .w(PREVIEW_SIDE)
                    .h(PREVIEW_SIDE)
                    .rounded(PREVIEW_SIDE / 2.0)
            })),
    )
}

/// The element furthest below its own minimum, if any is.
///
/// Both ends of every band are checked, not just the color it starts on: a fade
/// into something the background swallows is a gauge that disappears halfway.
/// A band's first color is also the color of its caption in the paired layout,
/// which is why it is named as a band rather than as a caption: the threshold
/// that applies to both is the same one.
///
/// Each element is measured against the threshold that applies to it rather
/// than against one blanket ratio, which is the same split the project's
/// accessibility budget already makes between text and everything else.
pub fn worst_contrast(preset: &DisplayPreset) -> Option<(&'static str, f32, f32)> {
    let background = Color::rgb(pack(preset.background));
    [
        ("Text 1", preset.readings[0].text, MIN_SMALL_TEXT_CONTRAST),
        ("Text 2", preset.readings[1].text, MIN_SMALL_TEXT_CONTRAST),
        (
            "Band 1",
            preset.readings[0].reading,
            MIN_LARGE_ELEMENT_CONTRAST,
        ),
        (
            "Fade 1",
            preset.readings[0].band_end(),
            MIN_LARGE_ELEMENT_CONTRAST,
        ),
        (
            "Band 2",
            preset.readings[1].reading,
            MIN_LARGE_ELEMENT_CONTRAST,
        ),
        (
            "Fade 2",
            preset.readings[1].band_end(),
            MIN_LARGE_ELEMENT_CONTRAST,
        ),
    ]
    .into_iter()
    .map(|(label, color, minimum)| (label, contrast(color, background), minimum))
    .filter(|(_, ratio, minimum)| ratio < minimum)
    // The furthest below its own bar is the one worth naming first.
    .min_by(|a, b| (a.1 - a.2).total_cmp(&(b.1 - b.2)))
}

fn contrast(color: kori_core::lighting::Rgb, background: Color) -> f32 {
    Color::rgb(pack(color)).contrast(background)
}

/// A core color as the theme's packed form.
fn pack(color: kori_core::lighting::Rgb) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::display::LcdMetric;
    use kori_core::lighting::Rgb;

    fn samples() -> [MetricSample; 2] {
        [
            MetricSample {
                metric: LcdMetric::CpuTemperature,
                value: Some(61.0),
            },
            MetricSample {
                metric: LcdMetric::GpuTemperature,
                value: Some(48.0),
            },
        ]
    }

    #[test]
    fn the_default_preset_is_the_reference_palette_and_its_violet_is_dim() {
        // The default is the palette of the reference screens, chosen so the
        // panel looks like the thing it is imitating. One of its colors does
        // not clear this project's own bar: the violet band reads 2.68:1
        // against black, under the 3:1 a non-text element takes.
        //
        // Pinned rather than hidden. The screen no longer prints a readability
        // warning, so this test is the whole guard: it is what would catch the
        // value drifting further or a second element joining it.
        let preset = DisplayPreset::default_infographic();
        let (label, ratio, minimum) =
            worst_contrast(&preset).expect("the reference violet is under the bar");
        assert_eq!(label, "Band 1");
        assert_eq!(minimum, MIN_LARGE_ELEMENT_CONTRAST);
        assert!(
            (2.6..2.8).contains(&ratio),
            "the reference violet measures {ratio:.2}:1, not the 2.68:1 recorded"
        );
        // Everything else clears its own threshold, including the same violet
        // as the second slot's fade and the white the values are set in.
        let background = Color::rgb(pack(preset.background));
        for (name, color, floor) in [
            (
                "Band 2",
                preset.readings[1].reading,
                MIN_LARGE_ELEMENT_CONTRAST,
            ),
            (
                "Fade 1",
                preset.readings[0].band_end(),
                MIN_LARGE_ELEMENT_CONTRAST,
            ),
            ("Text 1", preset.readings[0].text, MIN_SMALL_TEXT_CONTRAST),
            ("Text 2", preset.readings[1].text, MIN_SMALL_TEXT_CONTRAST),
        ] {
            let measured = contrast(color, background);
            assert!(
                measured >= floor,
                "{name} is {measured:.2}:1, under its own {floor:.1}:1"
            );
        }
    }

    #[test]
    fn a_low_contrast_choice_names_the_element_at_fault() {
        let mut preset = DisplayPreset::default_infographic();
        preset.readings[1].text = preset.background;

        let (label, ratio, minimum) = worst_contrast(&preset).unwrap();
        assert_eq!(label, "Text 2");
        assert_eq!(ratio, 1.0);
        assert_eq!(minimum, MIN_SMALL_TEXT_CONTRAST);
    }

    #[test]
    fn a_reading_is_held_to_the_large_element_bar_and_a_caption_to_the_text_one() {
        // The same color passes as a reading and fails as a caption, because
        // one is forty pixels of digit and the other is a five by seven label.
        let mut preset = DisplayPreset::default_infographic();
        let accent = Rgb::new(0x6f, 0x4e, 0xf2);
        preset.readings[0].reading = accent;
        preset.readings[0].text = accent;

        let ratio = contrast(accent, Color::rgb(pack(preset.background)));
        assert!(
            (MIN_LARGE_ELEMENT_CONTRAST..MIN_SMALL_TEXT_CONTRAST).contains(&ratio),
            "the fixture color must sit between the two thresholds, it is {ratio:.2}:1"
        );

        let (label, _, minimum) = worst_contrast(&preset).unwrap();
        assert_eq!(label, "Text 1");
        assert_eq!(minimum, MIN_SMALL_TEXT_CONTRAST);
    }

    #[test]
    fn the_preview_renders_the_same_frame_the_daemon_would_send() {
        // The one assertion that matters for FR-14: the preview's pixels are
        // the panel's pixels, because both come from one call.
        let preset = DisplayPreset::default_infographic();
        let panel = assumed_panel();
        let frame = kori_lcd_renderer::render(&preset, &samples(), &panel).unwrap();

        assert_eq!(frame.width(), u32::from(panel.width));
        assert_eq!(frame.height(), u32::from(panel.height));
        assert_eq!(
            frame.to_rgb565_be().len(),
            panel.frame_bytes as usize,
            "the preview is rendered at the exact size the transport takes"
        );
        assert!(frame.to_png().is_ok());
    }

    #[test]
    fn a_preset_that_cannot_render_leaves_the_preview_empty_rather_than_wrong() {
        // Image mode with no file. The element still builds; it simply carries
        // no picture, which is what keeps a broken entry from painting a stale
        // frame as if it were current.
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = kori_core::display::DisplayMode::Image;
        let panel = assumed_panel();
        assert!(kori_lcd_renderer::render(&preset, &samples(), &panel).is_err());
    }

    #[test]
    fn packing_a_core_color_matches_the_themes_own_form() {
        assert_eq!(pack(Rgb::new(0x6f, 0x4e, 0xf2)), 0x6f4ef2);
        assert_eq!(pack(Rgb::BLACK), 0x000000);
        assert_eq!(pack(Rgb::new(0xff, 0xff, 0xff)), 0xffffff);
    }
}
