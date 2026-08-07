// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! One [`DisplayPreset`] in, one exact framebuffer out.
//!
//! FR-14 asks that the editor's preview and the bytes the panel receives come
//! from the same description. This crate is how: the GPUI client renders a
//! preset to see it, the daemon renders the same preset to send it, and neither
//! extracts pixels from the other. A difference between what the window shows
//! and what the panel shows could then only be a difference in the preset.
//!
//! Nothing here opens a device or knows a report identifier. The only input
//! from outside the preset is a telemetry sample and, in image mode, a file the
//! operator picked.

pub mod canvas;
pub mod font;

use nzxt_core::PRODUCT_NAME;
use nzxt_core::capability::{LcdPanel, LcdPanelShape};
use nzxt_core::display::{
    DisplayError, DisplayMode, DisplayPreset, MAX_IMAGE_DIMENSION, MetricSample, Orientation,
};
use nzxt_core::lighting::Rgb;

use image::ImageEncoder;

use canvas::{Arc, Canvas};

/// A rendered panel picture, at exactly the panel's own size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Vec<Rgb>,
}

impl Framebuffer {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Straight RGB8, one entry per pixel, for the client's preview.
    pub fn pixels(&self) -> &[Rgb] {
        &self.pixels
    }

    /// The bytes the panel takes: five bits of red, six of green, five of
    /// blue, most significant byte first.
    ///
    /// The truncation is what the format is, not a loss to work around: the
    /// panel has no more bits. Doing it here rather than in the transport keeps
    /// the preview and the device looking at the same quantized colors.
    pub fn to_rgb565_be(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.pixels.len() * 2);
        for pixel in &self.pixels {
            let red = u16::from(pixel.r >> 3);
            let green = u16::from(pixel.g >> 2);
            let blue = u16::from(pixel.b >> 3);
            let packed = (red << 11) | (green << 5) | blue;
            bytes.extend_from_slice(&packed.to_be_bytes());
        }
        bytes
    }

    /// The frame as a PNG, at the panel's own size and colors.
    ///
    /// This is what the client's preview displays. Encoding rather than handing
    /// over a pixel buffer keeps the client free of the toolkit's image types
    /// and their version, and it means the preview shows the quantized colors
    /// the panel will actually produce rather than the full-depth ones the
    /// renderer worked in.
    pub fn to_png(&self) -> Result<Vec<u8>, DisplayError> {
        let mut flat = Vec::with_capacity(self.pixels.len() * 3);
        for pixel in self.quantized() {
            flat.extend_from_slice(&[pixel.r, pixel.g, pixel.b]);
        }

        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut encoded,
            // The preview re-encodes on every repaint and has a 16.7 ms budget,
            // so the encoder is asked for speed rather than for the smallest
            // file: nothing here is stored or sent anywhere.
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        )
        .write_image(
            &flat,
            self.width,
            self.height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| DisplayError::ImageUndecodable {
            path: "the rendered frame".to_string(),
            detail: error.to_string(),
        })?;
        Ok(encoded)
    }

    /// The same colors the panel will show, for a preview that does not lie.
    ///
    /// A preview drawn from the full-depth pixels would show gradients the
    /// hardware cannot reproduce, so the client asks for these instead.
    pub fn quantized(&self) -> Vec<Rgb> {
        self.pixels
            .iter()
            .map(|pixel| Rgb {
                r: (pixel.r >> 3) << 3,
                g: (pixel.g >> 2) << 2,
                b: (pixel.b >> 3) << 3,
            })
            .collect()
    }
}

