// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The panel's text, set in a real typeface.
//!
//! The panel is a framebuffer and nothing else: it holds no font, accepts no
//! string, and shows exactly the pixels it is given. Every glyph on the glass
//! is therefore rasterized here, which means the choice of face is entirely
//! ours and there is no reason for it to be anything but a proper one.
//!
//! The face is a subset of Noto Sans SemiBold, 98 glyphs of the 3000 the family
//! carries, cut with `pyftsubset` to the printable ASCII range plus the degree
//! sign and the middle dot. Ten kilobytes, embedded rather than looked up
//! through fontconfig: the daemon and the client have to rasterize the same
//! pixels from the same description, and a face resolved from the system would
//! make that depend on what happens to be installed.
//!
//! Sizes here are cap heights, not em sizes. A layout on a 240 pixel panel is
//! reasoned about in terms of how tall the digits look, and the em box is
//! neither visible nor constant across faces.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use kori_core::lighting::Rgb;

use crate::canvas::Canvas;

/// The subset that ships with the binary.
const PANEL_FACE: &[u8] = include_bytes!("../assets/NotoSans-SemiBold-subset.ttf");

/// Height of a capital, as a fraction of the scale `ab_glyph` is given.
///
/// Measured from the embedded file rather than declared. `ab_glyph` scales by
/// ascent minus descent, which is neither the em nor the cap height and differs
/// from face to face, so a constant here would quietly rescale every layout the
/// day the file is swapped. Measuring costs one outline, once.
fn cap_ratio() -> f32 {
    static RATIO: OnceLock<f32> = OnceLock::new();
    *RATIO.get_or_init(|| {
        let Some(face) = face() else {
            return 1.0;
        };
        // Large enough that the rounding of the outline's bounds does not
        // matter at the sizes the panel actually uses.
        const PROBE: f32 = 1000.0;
        let glyph = face
            .glyph_id('H')
            .with_scale_and_position(PxScale::from(PROBE), point(0.0, 0.0));
        match face.outline_glyph(glyph) {
            // The outline sits above the baseline, so its top edge is negative.
            Some(outlined) => -outlined.px_bounds().min.y / PROBE,
            None => 1.0,
        }
    })
}

/// The parsed face, or `None` if the embedded bytes are not a font.
///
/// Parsed once. A failure here would mean the shipped file is corrupt, which no
/// caller can do anything about, so it degrades to drawing nothing rather than
/// panicking on a thread that owns hardware.
fn face() -> Option<&'static FontRef<'static>> {
    static FACE: OnceLock<Option<FontRef<'static>>> = OnceLock::new();
    FACE.get_or_init(|| FontRef::try_from_slice(PANEL_FACE).ok())
        .as_ref()
}

/// The scale that puts the capitals at `cap_height` pixels.
fn scale_for(cap_height: f32) -> PxScale {
    PxScale::from(cap_height / cap_ratio())
}

/// Width of `text` when its capitals are `cap_height` pixels tall.
///
/// The sum of the advances, kerning included: that is the width the same string
/// occupies when drawn, so centering on it centers what is actually painted.
pub fn width(text: &str, cap_height: f32) -> f32 {
    let Some(face) = face() else {
        return 0.0;
    };
    let scaled = face.as_scaled(scale_for(cap_height));
    let mut total = 0.0;
    let mut previous = None;
    for character in text.chars() {
        let id = face.glyph_id(character);
        if let Some(previous) = previous {
            total += scaled.kern(previous, id);
        }
        total += scaled.h_advance(id);
        previous = Some(id);
    }
    total
}

/// Draw `text` with its left edge at `x` and its cap top at `top`.
pub fn draw(canvas: &mut Canvas, text: &str, x: f32, top: f32, cap_height: f32, color: Rgb) {
    let Some(face) = face() else {
        return;
    };
    let scale = scale_for(cap_height);
    let scaled = face.as_scaled(scale);
    // The pen rides the baseline, which sits one cap height under the top the
    // caller asked for.
    let baseline = top + cap_height;
    let mut pen = x;
    let mut previous = None;

    for character in text.chars() {
        let id = face.glyph_id(character);
        if let Some(previous) = previous {
            pen += scaled.kern(previous, id);
        }
        let glyph = id.with_scale_and_position(scale, point(pen, baseline));
        if let Some(outlined) = face.outline_glyph(glyph) {
            // Where this glyph's own ink starts, which is not the line's top:
            // the rasterizer reports coverage relative to the outline's bounds.
            let bounds = outlined.px_bounds();
            let (ink_left, ink_top) = (bounds.min.x as i32, bounds.min.y as i32);
            // The coverage lands straight on the canvas: no per-glyph bitmap is
            // allocated, and the edges blend exactly as the arcs do.
            outlined.draw(|dx, dy, coverage| {
                canvas.blend(ink_left + dx as i32, ink_top + dy as i32, color, coverage);
            });
        }
        pen += scaled.h_advance(id);
        previous = Some(id);
    }
}

