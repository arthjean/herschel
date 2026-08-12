// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The rolling series, drawn as a line over a dithered fill.
//!
//! A gap is drawn as a gap throughout: the line breaks, the fill opens, and a
//! tick marks the baseline under it. Joining across a hole would invent a value
//! and flattening it to zero would invent a plunge, and this product refuses
//! both. Everything the fill needs is here, because the dither is what the
//! chart is made of rather than a primitive anything else draws with.

use gpui::{
    Bounds, Div, Hsla, PathBuilder, Pixels, Point, Window, canvas, div, fill, point, prelude::*,
    px, size,
};

use kori_core::telemetry::History;

use crate::theme::{RADIUS, color};

use super::stroke_line;

/// A rolling series drawn as a line, with holes where samples are missing.
///
/// A gap is drawn as a gap. Joining across it would invent a value, and
/// flattening it to zero would invent a plunge.
pub struct Sparkline {
    history: Vec<Option<f32>>,
    min: f32,
    max: f32,
}

/// Height of every chart, so four sections read as one system.
const SPARKLINE_HEIGHT: Pixels = px(56.0);

/// Most points a chart ever plots.
///
/// A fifteen-minute window holds nine hundred samples, and a chart this size is
/// a few hundred pixels wide: plotting every sample would build a path with
/// more vertices than the chart has columns, once per second, for every
/// section. The cost of that is measurable in the idle CPU budget and none of
/// it is visible.
const MAX_PLOTTED_POINTS: usize = 180;

impl Sparkline {
    pub fn new(history: &History, min: f32, max: f32) -> Self {
        Self {
            history: downsample(history, MAX_PLOTTED_POINTS),
            min,
            max,
        }
    }