/// Render `preset` against `samples` at the panel's exact geometry.
///
/// The samples are only consulted by the modes that draw readings; a solid
/// field or a static image ignores them, so a panel showing a picture keeps
/// showing it when a collector drops out.
pub fn render(
    preset: &DisplayPreset,
    samples: &[MetricSample; 2],
    panel: &LcdPanel,
) -> Result<Framebuffer, DisplayError> {
    preset.validate()?;
    if panel.width == 0 || panel.height == 0 {
        return Err(DisplayError::PanelUnknown);
    }

    // The content is laid out at the size it will occupy *before* rotation, so
    // a quarter turn on a non-square panel composes rather than crops.
    let quarter = preset.orientation.quarter_turns();
    let (width, height) = if quarter % 2 == 1 {
        (u32::from(panel.height), u32::from(panel.width))
    } else {
        (u32::from(panel.width), u32::from(panel.height))
    };

    let mut canvas = Canvas::filled(width, height, preset.background);
    match preset.mode {
        DisplayMode::Solid => draw_wordmark(&mut canvas, preset),
        DisplayMode::Image => {
            draw_image(&mut canvas, preset)?;
        }
        DisplayMode::DualInfographic => {
            draw_infographic(&mut canvas, preset, samples, panel.shape);
            draw_wordmark(&mut canvas, preset);
        }
    }

    let canvas = rotate(&canvas, preset.orientation);
    Ok(Framebuffer {
        width: canvas.width(),
        height: canvas.height(),
        pixels: canvas.pixels().to_vec(),
    })
}

/// Where each element sits, as a fraction of the panel's smaller side.
///
/// Fractions rather than pixels so the layout survives a panel of another size
/// without a second set of constants to keep in step.
mod layout {
    /// Outer and inner radius of the gauge tracks.
    pub const TRACK_OUTER: f32 = 0.950;
    pub const TRACK_INNER: f32 = 0.845;
    /// Thinner band an unavailable reading falls back to.
    pub const TRACK_UNAVAILABLE_INNER: f32 = 0.900;

    /// Each track is centered on its half of the dial with a gap either side.
    pub const TRACK_SWEEP_TURNS: f32 = 0.44;

    /// Top edge and height of each reading, and of the text around them.
    ///
    /// Each caption sits against its own reading, on the outside of the pair,
    /// and the wordmark holds the middle alone. Grouped the other way the three
    /// lines of text ran together and the wordmark read as a third caption.
    pub const CAPTION_ONE_TOP: f32 = 0.208;
    pub const VALUE_ONE_TOP: f32 = 0.283;
    pub const WORDMARK_TOP: f32 = 0.479;
    pub const VALUE_TWO_TOP: f32 = 0.567;
    pub const CAPTION_TWO_TOP: f32 = 0.750;
    pub const VALUE_HEIGHT: f32 = 0.167;

    /// Room between a reading and its unit.
    pub const UNIT_GAP: f32 = 0.025;
}

/// Draw both gauges, both readings and both captions.
fn draw_infographic(
    canvas: &mut Canvas,
    preset: &DisplayPreset,
    samples: &[MetricSample; 2],
    shape: LcdPanelShape,
) {
    let width = canvas.width() as f32;
    let height = canvas.height() as f32;
    let center = (width / 2.0, height / 2.0);
    let radius = width.min(height) / 2.0;

    for (index, sample) in samples.iter().enumerate() {
        let slot = &preset.readings[index];
        // The first gauge occupies the top of the dial, the second the bottom,
        // each centered on its half with an equal gap at nine and three
        // o'clock. On a circular panel that keeps both fully on the glass.
        let center_turn = if index == 0 { 0.0 } else { 0.5 };
        let start = center_turn - layout::TRACK_SWEEP_TURNS / 2.0;

        match sample.fraction() {
            Some(fraction) => {
                canvas.fill_arc(
                    Arc {
                        center,
                        inner: radius * layout::TRACK_INNER,
                        outer: radius * layout::TRACK_OUTER,
                        start_turns: start,
                        sweep_turns: layout::TRACK_SWEEP_TURNS,
                    },
                    mix(preset.background, slot.reading, 0.18),
                );
                canvas.fill_arc(
                    Arc {
                        center,
                        inner: radius * layout::TRACK_INNER,
                        outer: radius * layout::TRACK_OUTER,
                        start_turns: start,
                        sweep_turns: layout::TRACK_SWEEP_TURNS * fraction,
                    },
                    slot.reading,
                );
            }
            // An unavailable reading gets a thinner, colorless band. The shape
            // carries the meaning as well as the color does, which is what
            // keeps it readable without relying on hue.
            None => canvas.fill_arc(
                Arc {
                    center,
                    inner: radius * layout::TRACK_UNAVAILABLE_INNER,
                    outer: radius * layout::TRACK_OUTER,
                    start_turns: start,
                    sweep_turns: layout::TRACK_SWEEP_TURNS,
                },
                mix(preset.background, slot.text, 0.28),
            ),
        }

        let value_top = height
            * if index == 0 {
                layout::VALUE_ONE_TOP
            } else {
                layout::VALUE_TWO_TOP
            };
        draw_reading(
            canvas,
            &sample.text(),
            sample.metric.unit(),
            center.0,
            value_top,
            height * layout::VALUE_HEIGHT,
            slot.reading,
            slot.text,
            caption_scale(height),
        );

        let caption_top = height
            * if index == 0 {
                layout::CAPTION_ONE_TOP
            } else {
                layout::CAPTION_TWO_TOP
            };
        font::draw_text_centered(
            canvas,
            sample.metric.caption(),
            center.0,
            caption_top.round() as i32,
            caption_scale(height),
            slot.text,
        );
    }

    // A square panel shows its corners, so the dial gets a boundary it does
    // not need on a circular one.
    if shape == LcdPanelShape::Square {
        canvas.fill_arc(
            Arc {
                center,
                inner: radius * layout::TRACK_OUTER,
                outer: radius * layout::TRACK_OUTER + 1.0,
                start_turns: 0.0,
                sweep_turns: 1.0,
            },
            mix(preset.background, preset.logo, 0.35),
        );
    }
}

