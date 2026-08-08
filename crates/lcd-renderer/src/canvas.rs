// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! A small software rasterizer, sized for one 240 by 240 panel.
//!
//! Two primitives cover everything the panel draws: an annular arc for the
//! gauges and a filled outline for every glyph. Each writes coverage rather
//! than hard pixels, so an edge lands as a blend instead of a staircase.
//!
//! Nothing here knows what a metric is. It takes coordinates and colors.

use nzxt_core::lighting::Rgb;

/// A straight RGB8 buffer, one entry per pixel, row major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<Rgb>,
}

impl Canvas {
    /// A canvas of `width` by `height`, filled with `background`.
    pub fn filled(width: u32, height: u32, background: Rgb) -> Self {
        Self {
            width,
            height,
            pixels: vec![background; (width as usize) * (height as usize)],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[Rgb] {
        &self.pixels
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb> {
        (x < self.width && y < self.height)
            .then(|| self.pixels[(y as usize) * (self.width as usize) + (x as usize)])
    }

    /// Mix `color` into one pixel at `coverage`, from 0 to 1.
    pub fn blend(&mut self, x: i32, y: i32, color: Rgb, coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let coverage = coverage.min(1.0);
        let index = (y as usize) * (self.width as usize) + (x as usize);
        let under = self.pixels[index];
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * coverage).round() as u8;
        self.pixels[index] = Rgb {
            r: mix(under.r, color.r),
            g: mix(under.g, color.g),
            b: mix(under.b, color.b),
        };
    }

    /// Fill a closed outline under the non-zero winding rule.
    ///
    /// This is what the numerals are drawn with. It is scanline rather than
    /// point-sampled: four sample rows per pixel, each turned into spans whose
    /// ends carry fractional coverage. The cost is proportional to the height of
    /// the glyph times its edge count rather than to its area times a sample
    /// grid, which is what keeps a three-digit reading inside the repaint
    /// budget while still resolving a curve cleanly at 60 pixels tall.
    ///
    /// Non-zero rather than even-odd on purpose: a glyph is assembled from
    /// strokes that overlap at their joins, and every stroke is wound the same
    /// way, so an overlap stays filled instead of punching a hole.
    pub fn fill_outline(&mut self, outline: &Outline, color: Rgb) {
        let edges = outline.edges();
        if edges.is_empty() {
            return;
        }

        let (mut min_x, mut max_x, mut min_y, mut max_y) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for (a, b) in &edges {
            min_x = min_x.min(a.0).min(b.0);
            max_x = max_x.max(a.0).max(b.0);
            min_y = min_y.min(a.1).min(b.1);
            max_y = max_y.max(a.1).max(b.1);
        }
        let left = (min_x.floor() as i32).max(0);
        let right = (max_x.ceil() as i32).min(self.width as i32 - 1);
        let top = (min_y.floor() as i32).max(0);
        let bottom = (max_y.ceil() as i32).min(self.height as i32 - 1);
        if left > right || top > bottom {
            return;
        }

        let span = (right - left + 1) as usize;
        let mut coverage = vec![0.0f32; span];
        let mut crossings: Vec<(f32, i32)> = Vec::new();
        let weight = 1.0 / OUTLINE_SAMPLES as f32;

        for row in top..=bottom {
            coverage.fill(0.0);
            for sample in 0..OUTLINE_SAMPLES {
                let y = row as f32 + (sample as f32 + 0.5) / OUTLINE_SAMPLES as f32;
                crossings.clear();
                for (a, b) in &edges {
                    // Half-open in y so a vertex shared by two edges is counted
                    // once, which is what keeps a join from leaving a pinhole.
                    let (lower, upper, direction) = if a.1 < b.1 { (a, b, 1) } else { (b, a, -1) };
                    if y < lower.1 || y >= upper.1 {
                        continue;
                    }
                    let t = (y - lower.1) / (upper.1 - lower.1);
                    crossings.push((lower.0 + (upper.0 - lower.0) * t, direction));
                }
                if crossings.len() < 2 {
                    continue;
                }
                crossings.sort_by(|a, b| a.0.total_cmp(&b.0));

                let mut winding = 0;
                let mut start = 0.0f32;
                for (x, direction) in &crossings {
                    if winding == 0 {
                        start = *x;
                    }
                    winding += direction;
                    if winding == 0 {
                        accumulate_span(&mut coverage, left, start, *x, weight);
                    }
                }
            }
            for (index, amount) in coverage.iter().enumerate() {
                self.blend(left + index as i32, row, color, *amount);
            }
        }
    }

    /// Fill part of a ring, from `start_turns` clockwise for `sweep_turns`.
    ///
    /// Angles are turns rather than radians or degrees, with zero at twelve
    /// o'clock and growing clockwise, because every caller here thinks in
    /// fractions of a gauge.
    ///
    /// Coverage is analytic rather than supersampled: a ring covers most of the
    /// panel, and nine point tests per pixel over that area is the one thing
    /// that would put the preview past its repaint budget.
    pub fn fill_arc(&mut self, arc: Arc, color: Rgb) {
        self.fill_arc_gradient(arc, color, color);
    }

    /// The same band, shading from `from` at its start to `to` at its end.
    ///
    /// One gauge reads as a single quantity rising rather than as a bar that
    /// happens to be colored, which is the whole reason the panel is a dial. The
    /// shade runs along the sweep and not across the band, so a thin track and a
    /// thick one carry the same progression.
    pub fn fill_arc_gradient(&mut self, arc: Arc, from: Rgb, to: Rgb) {
        if arc.sweep_turns < 0.0 || arc.outer <= arc.inner {
            return;
        }
        let sweep = arc.sweep_turns.min(1.0);
        if sweep <= 0.0 && !arc.round_caps {
            return;
        }
        let reach = arc.outer.ceil() as i32 + 1;
        let center_x = arc.center.0;
        let center_y = arc.center.1;

        for row in (center_y as i32 - reach)..=(center_y as i32 + reach) {
            for column in (center_x as i32 - reach)..=(center_x as i32 + reach) {
                let dx = column as f32 + 0.5 - center_x;
                let dy = row as f32 + 0.5 - center_y;
                let distance = (dx * dx + dy * dy).sqrt();
                if distance > arc.outer + 1.0 || distance < arc.inner - 1.0 {
                    continue;
                }

                // Radial coverage: one pixel of softness on each edge of the band.
                let radial = ((arc.outer + 0.5 - distance).clamp(0.0, 1.0))
                    * ((distance - arc.inner + 0.5).clamp(0.0, 1.0));
                if radial <= 0.0 {
                    continue;
                }

                let offset = wrap_turns(turns_at(dx, dy) - arc.start_turns);
                let coverage = if sweep >= 1.0 {
                    radial
                } else {
                    // Angular coverage, with a softness of half a pixel
                    // expressed as the fraction of a turn it spans at this
                    // radius, so the ends of an arc are as smooth as its sides.
                    let softness = if distance > 0.5 {
                        0.5 / (std::f32::consts::TAU * distance)
                    } else {
                        0.5
                    };
                    // The half turns the ramp into a coverage: a pixel whose
                    // center sits exactly on the end of the arc is half
                    // covered, not empty.
                    let from_start = (offset / softness + 0.5).clamp(0.0, 1.0);
                    let to_end = ((sweep - offset) / softness + 0.5).clamp(0.0, 1.0);
                    radial * from_start.min(to_end)
                };
                let color = if from == to {
                    from
                } else {
                    lerp(from, to, (offset / sweep).clamp(0.0, 1.0))
                };
                self.blend(column, row, color, coverage);
            }
        }

        // The ends last, so each covers the square edge the sweep left behind
        // rather than being cut by it. A full ring has no end to round off, and
        // a sweep of nothing is left as the single dot both ends make together,
        // which is what a gauge sitting at zero should look like.
        if arc.round_caps && sweep < 1.0 {
            let middle = (arc.inner + arc.outer) / 2.0;
            let cap = (arc.outer - arc.inner) / 2.0;
            self.fill_disc(polar(arc.center, middle, arc.start_turns), cap, from);
            self.fill_disc(polar(arc.center, middle, arc.start_turns + sweep), cap, to);
        }
    }

    /// Fill a disc, with one pixel of softness at its edge.
    ///
    /// Analytic like the arcs, and for the same reason: it is only ever used to
    /// round off a band whose sides are already drawn that way, so the two have
    /// to meet without a seam.
    fn fill_disc(&mut self, center: (f32, f32), radius: f32, color: Rgb) {
        if radius <= 0.0 {
            return;
        }
        let reach = radius.ceil() as i32 + 1;
        for row in (center.1 as i32 - reach)..=(center.1 as i32 + reach) {
            for column in (center.0 as i32 - reach)..=(center.0 as i32 + reach) {
                let dx = column as f32 + 0.5 - center.0;
                let dy = row as f32 + 0.5 - center.1;
                let coverage = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
                self.blend(column, row, color, coverage);
            }
        }
    }
}

/// The point `radius` from `center` at `turns` clockwise from twelve o'clock.
fn polar(center: (f32, f32), radius: f32, turns: f32) -> (f32, f32) {
    let angle = turns * std::f32::consts::TAU;
    (
        center.0 + radius * angle.sin(),
        // Screen y grows downward, so twelve o'clock is the negative direction.
        center.1 - radius * angle.cos(),
    )
}

/// Vertical samples taken per pixel row when filling an outline.
///
/// Four is where the ramp on a near-horizontal edge stops being visible at the
/// sizes the numerals are drawn at. Eight costs twice as much and changes
/// nothing a 240 pixel panel in RGB565 can show.
const OUTLINE_SAMPLES: u32 = 4;

/// How finely a quadratic curve is broken into straight segments.
///
/// Segments per curve, fixed rather than derived from the curve's length: the
/// numerals are built at one size range, and a fixed count keeps the glyph
/// construction allocation-predictable.
const CURVE_SEGMENTS: u32 = 12;

/// A closed shape, as one or more subpaths in device pixels.
///
/// Curves are flattened on the way in, so the rasterizer only ever sees line
/// segments. Every subpath is implicitly closed when the outline is filled: a
/// glyph is an area, and an open contour has no area to fill.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outline {
    subpaths: Vec<Vec<(f32, f32)>>,
}

impl Outline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new subpath at `point`.
    pub fn move_to(&mut self, point: (f32, f32)) {
        self.subpaths.push(vec![point]);
    }

