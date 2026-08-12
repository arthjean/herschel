// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! A small software rasterizer, sized for one 240 by 240 panel.
//!
//! One primitive and one operation cover everything the panel draws: an annular
//! arc for the gauges, and per-pixel coverage for everything else. The glyphs
//! land through the same coverage path, straight from the font rasterizer, so an
//! edge of a numeral and an edge of a band are blended by identical arithmetic.
//!
//! Nothing here knows what a metric is. It takes coordinates and colors.

use kori_core::lighting::Rgb;

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

    /// Hand the buffer over, so a finished picture is moved rather than copied.
    pub fn into_pixels(self) -> Vec<Rgb> {
        self.pixels
    }

    /// Read one pixel back, which is an assertion's need and not a drawing one:
    /// nothing that puts marks on a canvas ever reads it.
    #[cfg(test)]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgb> {
        (x < self.width && y < self.height)
            .then(|| self.pixels[(y as usize) * (self.width as usize) + (x as usize)])
    }

    /// Mix `color` into one pixel at `coverage`, from 0 to 1.
    pub fn blend(&mut self, x: i32, y: i32, color: Rgb, coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize) * (self.width as usize) + (x as usize);
        self.pixels[index] = self.pixels[index].mixed(color, coverage.min(1.0));
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
                    from.mixed(to, (offset / sweep).clamp(0.0, 1.0))
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

    #[test]
    fn the_buffer_leaves_the_canvas_intact_rather_than_copied() {
        let canvas = Canvas::filled(3, 2, Rgb::new(9, 8, 7));
        assert_eq!(canvas.clone().into_pixels(), canvas.pixels().to_vec());
        assert_eq!(canvas.into_pixels().len(), 6);
    }

    #[test]
    fn drawing_outside_the_canvas_changes_nothing_and_does_not_panic() {
        let mut canvas = Canvas::filled(4, 4, BLACK);
        canvas.blend(-1, 2, WHITE, 1.0);
        canvas.blend(2, 9999, WHITE, 1.0);
        canvas.blend(9999, 2, WHITE, 1.0);
        canvas.blend(2, -1, WHITE, 1.0);
        assert!(canvas.pixels().iter().all(|p| *p == BLACK));
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
        canvas.blend(0, 0, BLACK, -3.0);
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
            Arc {
                center: (10.5, 10.5),
                inner: 4.0,
                outer: 9.0,
                start_turns: 0.0,
                sweep_turns: -0.5,
                round_caps: true,
            },
        ] {
            canvas.fill_arc(arc, WHITE);
        }
        assert!(
            canvas.pixels().iter().all(|p| *p == BLACK),
            "a zero sweep, an inverted band and a negative sweep are all \
             nothing, not a full ring"
        );
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
