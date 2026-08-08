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
pub mod text;

use nzxt_core::capability::LcdPanel;
use nzxt_core::display::{
    DisplayError, DisplayMode, DisplayPreset, MAX_IMAGE_DIMENSION, MetricSample, Orientation,
    ReadingSlot,
};
use nzxt_core::lighting::Rgb;

use image::{ImageDecoder, ImageEncoder};

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
        DisplayMode::Solid => {}
        DisplayMode::Image => {
            draw_image(&mut canvas, preset)?;
        }
        DisplayMode::DualInfographic => draw_infographic(&mut canvas, preset, samples),
        DisplayMode::SingleReading => draw_single(&mut canvas, preset, &samples[0]),
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
    /// Outer and inner radius of the gauge bands.
    pub const TRACK_OUTER: f32 = 0.965;
    pub const TRACK_INNER: f32 = 0.845;
    /// Thinner band an unavailable reading falls back to.
    pub const TRACK_UNAVAILABLE_INNER: f32 = 0.905;

    /// Where a side band begins, and how far it runs.
    ///
    /// The two bands are mirrored about the vertical, centered on nine and
    /// three o'clock, and both climb toward the top. Short rather than
    /// sweeping: the pair of readings sits between them, so the band's job is
    /// to flank a column and not to enclose the panel.
    pub const SIDE_START_TURN: f32 = 0.610;
    pub const SIDE_SWEEP_TURNS: f32 = 0.280;

    /// The single reading's band: the whole dial, filling clockwise from the
    /// top, which is where a gauge with nothing beside it starts.
    pub const SINGLE_START_TURN: f32 = 0.0;
    pub const SINGLE_SWEEP_TURNS: f32 = 1.0;

    /// The two-reading layout: a column per reading, side by side, each with
    /// its caption under it and its band outside it.
    ///
    /// Side by side rather than stacked. Stacked, the two values sit one above
    /// the other on the same axis and read as one four-digit number; side by
    /// side, each is a column with its own caption and its own band beside it,
    /// and the pairing is legible without reading a word.
    pub const PAIR_OFFSET: f32 = 0.205;
    pub const VALUE_TOP: f32 = 0.383;
    pub const VALUE_HEIGHT: f32 = 0.128;
    pub const CAPTION_TOP: f32 = 0.550;

    /// The one-reading layout: the value and its metric, centered in the ring.
    pub const SINGLE_VALUE_TOP: f32 = 0.320;
    /// The largest the value is ever set, before the fit may shrink it.
    pub const SINGLE_VALUE_HEIGHT: f32 = 0.235;
    /// Widest a line may be before it would meet the ring.
    ///
    /// Held inside the ring's own opening rather than against it: the value is
    /// the tallest line on the glass, so its corners come closest to the band
    /// even where its middle would clear it.
    pub const SINGLE_VALUE_WIDTH: f32 = 0.750;
    pub const SINGLE_CAPTION_TOP: f32 = 0.615;
    pub const SINGLE_CAPTION_HEIGHT: f32 = 0.072;

    /// How the unit is set against the value it belongs to.
    ///
    /// Smaller and detached, aligned on the cap line rather than the baseline,
    /// which is where a degree sign belongs and where the reference screens put
    /// it. The value is centered on its own, so a reading is centered on its
    /// digits and the unit hangs off the right of them: a number that shifted
    /// sideways because its unit was wide would be a number that moves when the
    /// metric changes.
    pub const UNIT_SCALE: f32 = 0.450;
    pub const UNIT_GAP: f32 = 0.060;

    /// Cap height of a caption in the two-reading layout.
    pub const CAPTION_HEIGHT: f32 = 0.067;

    /// How much of the reading's own color the empty part of its track keeps.
    ///
    /// Enough to say where the band will grow, far enough below the reading
    /// itself that a full gauge and an empty one are never confused.
    pub const TRACK_REST: f32 = 0.130;
}