/// One reading and its unit, centered together.
///
/// The pair is centered as a group rather than the digits alone, so a reading
/// does not shift sideways when it gains a digit.
#[allow(clippy::too_many_arguments)]
fn draw_reading(
    canvas: &mut Canvas,
    value: &str,
    unit: &str,
    center_x: f32,
    top: f32,
    height: f32,
    value_color: Rgb,
    unit_color: Rgb,
    unit_scale: u32,
) {
    let value_width = font::seven_segment_width(value, height);
    let unit_width = font::text_width(unit, unit_scale) as f32;
    let gap = height * layout::UNIT_GAP;
    let total = value_width + gap + unit_width;
    let value_center = center_x - total / 2.0 + value_width / 2.0;

    font::draw_seven_segment_centered(canvas, value, value_center, top, height, value_color);
    font::draw_text(
        canvas,
        unit,
        (center_x + total / 2.0 - unit_width).round() as i32,
        top.round() as i32,
        unit_scale,
        unit_color,
    );
}

/// The project's own wordmark, never a vendor's.
fn draw_wordmark(canvas: &mut Canvas, preset: &DisplayPreset) {
    let height = canvas.height() as f32;
    font::draw_text_centered(
        canvas,
        &PRODUCT_NAME.to_ascii_uppercase(),
        canvas.width() as f32 / 2.0,
        (height * layout::WORDMARK_TOP).round() as i32,
        caption_scale(height),
        preset.logo,
    );
}

/// Scale the small face is drawn at on a panel of this height.
fn caption_scale(height: f32) -> u32 {
    ((height / 120.0).floor() as u32).max(1)
}

/// `base` moved `amount` of the way toward `toward`.
fn mix(base: Rgb, toward: Rgb, amount: f32) -> Rgb {
    let blend = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Rgb {
        r: blend(base.r, toward.r),
        g: blend(base.g, toward.g),
        b: blend(base.b, toward.b),
    }
}