    /// Vertical position of a value inside the plot, from 0.0 at the bottom.
    fn fraction(&self, value: f32) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    /// Runs of consecutive present samples, as index ranges.
    ///
    /// Each run becomes one path, which is what leaves the holes visible.
    fn segments(&self) -> Vec<(usize, usize)> {
        let mut runs = Vec::new();
        let mut start: Option<usize> = None;
        for (index, value) in self.history.iter().enumerate() {
            match (value, start) {
                (Some(_), None) => start = Some(index),
                (None, Some(first)) => {
                    runs.push((first, index - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(first) = start {
            runs.push((first, self.history.len() - 1));
        }
        runs
    }

    pub fn render(self) -> Div {
        let colors = SparklineColors {
            line: color::ACCENT.hsla(),
            // The same accent, at full opacity. Coverage is what makes the
            // ramp, so a translucent cell would fade a texture that is already
            // fading and leave the baseline weaker than the value it stands for.
            area: color::ACCENT.hsla(),
            gap: color::TEXT_DISABLED.hsla(),
            baseline: color::SEPARATOR.alpha(0.7),
        };
        let segments = self.segments();
        // Normalized once here, so the painter only places points and the
        // scale is exercised by the same code a test can call.
        let values: Vec<Option<f32>> = self
            .history
            .iter()
            .map(|value| value.map(|value| self.fraction(value)))
            .collect();

        div()
            .w_full()
            .h(SPARKLINE_HEIGHT)
            .rounded(RADIUS)
            // Darker than the panel it sits in, which is what sets the plot
            // area apart now that no card in this interface is outlined.
            .bg(color::SURFACE.hsla())
            .child(
                canvas(
                    move |_, _, _| {},
                    move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                        paint_sparkline(window, bounds, &values, &segments, &colors);
                    },
                )
                .size_full(),
            )
    }
}

/// Reduce a series to at most `limit` plotted points.
///
/// A bucket is a gap when *any* sample inside it is missing, never when only
/// some are. Widening a hole by one bucket overstates how much is missing;
/// averaging it away would claim data that was never read, and that is the
/// error this product refuses to make.
fn downsample(history: &History, limit: usize) -> Vec<Option<f32>> {
    let samples: Vec<Option<f32>> = history.points().map(|point| point.value).collect();
    if samples.len() <= limit || limit == 0 {
        return samples;
    }

    let bucket = samples.len().div_ceil(limit);
    samples
        .chunks(bucket)
        .map(|chunk| {
            if chunk.iter().any(Option::is_none) {
                return None;
            }
            let sum: f32 = chunk.iter().flatten().sum();
            Some(sum / chunk.len() as f32)
        })
        .collect()
}

/// Side of one dither cell, in logical pixels.
///
/// Three pixels rather than one. A one-pixel cell is the honest ordered dither,
/// and on a plot a few hundred pixels wide it is also tens of thousands of
/// quads repainted every sample, on four sections at once, which is spendable
/// only against a budget this process does not have. At three the texture still
/// reads as a texture and the cell count stays in the low thousands.
const DITHER_CELL: Pixels = px(3.0);

/// Bayer ordered-dither threshold map, as its integer ranks.
///
/// The classic recursive 4x4 matrix. Kept as ranks rather than as fractions so
/// the table can be checked for what it has to be, a permutation of `0..16`: a
/// transposed or duplicated entry does not break the fill, it just makes the
/// texture quietly uneven, which is the kind of defect nobody finds by looking.
const BAYER_RANKS: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Distinct densities the map resolves.
const BAYER_LEVELS: f32 = 16.0;

/// Whether the cell at this grid position is lit at this density.
///
/// The dither is what carries the tone: a cell is either the full color or
/// nothing, and the eye integrates the coverage. Nothing here is translucent,
/// which is what keeps the texture crisp at any window scale.
fn dither_cell(column: usize, row: usize, density: f32) -> bool {
    let rank = f32::from(BAYER_RANKS[row % 4][column % 4]);
    density.clamp(0.0, 1.0) * BAYER_LEVELS > rank
}

/// Height of the plotted series at one horizontal position, from 0.0 at the
/// baseline, or `None` where the series has a hole there.
///
/// A hole on either side of the position is a hole at the position. The line
/// leaves gaps visible for the same reason, and an area that closed over one
/// would invent the volume the line refuses to invent.
fn column_fraction(values: &[Option<f32>], position: f32) -> Option<f32> {
    if values.len() < 2 {
        return None;
    }
    let last = values.len() - 1;
    let scaled = position.clamp(0.0, 1.0) * last as f32;
    let low = scaled.floor() as usize;
    let high = scaled.ceil() as usize;
    let start = values.get(low).copied().flatten()?;
    let end = values.get(high).copied().flatten()?;
    Some(start + (end - start) * (scaled - low as f32))
}

/// Dither density at a depth below the curve, over the depth of the column.
///
/// Dense at the baseline and thinning out toward the line, which is what makes
/// the fill read as volume under a value rather than as a second series.
fn area_density(depth: f32, span: f32) -> f32 {
    if span <= 0.0 || depth <= 0.0 {
        return 0.0;
    }
    (depth / span).clamp(0.0, 1.0)
}

/// Whether a point falls inside a rounded rectangle.
///
/// GPUI masks paint to a rectangle, so a cell landing in a corner would square
/// off the curve the container's own fill draws. Cells are tested one by one
/// rather than the grid being inset, because an inset would leave a bare band
/// exactly along the baseline, where the texture is densest.
fn inside_rounded(bounds: Bounds<Pixels>, x: f32, y: f32, radius: f32) -> bool {
    let left = f32::from(bounds.origin.x) + radius;
    let right = f32::from(bounds.origin.x + bounds.size.width) - radius;
    let top = f32::from(bounds.origin.y) + radius;
    let bottom = f32::from(bounds.origin.y + bounds.size.height) - radius;
    // A container narrower or shorter than two radii inverts the bounds, and
    // `f32::clamp` panics on an inverted range rather than saturating.
    let dx = x - x.clamp(left.min(right), right.max(left));
    let dy = y - y.clamp(top.min(bottom), bottom.max(top));
    dx * dx + dy * dy <= radius * radius
}

/// Fill under the curve, as an ordered-dither ramp.
fn paint_dithered_area(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[Option<f32>],
    radius: Pixels,
    color: Hsla,
) {
    if values.len() < 2 || bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
        return;
    }

    let columns = (bounds.size.width / DITHER_CELL).ceil() as usize;
    let rows = (bounds.size.height / DITHER_CELL).ceil() as usize;
    let height = f32::from(bounds.size.height);
    let radius = f32::from(radius);

    for column in 0..columns {
        let x = bounds.origin.x + DITHER_CELL * column as f32;
        let center_x = f32::from(x) + f32::from(DITHER_CELL) * 0.5;
        let position = (center_x - f32::from(bounds.origin.x)) / f32::from(bounds.size.width);
        let Some(fraction) = column_fraction(values, position) else {
            continue;
        };

        let top = height * (1.0 - fraction.clamp(0.0, 1.0));
        for row in 0..rows {
            let y = bounds.origin.y + DITHER_CELL * row as f32;
            let center_y = f32::from(y) + f32::from(DITHER_CELL) * 0.5;
            let depth = center_y - f32::from(bounds.origin.y) - top;
            let density = area_density(depth, height - top);
            if density <= 0.0
                || !dither_cell(column, row, density)
                || !inside_rounded(bounds, center_x, center_y, radius)
            {
                continue;
            }

            // Clamped so the last cell of a row or column cannot paint past
            // the plot it belongs to.
            let width = DITHER_CELL.min(bounds.origin.x + bounds.size.width - x);
            let cell_height = DITHER_CELL.min(bounds.origin.y + bounds.size.height - y);
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(width, cell_height)),
                color,
            ));
        }
    }
}