/// Draw both gauges, both readings and both captions.
fn draw_infographic(canvas: &mut Canvas, preset: &DisplayPreset, samples: &[MetricSample; 2]) {
    let width = canvas.width() as f32;
    let height = canvas.height() as f32;

    for (index, sample) in samples.iter().enumerate() {
        let slot = &preset.readings[index];
        // The first band flanks the left column, the second the right. Both
        // climb toward the top, so the pair is a mirror rather than a rotation
        // and neither reading is the one that reads upside down.
        let start = if index == 0 {
            layout::SIDE_START_TURN
        } else {
            1.0 - layout::SIDE_START_TURN - layout::SIDE_SWEEP_TURNS
        };
        draw_gauge(
            canvas,
            preset,
            slot,
            sample,
            Band {
                start,
                sweep: layout::SIDE_SWEEP_TURNS,
                mirrored: index == 1,
                shade: DisplayMode::DualInfographic.gradates_band(),
            },
        );

        let column = if index == 0 {
            width * (0.5 - layout::PAIR_OFFSET)
        } else {
            width * (0.5 + layout::PAIR_OFFSET)
        };
        draw_reading(
            canvas,
            sample,
            slot.text,
            column,
            height * layout::VALUE_TOP,
            height * layout::VALUE_HEIGHT,
        );
        // The caption takes the band's color rather than the value's: it is the
        // band's label, and the color is what ties a reading to the arc that
        // measures it when two of them share the glass.
        text::draw_centered(
            canvas,
            sample.metric.caption(),
            column,
            height * layout::CAPTION_TOP,
            height * layout::CAPTION_HEIGHT,
            slot.reading,
        );
    }
}

/// Draw one metric as the panel's whole subject.
///
/// Two centered lines inside a full ring: the value at the largest size the
/// ring's opening allows, and the metric under it.
fn draw_single(canvas: &mut Canvas, preset: &DisplayPreset, sample: &MetricSample) {
    let width = canvas.width() as f32;
    let height = canvas.height() as f32;
    let center_x = width / 2.0;
    let slot = &preset.readings[0];

    draw_gauge(
        canvas,
        preset,
        slot,
        sample,
        Band {
            start: layout::SINGLE_START_TURN,
            sweep: layout::SINGLE_SWEEP_TURNS,
            mirrored: false,
            shade: DisplayMode::SingleReading.gradates_band(),
        },
    );
    draw_reading(
        canvas,
        sample,
        slot.text,
        center_x,
        height * layout::SINGLE_VALUE_TOP,
        single_value_height(sample, width, height),
    );
    text::draw_centered(
        canvas,
        sample.metric.caption(),
        center_x,
        height * layout::SINGLE_CAPTION_TOP,
        height * layout::SINGLE_CAPTION_HEIGHT,
        slot.text,
    );
}

/// How tall the single layout sets its value.
///
/// Sized against the widest reading the *metric* can produce rather than the
/// one it currently holds. Fitting the current value would resize the whole
/// line every time a load crossed 99, which is a panel that never sits still;
/// fitting the metric picks a size once and keeps it, at the cost of a value in
/// percent being set slightly smaller than one in degrees, because the percent
/// sign is the wider mark.
fn single_value_height(sample: &MetricSample, width: f32, height: f32) -> f32 {
    // Half the room, because the reading is centered and it is the right half
    // that has to hold both the digits and the unit. Width is linear in the cap
    // height, so one measurement gives the ratio.
    let half_room = width * layout::SINGLE_VALUE_WIDTH / 2.0;
    let reach = reading_reach(sample).max(f32::EPSILON);
    (height * layout::SINGLE_VALUE_HEIGHT).min(half_room / reach)
}

/// Where one band sits on the dial and how it is painted.
#[derive(Debug, Clone, Copy)]
struct Band {
    /// Turn the sweep begins at, clockwise from twelve o'clock.
    start: f32,
    sweep: f32,
    /// Fill from the end of the sweep rather than its start, which is what
    /// makes the right-hand gauge of a pair climb toward twelve o'clock like
    /// its neighbor instead of descending from it.
    mirrored: bool,
    /// Shade between the slot's two colors rather than drawing it solid.
    shade: bool,
}