/// Decode the operator's image and cover the panel with it.
///
/// The declared dimensions are checked before anything is decoded, so a file
/// claiming an enormous size is refused rather than allocated for. Every
/// failure is typed and leaves the canvas as it was, which is what keeps a
/// partial picture from becoming a frame.
fn draw_image(canvas: &mut Canvas, preset: &DisplayPreset) -> Result<(), DisplayError> {
    let Some(path) = preset.image.as_ref() else {
        return Err(DisplayError::ImagePathMissing);
    };
    let name = path.display().to_string();
    let undecodable = |detail: String| DisplayError::ImageUndecodable {
        path: name.clone(),
        detail,
    };

    let reader = image::ImageReader::open(path)
        .map_err(|error| undecodable(error.to_string()))?
        .with_guessed_format()
        .map_err(|error| undecodable(error.to_string()))?;
    let (source_width, source_height) = reader
        .into_dimensions()
        .map_err(|error| undecodable(error.to_string()))?;
    if source_width > MAX_IMAGE_DIMENSION || source_height > MAX_IMAGE_DIMENSION {
        return Err(DisplayError::ImageTooLarge {
            width: source_width,
            height: source_height,
            max: MAX_IMAGE_DIMENSION,
        });
    }
    if source_width == 0 || source_height == 0 {
        return Err(undecodable("the image has no pixels".to_string()));
    }

    let decoded = image::ImageReader::open(path)
        .map_err(|error| undecodable(error.to_string()))?
        .with_guessed_format()
        .map_err(|error| undecodable(error.to_string()))?
        .decode()
        .map_err(|error| undecodable(error.to_string()))?
        .to_rgb8();

    // Cover: the shorter side fills the panel and the longer one is trimmed
    // evenly, so a wide photograph is not squeezed into a circle.
    let scale = (canvas.width() as f32 / source_width as f32)
        .max(canvas.height() as f32 / source_height as f32);
    let offset_x = (source_width as f32 * scale - canvas.width() as f32) / 2.0;
    let offset_y = (source_height as f32 * scale - canvas.height() as f32) / 2.0;

    for y in 0..canvas.height() {
        for x in 0..canvas.width() {
            let source_x = (((x as f32 + 0.5 + offset_x) / scale) as u32).min(source_width - 1);
            let source_y = (((y as f32 + 0.5 + offset_y) / scale) as u32).min(source_height - 1);
            let pixel = decoded.get_pixel(source_x, source_y);
            canvas.blend(
                x as i32,
                y as i32,
                Rgb::new(pixel[0], pixel[1], pixel[2]),
                1.0,
            );
        }
    }
    Ok(())
}