/// The colors one sparkline is painted in.
struct SparklineColors {
    line: Hsla,
    area: Hsla,
    gap: Hsla,
    baseline: Hsla,
}

fn paint_sparkline(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[Option<f32>],
    segments: &[(usize, usize)],
    colors: &SparklineColors,
) {
    let SparklineColors {
        line: line_color,
        area: area_color,
        gap: gap_color,
        baseline: baseline_color,
    } = *colors;

    paint_dithered_area(window, bounds, values, RADIUS, area_color);

    stroke_line(
        window,
        Point {
            x: bounds.origin.x,
            y: bounds.origin.y + bounds.size.height,
        },
        Point {
            x: bounds.origin.x + bounds.size.width,
            y: bounds.origin.y + bounds.size.height,
        },
        px(1.0),
        baseline_color,
    );

    if values.len() < 2 {
        return;
    }

    let last = (values.len() - 1) as f32;
    let position = |index: usize, fraction: f32| Point {
        x: bounds.origin.x + bounds.size.width * (index as f32 / last),
        y: bounds.origin.y + bounds.size.height * (1.0 - fraction.clamp(0.0, 1.0)),
    };

    for (first, last_index) in segments {
        if last_index == first {
            continue;
        }
        let mut builder = PathBuilder::stroke(px(2.0));
        for (offset, value) in values[*first..=*last_index].iter().enumerate() {
            let Some(value) = value else { continue };
            let index = first + offset;
            let point = position(index, *value);
            if offset == 0 {
                builder.move_to(point);
            } else {
                builder.line_to(point);
            }
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, line_color);
        }
    }

    // A tick at the baseline under every hole: the break in the line is the
    // primary cue, and this makes it legible even for a single missing sample.
    for (index, value) in values.iter().enumerate() {
        if value.is_some() {
            continue;
        }
        let x = bounds.origin.x + bounds.size.width * (index as f32 / last);
        stroke_line(
            window,
            Point {
                x,
                y: bounds.origin.y + bounds.size.height,
            },
            Point {
                x,
                y: bounds.origin.y + bounds.size.height - px(6.0),
            },
            px(1.5),
            gap_color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sparkline_breaks_its_line_at_every_gap() {
        let mut history = History::new(60_000);
        for (step, value) in [Some(1.0), Some(2.0), None, Some(4.0), Some(5.0)]
            .into_iter()
            .enumerate()
        {
            history.push(step as u64 * 1_000, value);
        }

        let sparkline = Sparkline::new(&history, 0.0, 10.0);
        assert_eq!(sparkline.segments(), vec![(0, 1), (3, 4)]);
        assert!((sparkline.fraction(5.0) - 0.5).abs() < 0.001);
        assert_eq!(sparkline.fraction(-4.0), 0.0);
        assert_eq!(sparkline.fraction(40.0), 1.0);
    }

    #[test]
    fn a_full_window_is_reduced_to_a_bounded_number_of_plotted_points() {
        let mut history = History::new(kori_core::telemetry::HISTORY_WINDOW_MS);
        for step in 0..900u64 {
            history.push(step * 1_000, Some(step as f32 % 100.0));
        }
        assert_eq!(history.len(), 900);

        let sparkline = Sparkline::new(&history, 0.0, 100.0);
        assert!(
            sparkline.history.len() <= MAX_PLOTTED_POINTS,
            "{} points plotted",
            sparkline.history.len()
        );
        assert!(sparkline.history.iter().all(Option::is_some));
    }

    #[test]
    fn downsampling_never_hides_a_gap() {
        let mut history = History::new(kori_core::telemetry::HISTORY_WINDOW_MS);
        for step in 0..900u64 {
            history.push(step * 1_000, if step == 500 { None } else { Some(1.0) });
        }

        let sparkline = Sparkline::new(&history, 0.0, 10.0);
        assert_eq!(
            sparkline.history.iter().filter(|v| v.is_none()).count(),
            1,
            "the single missing sample must survive as a hole"
        );
        // Two runs remain, which is what makes the break visible.
        assert_eq!(sparkline.segments().len(), 2);
    }

    #[test]
    fn a_short_window_is_plotted_sample_for_sample() {
        let mut history = History::new(60_000);
        for step in 0..20u64 {
            history.push(step * 1_000, Some(step as f32));
        }
        assert_eq!(Sparkline::new(&history, 0.0, 20.0).history.len(), 20);
    }

    #[test]
    fn a_sparkline_of_only_gaps_draws_no_segment() {
        let mut history = History::new(60_000);
        for step in 0..4u64 {
            history.push(step * 1_000, None);
        }
        assert!(Sparkline::new(&history, 0.0, 10.0).segments().is_empty());
    }

    #[test]
    fn the_dither_map_is_a_permutation_of_its_levels() {
        let mut seen: Vec<u8> = BAYER_RANKS.iter().flatten().copied().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..BAYER_LEVELS as u8).collect::<Vec<u8>>());
    }

    #[test]
    fn the_dither_lights_no_cell_when_empty_and_every_cell_when_full() {
        for row in 0..4 {
            for column in 0..4 {
                assert!(!dither_cell(column, row, 0.0));
                assert!(dither_cell(column, row, 1.0));
            }
        }
    }

    /// Coverage has to track density, or the ramp is not a ramp.
    #[test]
    fn the_dither_lights_more_cells_as_the_density_rises() {
        let lit = |density: f32| {
            (0..4)
                .flat_map(|row| (0..4).map(move |column| (column, row)))
                .filter(|(column, row)| dither_cell(*column, *row, density))
                .count()
        };
        assert_eq!(lit(0.5), 8, "half density has to light half the map");
        let mut previous = 0;
        for step in 0..=16 {
            let count = lit(step as f32 / 16.0);
            assert!(count >= previous, "coverage fell at {step}");
            previous = count;
        }
        assert_eq!(previous, 16);
    }

    /// The fill is dense at the baseline and thin at the line, never inverted.
    #[test]
    fn the_area_density_is_highest_at_the_baseline() {
        assert_eq!(area_density(0.0, 40.0), 0.0);
        assert!((area_density(20.0, 40.0) - 0.5).abs() < 0.001);
        assert_eq!(area_density(40.0, 40.0), 1.0);
        assert_eq!(area_density(80.0, 40.0), 1.0);
        // A column with no depth under it, and a curve sitting on the baseline.
        assert_eq!(area_density(5.0, 0.0), 0.0);
        assert_eq!(area_density(-5.0, 40.0), 0.0);
    }

    #[test]
    fn the_area_follows_the_series_between_its_samples() {
        let values = vec![Some(0.0), Some(1.0), Some(0.0)];
        assert_eq!(column_fraction(&values, 0.0), Some(0.0));
        assert_eq!(column_fraction(&values, 0.5), Some(1.0));
        assert_eq!(column_fraction(&values, 1.0), Some(0.0));
        assert!((column_fraction(&values, 0.25).unwrap() - 0.5).abs() < 0.001);
        // Off either end, rather than extrapolating a value nothing measured.
        assert_eq!(column_fraction(&values, -1.0), Some(0.0));
        assert_eq!(column_fraction(&values, 2.0), Some(0.0));
    }

    /// A hole in the series is a hole in the fill, exactly as it is in the line.
    #[test]
    fn the_area_opens_where_the_series_has_a_hole() {
        let values = vec![Some(1.0), None, Some(1.0)];
        assert_eq!(column_fraction(&values, 0.5), None);
        assert_eq!(column_fraction(&values, 0.3), None);
        assert_eq!(column_fraction(&values, 0.0), Some(1.0));
        assert_eq!(column_fraction(&values, 1.0), Some(1.0));
        assert_eq!(column_fraction(&[Some(1.0)], 0.0), None);
        assert_eq!(column_fraction(&[], 0.0), None);
    }

    #[test]
    fn no_dither_cell_lands_outside_the_rounded_plot() {
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(56.0)));
        let radius = f32::from(RADIUS);
        assert!(inside_rounded(bounds, 50.0, 28.0, radius));
        assert!(inside_rounded(bounds, 1.0, 28.0, radius));
        assert!(inside_rounded(bounds, 50.0, 55.0, radius));
        // The four corners, which is the whole reason this test exists.
        for (x, y) in [(0.5, 0.5), (99.5, 0.5), (0.5, 55.5), (99.5, 55.5)] {
            assert!(!inside_rounded(bounds, x, y, radius), "corner {x},{y}");
        }
    }

    /// `f32::clamp` panics on an inverted range, and a plot narrower than two
    /// radii inverts it. The window is resizable, so this is reachable.
    #[test]
    fn a_plot_smaller_than_its_own_radius_still_answers() {
        let radius = f32::from(RADIUS);
        for side in [0.0, 1.0, 4.0, 15.0] {
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(side), px(side)));
            // Reaching the assertion at all is the point. What it checks is
            // that the middle of a plot is never culled, however small it got.
            let middle = side / 2.0;
            assert!(
                inside_rounded(bounds, middle, middle, radius),
                "side {side}"
            );
        }
    }
}