/// Draw `text` centered on `center_x`.
pub fn draw_centered(
    canvas: &mut Canvas,
    text: &str,
    center_x: f32,
    top: f32,
    cap_height: f32,
    color: Rgb,
) {
    draw(
        canvas,
        text,
        center_x - width(text, cap_height) / 2.0,
        top,
        cap_height,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);
    const BLACK: Rgb = Rgb::BLACK;

    /// The bounding box of everything drawn, in pixels.
    fn ink(canvas: &Canvas) -> Option<(u32, u32, u32, u32)> {
        let mut extent: Option<(u32, u32, u32, u32)> = None;
        for y in 0..canvas.height() {
            for x in 0..canvas.width() {
                if canvas.pixel(x, y) != Some(BLACK) {
                    extent = Some(match extent {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        extent
    }

    #[test]
    fn the_embedded_face_parses() {
        assert!(face().is_some(), "the shipped font file is not a font");
    }

    #[test]
    fn a_capital_comes_out_the_height_it_was_asked_for() {
        // The invariant every layout in this crate rests on: sizes are cap
        // heights, and a capital drawn at one is that many pixels tall. It is
        // asserted against drawn pixels rather than against font metrics, so it
        // survives a change of face.
        for cap in [12.0, 30.0, 72.0] {
            let mut canvas = Canvas::filled(160, 160, BLACK);
            draw(&mut canvas, "H", 20.0, 40.0, cap, WHITE);
            let (_, y0, _, y1) = ink(&canvas).expect("H was drawn");
            let measured = (y1 - y0 + 1) as f32;
            assert!(
                (measured - cap).abs() <= 1.5,
                "a capital asked for at {cap} px came out {measured} px tall"
            );
            assert!((y0 as f32 - 40.0).abs() <= 1.5, "its top is {y0}, not 40");
        }
    }

    #[test]
    fn a_string_is_drawn_at_the_size_and_place_it_was_given() {
        let cap = 40.0;
        let mut canvas = Canvas::filled(160, 100, BLACK);
        draw(&mut canvas, "42", 20.0, 30.0, cap, WHITE);
        let (x0, y0, x1, y1) = ink(&canvas).expect("something was drawn");

        // Digits have no descender and reach the cap line, so the ink is the
        // box the caller named, to within a pixel of antialiasing.
        assert!((y0 as f32 - 30.0).abs() <= 1.5, "top is {y0}, not 30");
        assert!(
            ((y1 as f32) - (30.0 + cap)).abs() <= 1.5,
            "baseline is {y1}, not {}",
            30.0 + cap
        );
        assert!(x0 >= 19, "ink started left of the origin at {x0}");
        assert!(
            (x1 as f32) <= 20.0 + width("42", cap) + 1.0,
            "ink ran past the advance at {x1}"
        );
    }

    #[test]
    fn the_digits_are_tabular_so_a_reading_never_shifts_under_itself() {
        // This face carries one advance for all ten digits, which is what keeps
        // a value from moving sideways as it changes. It is a property of the
        // file, so it is asserted against the file.
        let cap = 40.0;
        let one = width("1", cap);
        for digit in '0'..='9' {
            assert!(
                (width(&digit.to_string(), cap) - one).abs() < 0.01,
                "{digit} is not the width of the other digits"
            );
        }
        assert!((width("100", cap) - 3.0 * one).abs() < 0.01);
        assert_eq!(width("", cap), 0.0);
    }

    #[test]
    fn every_character_the_panel_can_be_asked_to_draw_has_a_glyph() {
        // The product's own name, every metric caption and unit, the digits and
        // the unavailable marker. A character the subset does not carry would
        // come out as a blank box, so the set is asserted against what the
        // product can produce rather than against the subset command line.
        let face = face().expect("the face parses");
        let required: String = kori_core::PRODUCT_NAME
            .chars()
            .chain(
                kori_core::display::LcdMetric::ALL
                    .iter()
                    .flat_map(|metric| {
                        metric
                            .caption()
                            .chars()
                            .chain(metric.unit().chars())
                            .collect::<Vec<_>>()
                    }),
            )
            .chain("0123456789-. ".chars())
            .collect();

        for character in required.chars() {
            assert!(
                face.glyph_id(character).0 != 0,
                "the subset has no glyph for {character:?}"
            );
        }
    }

    #[test]
    fn centering_puts_equal_room_on_both_sides() {
        let mut canvas = Canvas::filled(201, 80, BLACK);
        draw_centered(&mut canvas, "CPU", 100.5, 20.0, 30.0, WHITE);
        let (x0, _, x1, _) = ink(&canvas).expect("something was drawn");
        let (left, right) = (x0, 201 - 1 - x1);
        assert!(
            left.abs_diff(right) <= 2,
            "left margin {left} and right margin {right} differ"
        );
    }

    #[test]
    fn a_face_that_could_not_be_parsed_draws_nothing_rather_than_panicking() {
        // The degraded path: `width` answers zero and `draw` leaves the canvas
        // alone. Exercised here through an empty string, which takes the same
        // route through the face and must not blow up either.
        let mut canvas = Canvas::filled(20, 20, BLACK);
        draw(&mut canvas, "", 0.0, 0.0, 20.0, WHITE);
        assert!(canvas.pixels().iter().all(|pixel| *pixel == BLACK));
        assert_eq!(width("", 20.0), 0.0);
    }

    #[test]
    fn drawing_outside_the_canvas_is_clipped_rather_than_fatal() {
        let mut canvas = Canvas::filled(24, 24, BLACK);
        draw(&mut canvas, "888", -60.0, -40.0, 30.0, WHITE);
        draw(&mut canvas, "888", 400.0, 400.0, 30.0, WHITE);
        assert!(canvas.pixels().iter().all(|pixel| *pixel == BLACK));
    }
}