    /// Extend the current subpath with a straight segment.
    ///
    /// A `line_to` with no `move_to` before it starts a subpath rather than
    /// being dropped, so a builder mistake produces a visible shape instead of
    /// silent nothing.
    pub fn line_to(&mut self, point: (f32, f32)) {
        match self.subpaths.last_mut() {
            Some(subpath) => subpath.push(point),
            None => self.move_to(point),
        }
    }

    /// Extend the current subpath with a quadratic curve through `control`.
    pub fn quad_to(&mut self, control: (f32, f32), point: (f32, f32)) {
        let Some(from) = self
            .subpaths
            .last()
            .and_then(|subpath| subpath.last())
            .copied()
        else {
            self.move_to(point);
            return;
        };
        for step in 1..=CURVE_SEGMENTS {
            let t = step as f32 / CURVE_SEGMENTS as f32;
            let inverse = 1.0 - t;
            self.line_to((
                inverse * inverse * from.0 + 2.0 * inverse * t * control.0 + t * t * point.0,
                inverse * inverse * from.1 + 2.0 * inverse * t * control.1 + t * t * point.1,
            ));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.subpaths.iter().all(|subpath| subpath.len() < 3)
    }

    /// Move every point by `(dx, dy)`.
    pub fn translated(&self, dx: f32, dy: f32) -> Self {
        Self {
            subpaths: self
                .subpaths
                .iter()
                .map(|subpath| {
                    subpath
                        .iter()
                        .map(|(x, y)| (x + dx, y + dy))
                        .collect::<Vec<_>>()
                })
                .collect(),
        }
    }

    /// Wind every subpath the same way.
    ///
    /// A glyph is assembled from strokes built by separate helpers, and two
    /// overlapping subpaths of opposite winding cancel to nothing under the
    /// non-zero rule: the overlap would be a hole exactly where two strokes
    /// join. Reversing the subpaths that enclose a negative area removes the
    /// possibility rather than leaving each builder to remember it.
    pub fn normalize_winding(&mut self) {
        for subpath in &mut self.subpaths {
            if subpath.len() < 3 {
                continue;
            }
            let mut area = 0.0;
            for index in 0..subpath.len() {
                let (ax, ay) = subpath[index];
                let (bx, by) = subpath[(index + 1) % subpath.len()];
                area += ax * by - bx * ay;
            }
            if area < 0.0 {
                subpath.reverse();
            }
        }
    }

    /// Take every subpath of `other` into this outline.
    pub fn absorb(&mut self, other: Outline) {
        self.subpaths.extend(other.subpaths);
    }

    /// The horizontal extent of the outline, or `None` when it has no area.
    pub fn horizontal_extent(&self) -> Option<(f32, f32)> {
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for subpath in &self.subpaths {
            if subpath.len() < 3 {
                continue;
            }
            for (x, _) in subpath {
                min = min.min(*x);
                max = max.max(*x);
            }
        }
        (min <= max).then_some((min, max))
    }

    /// Every segment of every subpath, closing each one, horizontals dropped.
    ///
    /// A horizontal edge contributes no crossing to any sample row, so leaving
    /// it out of the list is not an approximation: it is the same result with
    /// less work per row.
    fn edges(&self) -> Vec<((f32, f32), (f32, f32))> {
        let mut edges = Vec::new();
        for subpath in &self.subpaths {
            if subpath.len() < 3 {
                continue;
            }
            for index in 0..subpath.len() {
                let a = subpath[index];
                let b = subpath[(index + 1) % subpath.len()];
                if a.1 != b.1 {
                    edges.push((a, b));
                }
            }
        }
        edges
    }
}

/// Add one horizontal span's coverage into a row accumulator.
///
/// The ends carry the fraction of the pixel they actually cover, which is what
/// makes a near-vertical stem land as one soft column rather than as a staircase
/// that moves by a whole pixel.
fn accumulate_span(coverage: &mut [f32], origin: i32, x0: f32, x1: f32, weight: f32) {
    let left = x0.max(origin as f32);
    let right = x1.min(origin as f32 + coverage.len() as f32);
    if right <= left {
        return;
    }
    let first = left.floor() as i32;
    let last = (right.ceil() as i32) - 1;
    for column in first..=last {
        let index = column - origin;
        if index < 0 || index as usize >= coverage.len() {
            continue;
        }
        let covered = (column as f32 + 1.0).min(right) - (column as f32).max(left);
        if covered > 0.0 {
            coverage[index as usize] += covered * weight;
        }
    }
}

/// `from` moved `t` of the way toward `to`.
fn lerp(from: Rgb, to: Rgb, t: f32) -> Rgb {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Rgb {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
    }
}

/// One annular arc: where it is, how thick, and how far around it goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    pub center: (f32, f32),
    pub inner: f32,
    pub outer: f32,
    /// Zero is twelve o'clock, growing clockwise, in turns.
    pub start_turns: f32,
    pub sweep_turns: f32,
    /// Round both ends of the sweep off with a half disc of the band's own
    /// thickness, which extends the arc past the angles it names by that much.
    pub round_caps: bool,
}

