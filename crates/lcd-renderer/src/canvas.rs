// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! A small software rasterizer, sized for one 240 by 240 panel.
//!
//! Three primitives cover everything the panel draws: a rectangle for the
//! bitmap glyphs, a convex polygon for the seven-segment digits and an annular
//! arc for the gauges. Each writes coverage rather than hard pixels, so an edge
//! lands as a blend instead of a staircase.
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

    /// Fill an axis-aligned rectangle whose edges fall on pixel boundaries.
    ///
    /// The bitmap glyphs are drawn from these at integer positions and integer
    /// scales, so their edges are exact and need no blending.
    pub fn fill_rect(&mut self, x: i32, y: i32, width: u32, height: u32, color: Rgb) {
        for row in y..y.saturating_add(height as i32) {
            for column in x..x.saturating_add(width as i32) {
                self.blend(column, row, color, 1.0);
            }
        }
    }

    /// Fill a convex polygon, supersampled three by three for its edges.
    ///
    /// Convexity is what makes the crossing test valid: a point is inside when
    /// it is on the same side of every edge. The seven-segment bars are convex
    /// hexagons, which is the only shape this is used for.
    pub fn fill_convex(&mut self, points: &[(f32, f32)], color: Rgb) {
        if points.len() < 3 {
            return;
        }
        let (min_x, max_x, min_y, max_y) = bounds(points);
        for row in min_y..=max_y {
            for column in min_x..=max_x {
                let mut hits = 0u8;
                for sub_y in 0..3 {
                    for sub_x in 0..3 {
                        let point = (
                            column as f32 + (sub_x as f32 + 0.5) / 3.0,
                            row as f32 + (sub_y as f32 + 0.5) / 3.0,
                        );
                        if inside_convex(points, point) {
                            hits += 1;
                        }
                    }
                }
                self.blend(column, row, color, f32::from(hits) / 9.0);
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
        if arc.sweep_turns <= 0.0 || arc.outer <= arc.inner {
            return;
        }
        let sweep = arc.sweep_turns.min(1.0);
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

                let coverage = if sweep >= 1.0 {
                    radial
                } else {
                    // Angular coverage, with a softness of half a pixel
                    // expressed as the fraction of a turn it spans at this
                    // radius, so the ends of an arc are as smooth as its sides.
                    let offset = wrap_turns(turns_at(dx, dy) - arc.start_turns);
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
                self.blend(column, row, color, coverage);
            }
        }
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

fn bounds(points: &[(f32, f32)]) -> (i32, i32, i32, i32) {
    let mut min_x = f32::MAX;
    let mut max_x = f32::MIN;
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for (x, y) in points {
        min_x = min_x.min(*x);
        max_x = max_x.max(*x);
        min_y = min_y.min(*y);
        max_y = max_y.max(*y);
    }
    (
        min_x.floor() as i32,
        max_x.ceil() as i32,
        min_y.floor() as i32,
        max_y.ceil() as i32,
    )
}

/// True when `point` is on the same side of every edge of a convex polygon.
fn inside_convex(points: &[(f32, f32)], point: (f32, f32)) -> bool {
    let mut positive = false;
    let mut negative = false;
    for index in 0..points.len() {
        let (ax, ay) = points[index];
        let (bx, by) = points[(index + 1) % points.len()];
        let cross = (bx - ax) * (point.1 - ay) - (by - ay) * (point.0 - ax);
        if cross > 0.0 {
            positive = true;
        } else if cross < 0.0 {
            negative = true;
        }
        if positive && negative {
            return false;
        }
    }
    true
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

    #[test]
    fn drawing_outside_the_canvas_changes_nothing_and_does_not_panic() {
        let mut canvas = Canvas::filled(4, 4, BLACK);
        canvas.fill_rect(-10, -10, 4, 4, WHITE);
        canvas.fill_rect(100, 100, 4, 4, WHITE);
        canvas.blend(-1, 2, WHITE, 1.0);
        canvas.blend(2, 9999, WHITE, 1.0);
        assert!(canvas.pixels().iter().all(|p| *p == BLACK));
    }

    #[test]
    fn a_rectangle_covers_exactly_its_own_pixels() {
        let mut canvas = Canvas::filled(6, 6, BLACK);
        canvas.fill_rect(2, 1, 2, 3, WHITE);
        let lit: Vec<(u32, u32)> = (0..6)
            .flat_map(|y| (0..6).map(move |x| (x, y)))
            .filter(|(x, y)| canvas.pixel(*x, *y) == Some(WHITE))
            .collect();
        assert_eq!(lit, vec![(2, 1), (3, 1), (2, 2), (3, 2), (2, 3), (3, 3)]);
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
    fn an_empty_or_inverted_arc_draws_nothing() {
        let mut canvas = Canvas::filled(21, 21, BLACK);
        for arc in [
            Arc {
                center: (10.5, 10.5),
                inner: 4.0,
                outer: 9.0,
                start_turns: 0.0,
                sweep_turns: 0.0,
            },
            Arc {
                center: (10.5, 10.5),
                inner: 9.0,
                outer: 4.0,
                start_turns: 0.0,
                sweep_turns: 1.0,
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
    fn a_convex_polygon_fills_its_interior() {
        let mut canvas = Canvas::filled(20, 20, BLACK);
        canvas.fill_convex(&[(4.0, 4.0), (16.0, 4.0), (16.0, 12.0), (4.0, 12.0)], WHITE);
        assert_eq!(canvas.pixel(10, 8), Some(WHITE), "inside");
        assert_eq!(canvas.pixel(10, 16), Some(BLACK), "below it");
        assert_eq!(canvas.pixel(2, 8), Some(BLACK), "left of it");

        // Degenerate inputs draw nothing rather than panicking.
        canvas.fill_convex(&[(1.0, 1.0), (2.0, 2.0)], WHITE);
        assert_eq!(canvas.pixel(1, 1), Some(BLACK));
    }
}