/// One band: the track it could reach, then the part it has.
fn draw_gauge(
    canvas: &mut Canvas,
    preset: &DisplayPreset,
    slot: &ReadingSlot,
    sample: &MetricSample,
    band: Band,
) {
    let width = canvas.width() as f32;
    let height = canvas.height() as f32;
    let center = (width / 2.0, height / 2.0);
    let radius = width.min(height) / 2.0;
    let head = if band.shade {
        slot.band_end()
    } else {
        slot.reading
    };

    let Some(fraction) = sample.fraction() else {
        // An unavailable reading gets a thinner, colorless band. The shape
        // carries the meaning as well as the color does, which is what keeps it
        // readable without relying on hue.
        canvas.fill_arc(
            Arc {
                center,
                inner: radius * layout::TRACK_UNAVAILABLE_INNER,
                outer: radius * layout::TRACK_OUTER,
                start_turns: band.start,
                sweep_turns: band.sweep,
                round_caps: true,
            },
            mix(preset.background, slot.text, 0.28),
        );
        return;
    };

    canvas.fill_arc(
        Arc {
            center,
            inner: radius * layout::TRACK_INNER,
            outer: radius * layout::TRACK_OUTER,
            start_turns: band.start,
            sweep_turns: band.sweep,
            round_caps: true,
        },
        mix(
            preset.background,
            mix(slot.reading, head, 0.5),
            layout::TRACK_REST,
        ),
    );

    // From the slot's first color to its second, and only ever between those
    // two: a band that shaded into a color nobody picked is a band whose
    // swatches do not describe it. A layout that does not shade, or a slot with
    // no second color, draws it solid.
    let filled = band.sweep * fraction;
    let (fill_start, from, to) = if band.mirrored {
        (band.start + band.sweep - filled, head, slot.reading)
    } else {
        (band.start, slot.reading, head)
    };
    canvas.fill_arc_gradient(
        Arc {
            center,
            inner: radius * layout::TRACK_INNER,
            outer: radius * layout::TRACK_OUTER,
            start_turns: fill_start,
            sweep_turns: filled,
            round_caps: true,
        },
        from,
        to,
    );
}

/// A reading centered on `center_x`, with its unit hung off the right.
///
/// The digits alone decide the centering. The unit is set smaller and aligned
/// on the cap line, so it reads as a mark on the number rather than as a
/// character of it, and a metric in percent does not push its value off center.
fn draw_reading(
    canvas: &mut Canvas,
    sample: &MetricSample,
    color: Rgb,
    center_x: f32,
    top: f32,
    cap_height: f32,
) {
    let value = sample.text();
    let value_width = text::width(&value, cap_height);
    let left = center_x - value_width / 2.0;
    text::draw(canvas, &value, left, top, cap_height, color);
    text::draw(
        canvas,
        sample.metric.unit(),
        left + value_width + cap_height * layout::UNIT_GAP,
        top,
        cap_height * layout::UNIT_SCALE,
        color,
    );
}