/// The angle of a screen-space offset, in turns clockwise from twelve o'clock.
fn turns_at(dx: f32, dy: f32) -> f32 {
    // Screen y grows downward, so clockwise on screen is the mathematically
    // positive direction here. Twelve o'clock is (0, -1).
    wrap_turns(dx.atan2(-dy) / std::f32::consts::TAU)
}

/// Reduce a turn count into `0.0..1.0`.
fn wrap_turns(turns: f32) -> f32 {
    let wrapped = turns % 1.0;
    if wrapped < 0.0 {
        wrapped + 1.0
    } else {
        wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);
    const BLACK: Rgb = Rgb::BLACK;

    #[test]
    fn a_new_canvas_is_uniformly_its_background() {
        let canvas = Canvas::filled(8, 4, Rgb::new(1, 2, 3));
        assert_eq!(canvas.pixels().len(), 32);
        assert!(canvas.pixels().iter().all(|p| *p == Rgb::new(1, 2, 3)));
        assert_eq!(canvas.pixel(7, 3), Some(Rgb::new(1, 2, 3)));
        assert_eq!(canvas.pixel(8, 0), None, "out of bounds reads nothing");
    }

    /// A rectangle as an outline, which is the only shape primitive left.
    fn rectangle(x0: f32, y0: f32, x1: f32, y1: f32) -> Outline {
        let mut outline = Outline::new();
        outline.move_to((x0, y0));
        outline.line_to((x1, y0));
        outline.line_to((x1, y1));
        outline.line_to((x0, y1));
        outline
    }

    #[test]
    fn drawing_outside_the_canvas_changes_nothing_and_does_not_panic() {
        let mut canvas = Canvas::filled(4, 4, BLACK);
        canvas.fill_outline(&rectangle(-10.0, -10.0, -6.0, -6.0), WHITE);
        canvas.fill_outline(&rectangle(100.0, 100.0, 104.0, 104.0), WHITE);
        canvas.blend(-1, 2, WHITE, 1.0);
        canvas.blend(2, 9999, WHITE, 1.0);
        assert!(canvas.pixels().iter().all(|p| *p == BLACK));
    }

    #[test]
    fn an_outline_on_pixel_boundaries_covers_exactly_its_own_pixels() {
        let mut canvas = Canvas::filled(6, 6, BLACK);
        canvas.fill_outline(&rectangle(2.0, 1.0, 4.0, 4.0), WHITE);
        let lit: Vec<(u32, u32)> = (0..6)
            .flat_map(|y| (0..6).map(move |x| (x, y)))
            .filter(|(x, y)| canvas.pixel(*x, *y) == Some(WHITE))
            .collect();
        assert_eq!(lit, vec![(2, 1), (3, 1), (2, 2), (3, 2), (2, 3), (3, 3)]);
    }

    #[test]
    fn an_edge_between_two_pixels_lands_as_coverage_rather_than_a_staircase() {
        // Half a pixel of the column is covered, so the column is half lit.
        let mut canvas = Canvas::filled(4, 4, BLACK);
        canvas.fill_outline(&rectangle(1.5, 0.0, 3.0, 4.0), WHITE);
        let partial = canvas.pixel(1, 0).unwrap();
        assert!(
            (120..=135).contains(&partial.r),
            "a half-covered column should be mid grey, got {partial:?}"
        );
        assert_eq!(canvas.pixel(2, 0), Some(WHITE));
        assert_eq!(canvas.pixel(0, 0), Some(BLACK));
    }

    #[test]
    fn two_overlapping_subpaths_stay_filled_once_they_are_wound_alike() {
        // The mistake this pins: a glyph is built from strokes that overlap at
        // their joins, and two subpaths of opposite winding cancel to nothing
        // under the non-zero rule, punching a hole exactly at the join.
        let mut crossed = rectangle(1.0, 1.0, 9.0, 5.0);
        let mut reversed = Outline::new();
        reversed.move_to((3.0, 4.0));
        reversed.line_to((3.0, 2.0));
        reversed.line_to((7.0, 2.0));
        reversed.line_to((7.0, 4.0));
        crossed.absorb(reversed);
        crossed.normalize_winding();

        let mut canvas = Canvas::filled(10, 6, BLACK);
        canvas.fill_outline(&crossed, WHITE);
        assert_eq!(canvas.pixel(5, 3), Some(WHITE), "the overlap became a hole");
    }

    #[test]
    fn coverage_blends_rather_than_replaces() {
        let mut canvas = Canvas::filled(1, 1, BLACK);
        canvas.blend(0, 0, WHITE, 0.5);
        let blended = canvas.pixel(0, 0).unwrap();
        assert!(
            (126..=129).contains(&blended.r),
            "half coverage of white on black is mid grey, got {blended:?}"
        );

        // Zero and negative coverage leave the pixel alone; above one is capped.
        canvas.blend(0, 0, BLACK, 0.0);
        assert_eq!(canvas.pixel(0, 0).unwrap().r, blended.r);
        canvas.blend(0, 0, WHITE, 4.0);
        assert_eq!(canvas.pixel(0, 0), Some(WHITE));
    }

    #[test]
    fn twelve_oclock_is_zero_turns_and_the_sweep_runs_clockwise() {
        // Directly above the center is zero, to the right is a quarter turn,
        // below is a half. This is the convention every caller depends on.
        assert!((turns_at(0.0, -10.0) - 0.0).abs() < 1e-4);
        assert!((turns_at(10.0, 0.0) - 0.25).abs() < 1e-4);
        assert!((turns_at(0.0, 10.0) - 0.5).abs() < 1e-4);
        assert!((turns_at(-10.0, 0.0) - 0.75).abs() < 1e-4);
    }

    #[test]
    fn an_arc_fills_only_between_its_radii() {
        let mut canvas = Canvas::filled(41, 41, BLACK);
        canvas.fill_arc(
            Arc {
                center: (20.5, 20.5),
                inner: 10.0,
                outer: 16.0,
                start_turns: 0.0,
                sweep_turns: 1.0,
                round_caps: false,
            },
            WHITE,
        );

        // The middle of the ring is fully lit, the hole and the outside are not.
        assert_eq!(canvas.pixel(20, 7), Some(WHITE), "top of the band");
        assert_eq!(canvas.pixel(20, 20), Some(BLACK), "the hole stays empty");
        assert_eq!(canvas.pixel(0, 0), Some(BLACK), "the corner is outside");
    }

    #[test]
    fn a_quarter_sweep_lights_the_quarter_it_names_and_no_other() {
        let mut canvas = Canvas::filled(41, 41, BLACK);
        canvas.fill_arc(
            Arc {
                center: (20.5, 20.5),
                inner: 12.0,
                outer: 18.0,
                start_turns: 0.0,
                sweep_turns: 0.25,
                round_caps: false,
            },
            WHITE,
        );

        // Clockwise from twelve, so the quadrant between twelve and three is
        // lit and the other three quadrants are not. Twelve itself is the
        // boundary and is asserted separately below.
        assert_eq!(canvas.pixel(23, 5), Some(WHITE), "just past twelve");
        assert_eq!(canvas.pixel(34, 17), Some(WHITE), "approaching three");
        assert_eq!(canvas.pixel(17, 5), Some(BLACK), "just before twelve");
        assert_eq!(canvas.pixel(20, 35), Some(BLACK), "six o'clock");
        assert_eq!(canvas.pixel(5, 20), Some(BLACK), "nine o'clock");

        // A pixel straddling the start is partly covered rather than snapped
        // to one side, which is what keeps the end of an arc from stepping.
        let edge = canvas.pixel(20, 5).unwrap();
        assert!(
            edge != WHITE && edge != BLACK,
            "the pixel on the boundary should be blended, got {edge:?}"
        );
    }

    #[test]
    fn an_arc_wrapping_past_twelve_stays_continuous() {
        let mut canvas = Canvas::filled(41, 41, BLACK);
        canvas.fill_arc(
            Arc {
                center: (20.5, 20.5),
                inner: 12.0,
                outer: 18.0,
                // Starts at nine o'clock and runs a half turn, so it crosses
                // the zero the angles wrap at.
                start_turns: 0.75,
                sweep_turns: 0.5,
                round_caps: false,
            },
            WHITE,
        );
        assert_eq!(
            canvas.pixel(6, 15),
            Some(WHITE),
            "just past nine, the start"
        );
        assert_eq!(canvas.pixel(20, 5), Some(WHITE), "twelve, across the wrap");
        assert_eq!(
            canvas.pixel(34, 16),
            Some(WHITE),
            "approaching three, the end"
        );
        assert_eq!(canvas.pixel(20, 35), Some(BLACK), "six is outside it");
        assert_eq!(canvas.pixel(8, 30), Some(BLACK), "and so is seven");
    }

    #[test]
    fn a_rounded_end_extends_the_band_past_the_angle_it_names() {
        // What a round cap is: half a disc of the band's own thickness, added
        // at each end. It reaches past the angle the sweep names by half the
        // band's width, which is the difference between a gauge that looks
        // machined and one that looks cut off.
        let arc = |round_caps| Arc {
            center: (20.5, 20.5),
            inner: 12.0,
            outer: 18.0,
            start_turns: 0.25,
            sweep_turns: 0.25,
            round_caps,
        };
        let mut square = Canvas::filled(41, 41, BLACK);
        let mut rounded = Canvas::filled(41, 41, BLACK);
        square.fill_arc(arc(false), WHITE);
        rounded.fill_arc(arc(true), WHITE);

        // Just before three o'clock, where the sweep starts: empty when the end
        // is square, lit when it is round.
        assert_eq!(square.pixel(35, 17), Some(BLACK));
        assert_ne!(rounded.pixel(35, 17), Some(BLACK), "the cap is missing");
        // The far side of the band is untouched either way.
        assert_eq!(square.pixel(20, 5), Some(BLACK));
        assert_eq!(rounded.pixel(20, 5), Some(BLACK));
        // And the middle of the sweep, well away from either end, is identical.
        assert_eq!(
            square.pixel(31, 31),
            rounded.pixel(31, 31),
            "rounding an end changed the middle of the band"
        );
    }

    #[test]
    fn a_rounded_band_with_no_sweep_is_the_single_dot_a_gauge_at_zero_shows() {
        // Both ends fall on the same point, so the two half discs make one, and
        // a reading of zero is visible as a dot rather than as nothing at all.
        let mut canvas = Canvas::filled(41, 41, BLACK);
        canvas.fill_arc(
            Arc {
                center: (20.5, 20.5),
                inner: 12.0,
                outer: 18.0,
                start_turns: 0.0,
                sweep_turns: 0.0,
                round_caps: true,
            },
            WHITE,
        );
        assert_eq!(canvas.pixel(20, 5), Some(WHITE), "the dot sits at twelve");
        assert_eq!(canvas.pixel(20, 35), Some(BLACK), "and nowhere else");
        assert_eq!(canvas.pixel(35, 20), Some(BLACK));
    }

    #[test]
    fn an_empty_or_inverted_arc_draws_nothing() {
        let mut canvas = Canvas::filled(21, 21, BLACK);
        for arc in [
            Arc {
                center: (10.5, 10.5),
                inner: 4.0,
                outer: 9.0,
                start_turns: 0.0,
                sweep_turns: 0.0,
                round_caps: false,
            },
            Arc {
                center: (10.5, 10.5),
                inner: 9.0,
                outer: 4.0,
                start_turns: 0.0,
                sweep_turns: 1.0,
                round_caps: false,
            },
        ] {
            canvas.fill_arc(arc, WHITE);
        }
        assert!(
            canvas.pixels().iter().all(|p| *p == BLACK),
            "a zero sweep and an inverted band are both nothing, not a full ring"
        );
    }

    #[test]
    fn an_outline_with_no_area_draws_nothing_rather_than_panicking() {
        let mut canvas = Canvas::filled(20, 20, BLACK);
        let mut degenerate = Outline::new();
        degenerate.line_to((1.0, 1.0));
        degenerate.line_to((2.0, 2.0));
        canvas.fill_outline(&degenerate, WHITE);
        canvas.fill_outline(&Outline::new(), WHITE);
        assert!(degenerate.is_empty());
        assert!(canvas.pixels().iter().all(|p| *p == BLACK));
    }

    #[test]
    fn a_curve_is_flattened_into_the_outline_it_was_asked_for() {
        // A quadratic bulging to the right must put ink past the straight line
        // between its ends, and none of it outside the control polygon.
        let mut bulge = Outline::new();
        bulge.move_to((4.0, 2.0));
        bulge.quad_to((16.0, 10.0), (4.0, 18.0));
        let mut canvas = Canvas::filled(20, 20, BLACK);
        canvas.fill_outline(&bulge, WHITE);
        assert_eq!(canvas.pixel(8, 10), Some(WHITE), "inside the bulge");
        assert_eq!(canvas.pixel(14, 10), Some(BLACK), "past the curve");
        assert_eq!(canvas.pixel(8, 3), Some(BLACK), "above it");
    }

    #[test]
    fn an_arc_gradient_runs_along_the_sweep_and_ends_on_its_own_color() {
        let mut canvas = Canvas::filled(41, 41, BLACK);
        let from = Rgb::new(0x20, 0x20, 0x20);
        let to = Rgb::new(0xff, 0x00, 0x00);
        canvas.fill_arc_gradient(
            Arc {
                center: (20.5, 20.5),
                inner: 12.0,
                outer: 18.0,
                start_turns: 0.0,
                sweep_turns: 0.5,
                round_caps: false,
            },
            from,
            to,
        );
        // Twelve o'clock is the start of the sweep, six o'clock its end, and
        // three o'clock is halfway between them. Sampled one pixel inside each
        // end, where the band is fully covered and the color is not blended
        // with the background the edge sits on.
        let start = canvas.pixel(23, 6).unwrap();
        let middle = canvas.pixel(35, 20).unwrap();
        let end = canvas.pixel(23, 34).unwrap();
        assert!(
            start.r < middle.r && middle.r < end.r,
            "the shade must run along the sweep: {start:?} {middle:?} {end:?}"
        );
        assert!(end.r > 0xd0, "the sweep must reach its own color: {end:?}");
    }
}
