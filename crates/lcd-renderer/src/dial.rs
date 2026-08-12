// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The two layouts that draw readings: a pair of columns, or one metric given
//! the whole ring.
//!
//! Both are built from the same three elements, a band, a value and a caption,
//! so a change to how a reading is set reaches both layouts at once. Nothing
//! here opens a file or knows the panel's byte format; it takes a canvas and
//! puts marks on it.

use kori_core::display::{DisplayPreset, LcdMetric, MetricSample, ReadingSlot};
use kori_core::lighting::Rgb;

use crate::canvas::{Arc, Canvas, Shade};
use crate::text;

/// Where each element sits, as a fraction of the panel's smaller side.
///
/// Fractions rather than pixels so the layout survives a panel of another size
/// without a second set of constants to keep in step. Every one of them is
/// resolved through [`Frame`], which is what makes "the smaller side" true
/// rather than merely written: the ring is bounded by that side, so anything
/// measured against the longer one drifts out of it the moment a panel is not
/// square.
pub(crate) mod layout {
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

/// The square the layout is drawn in: the largest one the panel contains,
/// centered on it.
///
/// Both layouts are a block of lines inside a ring, and the ring is bounded by
/// the panel's smaller side. Laying the lines out in the square of that side is
/// what keeps the two together: scaling the type with the height while the ring
/// follows the width puts a reading through its own gauge the first time a
/// panel is taller than it is wide.
///
/// On a square panel the origin is zero and every expression below reduces to
/// the side times a fraction, which is the arithmetic that was already being
/// done. That is deliberate rather than incidental: the panel this product
/// drives is square, and a rework of the scaling that moved a pixel on it would
/// be a rework nobody could check.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// The top left of that square, in panel coordinates.
    origin: (f32, f32),
    /// The panel's smaller side, which every fraction in [`layout`] scales by.
    side: f32,
}

impl Frame {
    fn of(width: f32, height: f32) -> Self {
        let side = width.min(height);
        Self {
            origin: ((width - side) / 2.0, (height - side) / 2.0),
            side,
        }
    }

    /// A length stated as a fraction of the smaller side.
    fn length(self, fraction: f32) -> f32 {
        self.side * fraction
    }

    /// The top of a line, stated as a fraction down the square.
    fn top(self, fraction: f32) -> f32 {
        self.origin.1 + self.length(fraction)
    }

    /// A column `fraction` of the side to the right of the square's middle.
    fn column(self, fraction: f32) -> f32 {
        self.origin.0 + self.length(0.5 + fraction)
    }

    /// The center of the dial, which is the center of the panel itself.
    fn center(self) -> (f32, f32) {
        (self.column(0.0), self.top(0.5))
    }

    fn radius(self) -> f32 {
        self.side / 2.0
    }
}

/// Draw both gauges, both readings and both captions.
pub(crate) fn infographic(
    canvas: &mut Canvas,
    preset: &DisplayPreset,
    samples: &[MetricSample; 2],
) {
    let frame = Frame::of(canvas.width() as f32, canvas.height() as f32);

    for (index, sample) in samples.iter().enumerate() {
        let mirrored = index == 1;
        let slot = &preset.readings[index];
        gauge(canvas, preset, slot, sample, Band::side(mirrored));

        let column = frame.column(if mirrored {
            layout::PAIR_OFFSET
        } else {
            -layout::PAIR_OFFSET
        });
        reading(
            canvas,
            sample,
            slot.text,
            column,
            frame.top(layout::VALUE_TOP),
            frame.length(layout::VALUE_HEIGHT),
        );
        // The caption takes the band's color rather than the value's: it is the
        // band's label, and the color is what ties a reading to the arc that
        // measures it when two of them share the glass.
        text::draw_centered(
            canvas,
            sample.metric.caption(),
            column,
            frame.top(layout::CAPTION_TOP),
            frame.length(layout::CAPTION_HEIGHT),
            slot.reading,
        );
    }
}