/// Turn the finished picture, which is where the only rotation happens.
///
/// The panel is left on its own orientation zero, so this is the single place
/// a quarter turn is applied and the preview cannot disagree with the glass.
fn rotate(canvas: &Canvas, orientation: Orientation) -> Canvas {
    let (width, height) = (canvas.width(), canvas.height());
    let quarter = orientation.quarter_turns();
    if quarter == 0 {
        return canvas.clone();
    }
    let (target_width, target_height) = if quarter % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    };

    let mut turned = Canvas::filled(target_width, target_height, Rgb::BLACK);
    for y in 0..target_height {
        for x in 0..target_width {
            let (source_x, source_y) = match quarter {
                1 => (y, height - 1 - x),
                2 => (width - 1 - x, height - 1 - y),
                _ => (width - 1 - y, x),
            };
            if let Some(pixel) = canvas.pixel(source_x, source_y) {
                turned.blend(x as i32, y as i32, pixel, 1.0);
            }
        }
    }
    turned
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::capability::LcdPanelShape;
    use nzxt_core::display::{LcdMetric, ReadingSlot};

    fn panel() -> LcdPanel {
        LcdPanel {
            width: 240,
            height: 240,
            shape: LcdPanelShape::Circular,
            pixel_format: "RGB565 big-endian".into(),
            frame_bytes: 240 * 240 * 2,
            bulk_endpoint: 0x02,
            bulk_interface: 0,
        }
    }

    fn samples(first: Option<f32>, second: Option<f32>) -> [MetricSample; 2] {
        [
            MetricSample {
                metric: LcdMetric::CpuTemperature,
                value: first,
            },
            MetricSample {
                metric: LcdMetric::GpuTemperature,
                value: second,
            },
        ]
    }

    /// How many pixels differ from the preset's background.
    fn drawn(frame: &Framebuffer, background: Rgb) -> usize {
        frame.pixels().iter().filter(|p| **p != background).count()
    }

    #[test]
    fn a_frame_is_exactly_the_panels_size_in_the_panels_format() {
        let preset = DisplayPreset::default_infographic();
        let frame = render(&preset, &samples(Some(50.0), Some(40.0)), &panel()).unwrap();

        assert_eq!(frame.width(), 240);
        assert_eq!(frame.height(), 240);
        assert_eq!(frame.pixels().len(), 240 * 240);
        assert_eq!(frame.to_rgb565_be().len(), panel().frame_bytes as usize);
    }

    #[test]
    fn rgb565_packs_five_six_five_most_significant_byte_first() {
        let mut preset = DisplayPreset::solid(Rgb::new(0xff, 0x00, 0x00));
        preset.logo = Rgb::new(0xff, 0x00, 0x00);
        let frame = render(&preset, &samples(None, None), &panel()).unwrap();
        let bytes = frame.to_rgb565_be();
        // Pure red is five ones in the top bits: 0xF800.
        assert_eq!(&bytes[0..2], &[0xf8, 0x00]);

        let mut blue = DisplayPreset::solid(Rgb::new(0x00, 0x00, 0xff));
        blue.logo = Rgb::new(0x00, 0x00, 0xff);
        let bytes = render(&blue, &samples(None, None), &panel())
            .unwrap()
            .to_rgb565_be();
        assert_eq!(&bytes[0..2], &[0x00, 0x1f]);

        let mut green = DisplayPreset::solid(Rgb::new(0x00, 0xff, 0x00));
        green.logo = Rgb::new(0x00, 0xff, 0x00);
        let bytes = render(&green, &samples(None, None), &panel())
            .unwrap()
            .to_rgb565_be();
        assert_eq!(&bytes[0..2], &[0x07, 0xe0]);
    }

    #[test]
    fn the_preview_shows_the_colors_the_panel_can_actually_reproduce() {
        let preset = DisplayPreset::solid(Rgb::new(0xff, 0xff, 0xff));
        let frame = render(&preset, &samples(None, None), &panel()).unwrap();
        let quantized = frame.quantized();
        // White survives quantization exactly; a value the panel cannot hold
        // is shown as the value it will hold.
        assert_eq!(quantized[0], Rgb::new(0xf8, 0xfc, 0xf8));
        assert_eq!(quantized.len(), frame.pixels().len());
    }

    #[test]
    fn a_solid_field_carries_the_wordmark_and_nothing_else() {
        let background = Rgb::new(0x11, 0x22, 0x33);
        let preset = DisplayPreset::solid(background);
        let frame = render(&preset, &samples(Some(99.0), Some(99.0)), &panel()).unwrap();

        let marked = drawn(&frame, background);
        assert!(marked > 0, "the wordmark must be drawn");
        // The readings are not: a solid field ignores telemetry entirely, which
        // is what makes it usable as a transport test frame.
        assert!(
            marked < 240 * 240 / 10,
            "a solid field drew {marked} pixels, which is a gauge, not a wordmark"
        );
    }

    #[test]
    fn a_higher_reading_fills_more_of_its_gauge() {
        let preset = DisplayPreset::default_infographic();
        let mut filled = Vec::new();
        for value in [0.0, 25.0, 50.0, 100.0] {
            let frame = render(&preset, &samples(Some(value), Some(0.0)), &panel()).unwrap();
            filled.push(
                frame
                    .pixels()
                    .iter()
                    .filter(|p| **p == preset.readings[0].reading)
                    .count(),
            );
        }
        assert!(
            filled.windows(2).all(|pair| pair[1] > pair[0]),
            "gauge fill did not grow with the reading: {filled:?}"
        );
    }

    #[test]
    fn an_unavailable_reading_shows_dashes_and_never_a_zero_gauge() {
        let preset = DisplayPreset::default_infographic();
        let missing = render(&preset, &samples(None, Some(40.0)), &panel()).unwrap();
        let zero = render(&preset, &samples(Some(0.0), Some(40.0)), &panel()).unwrap();

        assert_ne!(
            missing, zero,
            "an unavailable reading must not render the same as a reading of zero"
        );
        // Inside the dial the dashes are still drawn in the reading's color,
        // which is right: it is the gauge that must lose it. Counting only the
        // ring is what separates the two.
        let in_ring = |frame: &Framebuffer, color: Rgb| {
            frame
                .pixels()
                .iter()
                .enumerate()
                .filter(|(index, pixel)| {
                    let (x, y) = ((index % 240) as f32, (index / 240) as f32);
                    let distance = ((x - 120.0).powi(2) + (y - 120.0).powi(2)).sqrt();
                    distance > 120.0 * 0.845 && **pixel == color
                })
                .count()
        };
        assert_eq!(
            in_ring(&missing, preset.readings[0].reading),
            0,
            "the unavailable gauge kept the reading color"
        );
        assert!(
            in_ring(
                &zero,
                mix(preset.background, preset.readings[0].reading, 0.18)
            ) > 0,
            "a reading of zero still shows its own track, in its own color"
        );

        // The dashes are the second signal, so the state does not depend on
        // hue alone.
        assert!(
            missing.pixels().contains(&preset.readings[0].reading),
            "the unavailable marker is still drawn in the reading's color"
        );
    }

    #[test]
    fn both_readings_are_drawn_and_each_uses_its_own_colors() {
        let mut preset = DisplayPreset::default_infographic();
        preset.readings[0] = ReadingSlot {
            metric: LcdMetric::CpuTemperature,
            reading: Rgb::new(0xff, 0x00, 0x00),
            text: Rgb::new(0x00, 0xff, 0x00),
        };
        preset.readings[1] = ReadingSlot {
            metric: LcdMetric::GpuTemperature,
            reading: Rgb::new(0x00, 0x00, 0xff),
            text: Rgb::new(0xff, 0xff, 0x00),
        };
        let frame = render(&preset, &samples(Some(80.0), Some(80.0)), &panel()).unwrap();

        for color in [
            preset.readings[0].reading,
            preset.readings[0].text,
            preset.readings[1].reading,
            preset.readings[1].text,
            preset.logo,
        ] {
            assert!(
                frame.pixels().contains(&color),
                "{color:?} was selected but never drawn"
            );
        }
    }

    #[test]
    fn the_two_gauges_occupy_opposite_halves_of_the_dial() {
        let mut preset = DisplayPreset::default_infographic();
        preset.readings[0].reading = Rgb::new(0xff, 0x00, 0x00);
        preset.readings[1].reading = Rgb::new(0x00, 0x00, 0xff);
        let frame = render(&preset, &samples(Some(100.0), Some(100.0)), &panel()).unwrap();

        let row_of = |color: Rgb| -> f32 {
            let rows: Vec<u32> = frame
                .pixels()
                .iter()
                .enumerate()
                .filter(|(_, p)| **p == color)
                .map(|(index, _)| index as u32 / 240)
                .collect();
            rows.iter().sum::<u32>() as f32 / rows.len() as f32
        };
        assert!(
            row_of(Rgb::new(0xff, 0x00, 0x00)) < 120.0,
            "the first gauge belongs to the top half"
        );
        assert!(
            row_of(Rgb::new(0x00, 0x00, 0xff)) > 120.0,
            "the second gauge belongs to the bottom half"
        );
    }

    #[test]
    fn a_quarter_turn_moves_the_picture_and_keeps_every_pixel() {
        let preset = DisplayPreset::default_infographic();
        let upright = render(&preset, &samples(Some(70.0), Some(30.0)), &panel()).unwrap();

        let mut turned_preset = preset.clone();
        turned_preset.orientation = Orientation::Deg90;
        let turned = render(&turned_preset, &samples(Some(70.0), Some(30.0)), &panel()).unwrap();

        assert_ne!(upright, turned, "the picture must actually turn");
        assert_eq!(turned.width(), 240);
        assert_eq!(turned.height(), 240);

        // A pixel at the top of the upright picture is at the right of the
        // picture turned a quarter clockwise.
        let at = |frame: &Framebuffer, x: u32, y: u32| frame.pixels()[(y * 240 + x) as usize];
        assert_eq!(at(&upright, 120, 12), at(&turned, 240 - 1 - 12, 120));
    }

    #[test]
    fn four_quarter_turns_return_the_picture_unchanged() {
        let preset = DisplayPreset::default_infographic();
        let mut orientation = Orientation::Deg0;
        let first = render(&preset, &samples(Some(55.0), Some(45.0)), &panel()).unwrap();
        for _ in 0..4 {
            orientation = orientation.rotated();
        }
        let mut round_trip = preset.clone();
        round_trip.orientation = orientation;
        assert_eq!(
            render(&round_trip, &samples(Some(55.0), Some(45.0)), &panel()).unwrap(),
            first
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
    fn image_mode_without_a_file_is_refused_before_anything_is_drawn() {
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        assert_eq!(
            render(&preset, &samples(None, None), &panel()),
            Err(DisplayError::ImagePathMissing)
        );
    }

    #[test]
    fn a_file_that_is_not_an_image_is_rejected_without_panicking() {
        let directory = std::env::temp_dir().join(format!("nzxt-lcd-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("not-an-image.png");
        std::fs::write(&path, b"this is not a PNG, whatever the extension says").unwrap();

        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        preset.image = Some(path.clone());
        match render(&preset, &samples(None, None), &panel()) {
            Err(DisplayError::ImageUndecodable { path: named, .. }) => {
                assert!(named.ends_with("not-an-image.png"), "{named}");
            }
            other => panic!("expected a decode refusal, got {other:?}"),
        }

        // A file that is not there at all fails the same way.
        preset.image = Some(directory.join("absent.png"));
        assert!(matches!(
            render(&preset, &samples(None, None), &panel()),
            Err(DisplayError::ImageUndecodable { .. })
        ));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_image_larger_than_the_ceiling_is_refused_before_it_is_decoded() {
        let directory = std::env::temp_dir().join(format!("nzxt-lcd-big-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("enormous.png");

        // A structurally valid PNG declaring 9000 by 9000 with no pixel data
        // behind it. Forty-five bytes on disk, so the refusal cannot be coming
        // from the file being large; it comes from the size it claims. A build
        // that decoded first would have to allocate 243 MB to reach the same
        // verdict, which is the mistake this pins.
        let mut png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut ihdr: Vec<u8> = b"IHDR".to_vec();
        ihdr.extend_from_slice(&9000u32.to_be_bytes());
        ihdr.extend_from_slice(&9000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(&ihdr);
        png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        // One pixel row's worth of stored-deflate data, so the file is a
        // structurally complete PNG rather than a truncated one. The decoder
        // needs an IDAT to exist before it will answer a dimension question.
        let mut idat: Vec<u8> = b"IDAT".to_vec();
        idat.extend_from_slice(&[
            0x78, 0x01, 0x01, 0x01, 0x00, 0xfe, 0xff, 0x00, 0x00, 0x01, 0x00, 0x01,
        ]);
        png.extend_from_slice(&12u32.to_be_bytes());
        png.extend_from_slice(&idat);
        png.extend_from_slice(&crc32(&idat).to_be_bytes());

        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&crc32(b"IEND").to_be_bytes());
        std::fs::write(&path, &png).unwrap();
        assert!(
            png.len() < 128,
            "the fixture is small, the size it claims is not"
        );

        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        preset.image = Some(path);
        match render(&preset, &samples(None, None), &panel()) {
            Err(DisplayError::ImageTooLarge { width, height, max }) => {
                assert_eq!((width, height), (9000, 9000));
                assert_eq!(max, MAX_IMAGE_DIMENSION);
            }
            other => panic!("expected a size refusal, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The checksum a PNG chunk carries, so the fixture above is a real file.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn a_frame_encodes_to_a_png_the_toolkit_can_decode() {
        let preset = DisplayPreset::default_infographic();
        let frame = render(&preset, &samples(Some(61.0), Some(48.0)), &panel()).unwrap();
        let png = frame.to_png().unwrap();

        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (240, 240));

        // Every pixel of the preview is a pixel the panel will show, so the
        // two cannot disagree about a color the hardware cannot reproduce.
        let quantized = frame.quantized();
        for (index, pixel) in decoded.pixels().enumerate() {
            assert_eq!(
                Rgb::new(pixel[0], pixel[1], pixel[2]),
                quantized[index],
                "pixel {index} differs between the preview and the frame"
            );
        }
    }

    #[test]
    fn a_panel_with_no_geometry_produces_no_frame() {
        let mut unknown = panel();
        unknown.width = 0;
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
        // US-017 gives the preview 16.7 ms at P95, which is the budget for the
        // shipped build. An unoptimized build is an order of magnitude slower
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

    #[test]
    fn the_panel_never_carries_a_vendors_wordmark() {
        // FR-20. The only name the renderer can draw is the product's own, and
        // it comes from one constant both processes read.
        assert_eq!(PRODUCT_NAME, nzxt_core::PRODUCT_NAME);
        assert!(!PRODUCT_NAME.to_ascii_uppercase().contains("NZXT"));
        assert!(!PRODUCT_NAME.to_ascii_uppercase().contains("CAM"));
    }
}