/// How far right of the value's center its unit reaches, per unit of cap
/// height.
///
/// What bounds the size of a reading: the digits are centered, so the value
/// claims half its own width on each side, and the unit claims the rest on the
/// right alone.
fn reading_reach(sample: &MetricSample) -> f32 {
    let value = MetricSample {
        metric: sample.metric,
        // The widest a reading gets, since every metric is scaled to 100.
        value: Some(100.0),
    };
    text::width(&value.text(), 1.0) / 2.0
        + layout::UNIT_GAP
        + text::width(sample.metric.unit(), layout::UNIT_SCALE)
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
///
/// The file is opened once. Reading the header to get the size and then
/// reopening to decode meant a whole second decode per render, and this runs on
/// every panel refresh and on every preview repaint. The decoder answers the
/// declared size from the header it has already read, so the refusal still
/// happens before a single pixel is decoded.
fn draw_image(canvas: &mut Canvas, preset: &DisplayPreset) -> Result<(), DisplayError> {
    let Some(path) = preset.image.as_ref() else {
        return Err(DisplayError::ImagePathMissing);
    };
    let name = path.display().to_string();
    let undecodable = |detail: String| DisplayError::ImageUndecodable {
        path: name.clone(),
        detail,
    };

    let decoder = image::ImageReader::open(path)
        .map_err(|error| undecodable(error.to_string()))?
        .with_guessed_format()
        .map_err(|error| undecodable(error.to_string()))?
        .into_decoder()
        .map_err(|error| undecodable(error.to_string()))?;
    let (source_width, source_height) = decoder.dimensions();
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

    let decoded = image::DynamicImage::from_decoder(decoder)
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
        let preset = DisplayPreset::solid(Rgb::new(0xff, 0x00, 0x00));
        let frame = render(&preset, &samples(None, None), &panel()).unwrap();
        let bytes = frame.to_rgb565_be();
        // Pure red is five ones in the top bits: 0xF800.
        assert_eq!(&bytes[0..2], &[0xf8, 0x00]);

        let blue = DisplayPreset::solid(Rgb::new(0x00, 0x00, 0xff));
        let bytes = render(&blue, &samples(None, None), &panel())
            .unwrap()
            .to_rgb565_be();
        assert_eq!(&bytes[0..2], &[0x00, 0x1f]);

        let green = DisplayPreset::solid(Rgb::new(0x00, 0xff, 0x00));
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
    fn a_solid_field_is_one_color_and_nothing_else() {
        // What makes it usable as a transport test frame: every pixel is the
        // color that was asked for, so an operator watching the panel is
        // reading the transport rather than the renderer. Telemetry is ignored
        // entirely, whatever the samples say.
        let background = Rgb::new(0x11, 0x22, 0x33);
        let preset = DisplayPreset::solid(background);
        let frame = render(&preset, &samples(Some(99.0), Some(99.0)), &panel()).unwrap();
        assert_eq!(
            drawn(&frame, background),
            0,
            "a solid field drew something over its own color"
        );
    }

    /// How much of the ring one slot has actually filled.
    ///
    /// Counted as pixels brighter than the slot's own resting track rather than
    /// as pixels of one exact color: the fill shades along its sweep, so only
    /// its far end is ever the chosen color exactly.
    fn filled_ring(frame: &Framebuffer, preset: &DisplayPreset, slot: usize) -> usize {
        let rest = mix(
            preset.background,
            preset.readings[slot].reading,
            layout::TRACK_REST,
        );
        let level = |color: Rgb| u32::from(color.r) + u32::from(color.g) + u32::from(color.b);
        in_ring(frame)
            .filter(|pixel| level(*pixel) > level(rest) + 8)
            .count()
    }

    /// Every pixel of the frame that lies on the gauge band.
    fn in_ring(frame: &Framebuffer) -> impl Iterator<Item = Rgb> + '_ {
        frame
            .pixels()
            .iter()
            .enumerate()
            .filter_map(|(index, pixel)| {
                let (x, y) = ((index % 240) as f32, (index / 240) as f32);
                let distance = ((x - 120.0).powi(2) + (y - 120.0).powi(2)).sqrt();
                (distance > 120.0 * layout::TRACK_INNER).then_some(*pixel)
            })
    }

    #[test]
    fn a_higher_reading_fills_more_of_its_gauge() {
        for mode in [DisplayMode::DualInfographic, DisplayMode::SingleReading] {
            let mut preset = DisplayPreset::default_infographic();
            preset.mode = mode;
            let mut filled = Vec::new();
            for value in [0.0, 25.0, 50.0, 100.0] {
                let frame = render(&preset, &samples(Some(value), Some(0.0)), &panel()).unwrap();
                filled.push(filled_ring(&frame, &preset, 0));
            }
            assert!(
                filled.windows(2).all(|pair| pair[1] > pair[0]),
                "{mode:?} gauge fill did not grow with the reading: {filled:?}"
            );
        }
    }

    #[test]
    fn a_band_only_ever_shows_the_colors_its_slot_names() {
        // The swatches in the editor have to describe the band. A gauge that
        // shaded through a color nobody picked would make the two disagree, and
        // the operator would have no way to name what they are seeing.
        //
        // Measured on the single layout, where one band owns the whole ring:
        // every pixel there has to lie inside the envelope of the background
        // and the slot's two colors, because the resting track is a mix of
        // them and both the shade and the antialiasing only interpolate.
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::SingleReading;
        let slot = preset.readings[0];
        for reading in [40.0, 100.0] {
            let frame = render(&preset, &samples(Some(reading), None), &panel()).unwrap();
            for named in [slot.reading, slot.band_end()] {
                assert!(
                    in_ring(&frame).any(|pixel| pixel == named),
                    "the band at {reading} never shows {named:?}"
                );
            }
            let inside = |channel: fn(Rgb) -> u8, pixel: Rgb| {
                let ends = [
                    channel(preset.background),
                    channel(slot.reading),
                    channel(slot.band_end()),
                ];
                let low = ends.iter().copied().min().unwrap_or(0);
                let high = ends.iter().copied().max().unwrap_or(255);
                channel(pixel) >= low.saturating_sub(2) && channel(pixel) <= high.saturating_add(2)
            };
            let unexpected = in_ring(&frame)
                .filter(|pixel| {
                    !(inside(|color| color.r, *pixel)
                        && inside(|color| color.g, *pixel)
                        && inside(|color| color.b, *pixel))
                })
                .count();
            assert_eq!(
                unexpected, 0,
                "the band at {reading} drew {unexpected} pixels outside its own colors"
            );
        }

        // The paired layout shades neither of its two bands, so each is drawn
        // in its first color alone whatever second color the preset carries.
        // The fades are set to colors nothing else on the panel uses, so their
        // absence cannot be confused with the other slot's band.
        let mut paired = DisplayPreset::default_infographic();
        paired.readings[0].reading_end = Some(Rgb::new(0x00, 0xff, 0x00));
        paired.readings[1].reading_end = Some(Rgb::new(0xff, 0xff, 0x00));
        let frame = render(&paired, &samples(Some(60.0), Some(60.0)), &panel()).unwrap();
        for index in 0..2 {
            assert!(
                in_ring(&frame).any(|pixel| pixel == paired.readings[index].reading),
                "gauge {index} never shows the color it was given"
            );
            assert!(
                !in_ring(&frame).any(|pixel| pixel == paired.readings[index].band_end()),
                "gauge {index} shaded in a layout that draws its bands solid"
            );
        }
    }

    #[test]
    fn a_reading_is_centered_on_its_digits_and_not_on_its_unit() {
        // The unit hangs off the right of the value at a smaller size. If the
        // pair were centered as a group, the same number would sit in a
        // different place under a degree sign and under a percent sign, and the
        // panel would appear to shift when the metric changed.
        let columns = |metric: LcdMetric| -> (u32, u32) {
            let mut preset = DisplayPreset::default_infographic();
            preset.readings[0].metric = metric;
            let frame = render(
                &preset,
                &[
                    MetricSample {
                        metric,
                        value: Some(50.0),
                    },
                    MetricSample::unavailable(LcdMetric::GpuTemperature),
                ],
                &panel(),
            )
            .unwrap();
            // Only the rows and the column the first reading occupies, so the
            // caption under it and the reading beside it are not measured with
            // it.
            let rows = ((layout::VALUE_TOP * 240.0) as usize + 2)
                ..((layout::VALUE_TOP + layout::VALUE_HEIGHT) * 240.0) as usize;
            let lit: Vec<u32> = frame
                .pixels()
                .iter()
                .enumerate()
                .filter(|(index, pixel)| {
                    rows.contains(&(index / 240))
                        && index % 240 < 120
                        && **pixel == preset.readings[0].text
                })
                .map(|(index, _)| index as u32 % 240)
                .collect();
            assert!(!lit.is_empty(), "{metric:?} drew no value");
            (
                lit.iter().copied().min().unwrap_or(0),
                lit.iter().copied().max().unwrap_or(0),
            )
        };

        let (degrees_left, degrees_right) = columns(LcdMetric::CpuTemperature);
        let (percent_left, percent_right) = columns(LcdMetric::CpuLoad);
        let column = 240.0 * (0.5 - layout::PAIR_OFFSET);
        assert!(
            degrees_left.abs_diff(percent_left) <= 1,
            "the digits start at {degrees_left} under a degree sign and \
             {percent_left} under a percent sign"
        );
        assert!(
            percent_right > degrees_right,
            "the percent sign is the wider mark, so it must reach further right"
        );

        // And the digits are centered on their column, the unit excluded: the
        // ink starts as far left of it as the digits alone are wide.
        assert!(
            (degrees_left as f32) < column && column < degrees_right as f32,
            "the reading does not straddle the column it belongs to"
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
        let count = |frame: &Framebuffer, color: Rgb| {
            in_ring(frame).filter(|pixel| *pixel == color).count()
        };
        assert_eq!(
            count(&missing, preset.readings[0].reading),
            0,
            "the unavailable gauge kept the reading color"
        );
        // The paired layout draws its bands solid, so the resting track under
        // one is its own color dimmed and nothing else.
        assert!(
            count(
                &zero,
                mix(
                    preset.background,
                    preset.readings[0].reading,
                    layout::TRACK_REST
                )
            ) > 0,
            "a reading of zero still shows its own track, in its own color"
        );

        // The dashes are the second signal, so the state does not depend on
        // hue alone. They are set in the slot's text color, which is what the
        // value itself uses.
        assert!(
            missing.pixels().contains(&preset.readings[0].text),
            "the unavailable marker is still drawn in the text color"
        );
    }

    #[test]
    fn both_readings_are_drawn_and_each_uses_its_own_colors() {
        let mut preset = DisplayPreset::default_infographic();
        preset.readings[0] = ReadingSlot {
            metric: LcdMetric::CpuTemperature,
            reading: Rgb::new(0xff, 0x00, 0x00),
            reading_end: None,
            text: Rgb::new(0x00, 0xff, 0x00),
        };
        preset.readings[1] = ReadingSlot {
            metric: LcdMetric::GpuTemperature,
            reading: Rgb::new(0x00, 0x00, 0xff),
            reading_end: None,
            text: Rgb::new(0xff, 0xff, 0x00),
        };
        let frame = render(&preset, &samples(Some(80.0), Some(80.0)), &panel()).unwrap();

        for color in [
            preset.readings[0].reading,
            preset.readings[0].text,
            preset.readings[1].reading,
            preset.readings[1].text,
        ] {
            assert!(
                frame.pixels().contains(&color),
                "{color:?} was selected but never drawn"
            );
        }
    }

    #[test]
    fn the_two_gauges_occupy_opposite_sides_of_the_dial() {
        let mut preset = DisplayPreset::default_infographic();
        preset.readings[0].reading = Rgb::new(0xff, 0x00, 0x00);
        preset.readings[1].reading = Rgb::new(0x00, 0x00, 0xff);
        let frame = render(&preset, &samples(Some(100.0), Some(100.0)), &panel()).unwrap();

        // Each band is a mirror of the other about the vertical, so the side a
        // band sits on is what identifies it, not the half of the panel. Only
        // the ring is measured: the readings are drawn in the same colors, in
        // the middle, and would pull both averages to the center.
        let column_of = |dominant: fn(Rgb) -> bool| -> f32 {
            let columns: Vec<u32> = frame
                .pixels()
                .iter()
                .enumerate()
                .filter(|(index, pixel)| {
                    let (x, y) = ((index % 240) as f32, (index / 240) as f32);
                    let distance = ((x - 120.0).powi(2) + (y - 120.0).powi(2)).sqrt();
                    distance > 120.0 * layout::TRACK_INNER && dominant(**pixel)
                })
                .map(|(index, _)| index as u32 % 240)
                .collect();
            assert!(!columns.is_empty(), "one of the bands was never drawn");
            columns.iter().sum::<u32>() as f32 / columns.len() as f32
        };
        assert!(
            column_of(|pixel| u16::from(pixel.r) > u16::from(pixel.b) + 20) < 120.0,
            "the first gauge climbs the left of the dial"
        );
        assert!(
            column_of(|pixel| u16::from(pixel.b) > u16::from(pixel.r) + 20) > 120.0,
            "the second gauge climbs the right of the dial"
        );
    }

    #[test]
    fn the_single_layout_shows_one_reading_and_gives_it_the_whole_dial() {
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::SingleReading;
        preset.readings[1].reading = Rgb::new(0x00, 0x00, 0xff);
        preset.readings[1].text = Rgb::new(0xff, 0xff, 0x00);
        let frame = render(&preset, &samples(Some(50.0), Some(50.0)), &panel()).unwrap();

        // The second slot is not drawn at all: one reading means one reading,
        // not a second one in the same place.
        for absent in [preset.readings[1].reading, preset.readings[1].text] {
            assert!(
                !frame.pixels().contains(&absent),
                "{absent:?} belongs to the second slot, which this mode omits"
            );
        }

        // And the one it does draw is larger than the same value would be with
        // two on the glass, which is the point of the mode.
        let ink = |frame: &Framebuffer| {
            frame
                .pixels()
                .iter()
                .filter(|pixel| **pixel == preset.readings[0].text)
                .count()
        };
        let mut dual = preset.clone();
        dual.mode = DisplayMode::DualInfographic;
        let paired = render(&dual, &samples(Some(50.0), Some(50.0)), &panel()).unwrap();
        assert!(
            ink(&frame) > ink(&paired),
            "the single reading is not drawn larger than the paired one"
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
    fn the_panel_carries_no_wordmark_at_all() {
        // FR-20 and US-017 AC-6 ask for the project's own wordmark or no logo.
        // It is now no logo: the panel shows the reading it exists to show and
        // nothing that names anybody, which is the one arrangement that cannot
        // be mistaken for a vendor's.
        //
        // Measured where the name used to sit: the band of the glass above the
        // value, between the two sides of the ring. Every layout drew a name
        // there or on its own axis, and nothing may now.
        let mut preset = DisplayPreset::default_infographic();
        preset.background = Rgb::BLACK;

        let empty_between = |frame: &Framebuffer, rows: std::ops::Range<u32>| {
            let inked = frame
                .pixels()
                .iter()
                .enumerate()
                .filter(|(index, pixel)| {
                    let (x, y) = ((index % 240) as f32, (index / 240) as f32);
                    let distance = ((x - 120.0).powi(2) + (y - 120.0).powi(2)).sqrt();
                    // Inside the ring, so the band itself is not counted.
                    rows.contains(&(y as u32))
                        && distance < 120.0 * layout::TRACK_INNER - 2.0
                        && **pixel != Rgb::BLACK
                })
                .count();
            assert_eq!(inked, 0, "{inked} pixels drawn where the name used to be");
        };

        preset.mode = DisplayMode::SingleReading;
        let single = render(&preset, &samples(Some(50.0), None), &panel()).unwrap();
        // Above the value, where the single layout put the name.
        empty_between(&single, 24..(240.0 * layout::SINGLE_VALUE_TOP) as u32 - 2);

        preset.mode = DisplayMode::DualInfographic;
        let paired = render(&preset, &samples(Some(50.0), Some(50.0)), &panel()).unwrap();
        // Above the pair of readings, where the paired layout put it.
        empty_between(&paired, 24..(240.0 * layout::VALUE_TOP) as u32 - 2);

        preset.mode = DisplayMode::Solid;
        let solid = render(&preset, &samples(None, None), &panel()).unwrap();
        assert_eq!(drawn(&solid, Rgb::BLACK), 0, "a solid field names nobody");
    }
}