/// Draw one metric as the panel's whole subject.
///
/// Two centered lines inside a full ring: the value at the largest size the
/// ring's opening allows, and the metric under it.
pub(crate) fn single(canvas: &mut Canvas, preset: &DisplayPreset, sample: &MetricSample) {
    let frame = Frame::of(canvas.width() as f32, canvas.height() as f32);
    let center_x = frame.column(0.0);
    let slot = &preset.readings[0];

    gauge(canvas, preset, slot, sample, Band::WHOLE);
    reading(
        canvas,
        sample,
        slot.text,
        center_x,
        frame.top(layout::SINGLE_VALUE_TOP),
        single_value_height(sample.metric, frame),
    );
    text::draw_centered(
        canvas,
        sample.metric.caption(),
        center_x,
        frame.top(layout::SINGLE_CAPTION_TOP),
        frame.length(layout::SINGLE_CAPTION_HEIGHT),
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
fn single_value_height(metric: LcdMetric, frame: Frame) -> f32 {
    // Half the room, because the reading is centered and it is the right half
    // that has to hold both the digits and the unit. Width is linear in the cap
    // height, so one measurement gives the ratio.
    let half_room = frame.length(layout::SINGLE_VALUE_WIDTH) / 2.0;
    let reach = reading_reach(metric).max(f32::EPSILON);
    frame
        .length(layout::SINGLE_VALUE_HEIGHT)
        .min(half_room / reach)
}

/// How far right of the value's center its unit reaches, per unit of cap
/// height.
///
/// What bounds the size of a reading: the digits are centered, so the value
/// claims half its own width on each side, and the unit claims the rest on the
/// right alone.
fn reading_reach(metric: LcdMetric) -> f32 {
    // The widest a reading gets, since every metric is scaled to its own full
    // scale and the digits are tabular.
    let widest = MetricSample {
        metric,
        value: Some(metric.full_scale()),
    };
    text::width(&widest.text(), 1.0) / 2.0
        + layout::UNIT_GAP
        + text::width(metric.unit(), layout::UNIT_SCALE)
}

/// Where one band sits on the dial: geometry only, no color.
#[derive(Debug, Clone, Copy)]
struct Band {
    /// Turn the sweep begins at, clockwise from twelve o'clock.
    start: f32,
    sweep: f32,
    /// Fill from the end of the sweep rather than its start, which is what
    /// makes the right-hand gauge of a pair climb toward twelve o'clock like
    /// its neighbor instead of descending from it.
    mirrored: bool,
}

impl Band {
    /// The whole dial, for the layout with nothing beside it.
    const WHOLE: Self = Self {
        start: layout::SINGLE_START_TURN,
        sweep: layout::SINGLE_SWEEP_TURNS,
        mirrored: false,
    };

    /// One of the two bands that flank a column, mirrored about the vertical.
    ///
    /// The mirror is stated once, here: the second band begins as far before
    /// twelve o'clock as the first begins after it, and fills from the other
    /// end. Stating it twice is how the pair ends up as one gauge rotated, with
    /// one of the two readings hanging upside down.
    fn side(mirrored: bool) -> Self {
        let start = if mirrored {
            1.0 - layout::SIDE_START_TURN - layout::SIDE_SWEEP_TURNS
        } else {
            layout::SIDE_START_TURN
        };
        Self {
            start,
            sweep: layout::SIDE_SWEEP_TURNS,
            mirrored,
        }
    }
}

/// One band: the track it could reach, then the part it has.
fn gauge(
    canvas: &mut Canvas,
    preset: &DisplayPreset,
    slot: &ReadingSlot,
    sample: &MetricSample,
    band: Band,
) {
    let frame = Frame::of(canvas.width() as f32, canvas.height() as f32);
    let center = frame.center();
    let radius = frame.radius();
    // Whether a band is long enough for a shade to read as a shade is a
    // property of the layout, so the mode is asked here rather than carried in
    // by each caller. A slot with no second color draws solid either way.
    let head = if preset.mode.gradates_band() {
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
            preset.background.mixed(slot.text, 0.28),
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
        preset
            .background
            .mixed(slot.reading.mixed(head, 0.5), layout::TRACK_REST),
    );

    // From the slot's first color to its second, and only ever between those
    // two: a band that shaded into a color nobody picked is a band whose
    // swatches do not describe it. A layout that does not shade, or a slot with
    // no second color, draws it solid.
    //
    // The mirror is handed over rather than applied here. A mirrored band fills
    // from the end of its sweep back toward the start, so its foot is the far
    // end; swapping the two colors would put the right shade on the band and
    // still leave the wrong one on the cap that survives at a low reading.
    let filled = band.sweep * fraction;
    let fill_start = if band.mirrored {
        band.start + band.sweep - filled
    } else {
        band.start
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
        Shade {
            foot: slot.reading,
            head,
            foot_at_end: band.mirrored,
        },
    );
}

/// A reading centered on `center_x`, with its unit hung off the right.
///
/// The digits alone decide the centering. The unit is set smaller and aligned
/// on the cap line, so it reads as a mark on the number rather than as a
/// character of it, and a metric in percent does not push its value off center.
fn reading(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{RADIUS, SIDE, in_ring, panel, samples, scan};
    use crate::{Framebuffer, render};
    use kori_core::display::{DisplayMode, DisplayPreset};

    /// How much of the ring one slot has actually filled.
    ///
    /// Counted as pixels brighter than the slot's own resting track rather than
    /// as pixels of one exact color: the fill shades along its sweep, so only
    /// its far end is ever the chosen color exactly.
    fn filled_ring(frame: &Framebuffer, preset: &DisplayPreset, slot: usize) -> usize {
        let rest = preset
            .background
            .mixed(preset.readings[slot].reading, layout::TRACK_REST);
        let level = |color: Rgb| u32::from(color.r) + u32::from(color.g) + u32::from(color.b);
        in_ring(frame)
            .filter(|(_, pixel)| level(*pixel) > level(rest) + 8)
            .count()
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
        for value in [40.0, 100.0] {
            let frame = render(&preset, &samples(Some(value), None), &panel()).unwrap();
            for named in [slot.reading, slot.band_end()] {
                assert!(
                    in_ring(&frame).any(|(_, pixel)| pixel == named),
                    "the band at {value} never shows {named:?}"
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
                .filter(|(_, pixel)| {
                    !(inside(|color| color.r, *pixel)
                        && inside(|color| color.g, *pixel)
                        && inside(|color| color.b, *pixel))
                })
                .count();
            assert_eq!(
                unexpected, 0,
                "the band at {value} drew {unexpected} pixels outside its own colors"
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
                in_ring(&frame).any(|(_, pixel)| pixel == paired.readings[index].reading),
                "gauge {index} never shows the color it was given"
            );
            assert!(
                !in_ring(&frame).any(|(_, pixel)| pixel == paired.readings[index].band_end()),
                "gauge {index} shaded in a layout that draws its bands solid"
            );
        }
    }

    #[test]
    fn a_gauge_at_the_foot_of_its_scale_shows_the_color_its_foot_names() {
        // The band's two colors are what the editor's swatches promise. A
        // reading of zero showed the second one, because both round caps land
        // on the same point when the sweep is empty and the head was painted
        // last. An idle load then wore the color that means full scale.
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::SingleReading;
        preset.readings[0].metric = LcdMetric::CpuLoad;
        preset.readings[0].reading = Rgb::new(0x00, 0x00, 0xff);
        preset.readings[0].reading_end = Some(Rgb::new(0xff, 0x00, 0x00));

        // The middle of the band at twelve o'clock, where a full ring starts
        // and where the dot of an empty gauge sits.
        let row = (RADIUS - RADIUS * (layout::TRACK_INNER + layout::TRACK_OUTER) / 2.0) as u32;
        let dot = |value: f32| {
            let frame = render(
                &preset,
                &[
                    MetricSample {
                        metric: LcdMetric::CpuLoad,
                        value: Some(value),
                    },
                    MetricSample::unavailable(LcdMetric::GpuTemperature),
                ],
                &panel(),
            )
            .unwrap();
            crate::fixtures::at(&frame, SIDE / 2, row)
        };

        // Zero, and every reading too small to hold the two caps apart.
        for value in [0.0, 0.5, 1.0, 2.0] {
            let pixel = dot(value);
            assert!(
                pixel.b > pixel.r,
                "a reading of {value} wore its head color at the foot: {pixel:?}"
            );
        }
        // And the head is still reached where the band actually ends.
        let full = render(&preset, &samples(Some(100.0), None), &panel()).unwrap();
        assert!(
            in_ring(&full).any(|(_, pixel)| pixel == preset.readings[0].band_end()),
            "a full gauge never reaches the color its head names"
        );
    }

    #[test]
    fn an_oblong_panel_keeps_its_readings_inside_its_own_ring() {
        // Every fraction is taken against the smaller side, which is the side
        // the ring is bounded by. Scaling the type with the height while the
        // ring followed the width put a reading through its own gauge as soon
        // as a panel was taller than it was wide.
        for (width, height) in [(240, 240), (320, 240), (240, 320)] {
            let mut oblong = panel();
            oblong.width = width;
            oblong.height = height;
            oblong.frame_bytes = u32::from(width) * u32::from(height) * 2;

            for mode in [DisplayMode::DualInfographic, DisplayMode::SingleReading] {
                let mut preset = DisplayPreset::default_infographic();
                preset.mode = mode;
                preset.background = Rgb::BLACK;
                preset.readings[0].text = Rgb::new(0x00, 0xff, 0x00);
                preset.readings[1].text = Rgb::new(0x00, 0xff, 0x00);
                let frame = render(&preset, &samples(Some(100.0), Some(100.0)), &oblong).unwrap();

                let ink: Vec<f32> = scan(&frame)
                    .filter(|(_, _, _, pixel)| *pixel == preset.readings[0].text)
                    .map(|(_, _, distance, _)| distance)
                    .collect();
                assert!(!ink.is_empty(), "{width}x{height} {mode:?} drew no text");
                let inner = f32::from(width.min(height)) / 2.0 * layout::TRACK_INNER;
                let furthest = ink.iter().copied().fold(0.0_f32, f32::max);
                assert!(
                    furthest < inner,
                    "{width}x{height} {mode:?} put text {furthest} from the center, \
                     into a ring that starts at {inner}"
                );
            }
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
            let rows = ((layout::VALUE_TOP * SIDE as f32) as u32 + 2)
                ..((layout::VALUE_TOP + layout::VALUE_HEIGHT) * SIDE as f32) as u32;
            let lit: Vec<u32> = scan(&frame)
                .filter(|(x, y, _, pixel)| {
                    rows.contains(y) && *x < SIDE / 2 && *pixel == preset.readings[0].text
                })
                .map(|(x, _, _, _)| x)
                .collect();
            assert!(!lit.is_empty(), "{metric:?} drew no value");
            (
                lit.iter().copied().min().unwrap_or(0),
                lit.iter().copied().max().unwrap_or(0),
            )
        };

        let (degrees_left, degrees_right) = columns(LcdMetric::CpuTemperature);
        let (percent_left, percent_right) = columns(LcdMetric::CpuLoad);
        let column = SIDE as f32 * (0.5 - layout::PAIR_OFFSET);
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
            in_ring(frame).filter(|(_, pixel)| *pixel == color).count()
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
                preset
                    .background
                    .mixed(preset.readings[0].reading, layout::TRACK_REST)
            ) > 0,
            "a reading of zero still shows its own track, in its own color"
        );

        // The dashes are the second signal, so the state does not depend on
        // hue alone. They are set in the slot's text color, which is what the
        // value itself uses.
        assert!(
            scan(&missing).any(|(_, _, _, pixel)| pixel == preset.readings[0].text),
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
                scan(&frame).any(|(_, _, _, pixel)| pixel == color),
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
            let columns: Vec<u32> = in_ring(&frame)
                .filter(|(_, pixel)| dominant(*pixel))
                .map(|(x, _)| x)
                .collect();
            assert!(!columns.is_empty(), "one of the bands was never drawn");
            columns.iter().sum::<u32>() as f32 / columns.len() as f32
        };
        assert!(
            column_of(|pixel| u16::from(pixel.r) > u16::from(pixel.b) + 20) < RADIUS,
            "the first gauge climbs the left of the dial"
        );
        assert!(
            column_of(|pixel| u16::from(pixel.b) > u16::from(pixel.r) + 20) > RADIUS,
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
                !scan(&frame).any(|(_, _, _, pixel)| pixel == absent),
                "{absent:?} belongs to the second slot, which this mode omits"
            );
        }

        // And the one it does draw is larger than the same value would be with
        // two on the glass, which is the point of the mode.
        let ink = |frame: &Framebuffer| {
            scan(frame)
                .filter(|(_, _, _, pixel)| *pixel == preset.readings[0].text)
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
    fn a_value_is_sized_by_its_metric_rather_than_by_the_reading_it_holds() {
        // A load crossing 99 must not resize the line: the fit is computed
        // against the widest reading the metric can produce, so every value of
        // one metric is set at one size.
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::SingleReading;
        preset.readings[0].metric = LcdMetric::CpuLoad;

        let height_of = |value: f32| {
            let frame = render(
                &preset,
                &[
                    MetricSample {
                        metric: LcdMetric::CpuLoad,
                        value: Some(value),
                    },
                    MetricSample::unavailable(LcdMetric::GpuTemperature),
                ],
                &panel(),
            )
            .unwrap();
            let rows: Vec<u32> = scan(&frame)
                .filter(|(_, _, distance, pixel)| {
                    *distance < RADIUS * layout::TRACK_INNER && *pixel == preset.readings[0].text
                })
                .map(|(_, y, _, _)| y)
                .collect();
            assert!(!rows.is_empty(), "{value} drew no value");
            let top = rows.iter().copied().min().unwrap_or(0);
            let bottom = rows.iter().copied().max().unwrap_or(0);
            (top, bottom)
        };

        // The digits have no descender, so one value set at the same size as
        // another occupies the same two lines, whatever its width.
        assert_eq!(
            height_of(9.0),
            height_of(100.0),
            "a one-digit and a three-digit reading are not set at one size"
        );
        // And a percent sign is wider than a degree sign, so a load is set
        // slightly smaller than a temperature at the same geometry.
        let square = Frame::of(SIDE as f32, SIDE as f32);
        assert!(
            single_value_height(LcdMetric::CpuLoad, square)
                < single_value_height(LcdMetric::CpuTemperature, square),
            "the wider unit must be the one that shrinks its value"
        );
    }

    #[test]
    fn the_panel_carries_no_wordmark_at_all() {
        // The panel carries the project's own wordmark or no logo at all.
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
            let inked = scan(frame)
                .filter(|(_, y, distance, pixel)| {
                    // Inside the ring, so the band itself is not counted.
                    rows.contains(y)
                        && *distance < RADIUS * layout::TRACK_INNER - 2.0
                        && *pixel != Rgb::BLACK
                })
                .count();
            assert_eq!(inked, 0, "{inked} pixels drawn where the name used to be");
        };

        preset.mode = DisplayMode::SingleReading;
        let single = render(&preset, &samples(Some(50.0), None), &panel()).unwrap();
        // Above the value, where the single layout put the name.
        empty_between(
            &single,
            24..(SIDE as f32 * layout::SINGLE_VALUE_TOP) as u32 - 2,
        );

        preset.mode = DisplayMode::DualInfographic;
        let paired = render(&preset, &samples(Some(50.0), Some(50.0)), &panel()).unwrap();
        // Above the pair of readings, where the paired layout put it.
        empty_between(&paired, 24..(SIDE as f32 * layout::VALUE_TOP) as u32 - 2);
    }
}
