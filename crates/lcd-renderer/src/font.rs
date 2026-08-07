// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The panel's two typefaces, both drawn rather than loaded.
//!
//! Original work, on purpose. Embedding a font file would put a third party's
//! licensing into the shipped binary for the sake of six captions, and the
//! panel is 240 pixels across, which is small enough that a face designed for
//! it beats a face scaled down to it.
//!
//! * [`glyph`] is a five by seven grid used for captions and the wordmark at
//!   small sizes, where a pixel face reads as a deliberate instrument label.
//! * [`seven_segment`] draws the readings, where a stroked face stays legible
//!   at a glance and scales without a staircase.

use nzxt_core::lighting::Rgb;

use crate::canvas::Canvas;

/// Width and height of one cell of the bitmap face.
pub const GLYPH_WIDTH: u32 = 5;
pub const GLYPH_HEIGHT: u32 = 7;

/// Cells between the left edge of one glyph and the next.
const GLYPH_ADVANCE: u32 = GLYPH_WIDTH + 1;

/// One row of a glyph, as the low five bits with the leftmost pixel highest.
type GlyphRows = [u8; GLYPH_HEIGHT as usize];

/// The bitmap for one character, or `None` when the face has no such glyph.
///
/// Lowercase maps to uppercase: the panel has no room for two cases, and a
/// caption that silently lost its letters would be worse than one in capitals.
pub fn glyph(character: char) -> Option<GlyphRows> {
    let upper = character.to_ascii_uppercase();
    Some(match upper {
        ' ' => [0; 7],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10011, 0b10001, 0b10001, 0b01110,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00001, 0b00001, 0b00001, 0b00001, 0b10001, 0b10001, 0b01110,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        '%' => [
            0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011,
        ],
        '\u{b0}' => [
            0b01100, 0b10010, 0b10010, 0b01100, 0b00000, 0b00000, 0b00000,
        ],
        _ => return None,
    })
}

/// Width of `text` in the bitmap face at `scale`, gaps included.
///
/// The trailing gap is not counted: it belongs between two glyphs, not after
/// the last one, and counting it would push a centered string off center.
pub fn text_width(text: &str, scale: u32) -> u32 {
    let count = text.chars().count() as u32;
    if count == 0 {
        return 0;
    }
    (count * GLYPH_ADVANCE - 1) * scale
}

/// Draw `text` in the bitmap face with its left edge at `x` and top at `y`.
///
/// A character the face has no glyph for is skipped rather than substituted,
/// and it still advances, so a caption keeps its spacing.
pub fn draw_text(canvas: &mut Canvas, text: &str, x: i32, y: i32, scale: u32, color: Rgb) {
    for (index, character) in text.chars().enumerate() {
        let origin_x = x + (index as u32 * GLYPH_ADVANCE * scale) as i32;
        let Some(rows) = glyph(character) else {
            continue;
        };
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..GLYPH_WIDTH {
                let lit = bits >> (GLYPH_WIDTH - 1 - column) & 1 == 1;
                if lit {
                    canvas.fill_rect(
                        origin_x + (column * scale) as i32,
                        y + (row as u32 * scale) as i32,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
    }
}

/// Draw `text` centered horizontally on `center_x`.
pub fn draw_text_centered(
    canvas: &mut Canvas,
    text: &str,
    center_x: f32,
    y: i32,
    scale: u32,
    color: Rgb,
) {
    let x = (center_x - text_width(text, scale) as f32 / 2.0).round() as i32;
    draw_text(canvas, text, x, y, scale, color);
}

/// Which of the seven bars each character lights.
///
/// Indices are the customary `A` through `G`: top, upper right, lower right,
/// bottom, lower left, upper left, middle.
fn segments(character: char) -> Option<[bool; 7]> {
    let mask: u8 = match character {
        '0' => 0b0111111,
        '1' => 0b0000110,
        '2' => 0b1011011,
        '3' => 0b1001111,
        '4' => 0b1100110,
        '5' => 0b1101101,
        '6' => 0b1111101,
        '7' => 0b0000111,
        '8' => 0b1111111,
        '9' => 0b1101111,
        '-' => 0b1000000,
        ' ' => 0,
        _ => return None,
    };
    let mut lit = [false; 7];
    for (index, slot) in lit.iter_mut().enumerate() {
        *slot = mask >> index & 1 == 1;
    }
    Some(lit)
}

/// Width of one seven-segment digit at `height`.
///
/// The proportion is fixed so a two-digit and a three-digit reading share a
/// stroke weight and only differ in how much room they take.
pub fn digit_width(height: f32) -> f32 {
    height * 0.58
}

/// Distance between the left edges of two adjacent digits.
fn digit_advance(height: f32) -> f32 {
    digit_width(height) + height * 0.14
}

/// Width of a whole seven-segment string at `height`.
pub fn seven_segment_width(text: &str, height: f32) -> f32 {
    let count = text.chars().count();
    if count == 0 {
        return 0.0;
    }
    digit_advance(height) * (count as f32 - 1.0) + digit_width(height)
}

/// Draw `text` in the seven-segment face, centered on `center_x`.
///
/// Characters the face cannot form are skipped and still advance, so `--`
/// occupies exactly the room two digits would.
pub fn draw_seven_segment_centered(
    canvas: &mut Canvas,
    text: &str,
    center_x: f32,
    top: f32,
    height: f32,
    color: Rgb,
) {
    let mut x = center_x - seven_segment_width(text, height) / 2.0;
    let width = digit_width(height);
    // A sixteenth of the height gives a stroke that stays solid at 20 pixels
    // and does not close up the counters at 60.
    let half_stroke = height / 16.0;
    let gap = half_stroke * 0.6;

    for character in text.chars() {
        if let Some(lit) = segments(character) {
            draw_digit(canvas, lit, x, top, width, height, half_stroke, gap, color);
        }
        x += digit_advance(height);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_digit(
    canvas: &mut Canvas,
    lit: [bool; 7],
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    half_stroke: f32,
    gap: f32,
    color: Rgb,
) {
    let middle = y + height / 2.0;
    let left = x + half_stroke;
    let right = x + width - half_stroke;
    let bar_left = left + gap;
    let bar_right = right - gap;

    let mut horizontal = |center_y: f32| {
        canvas.fill_convex(
            &[
                (bar_left, center_y),
                (bar_left + half_stroke, center_y - half_stroke),
                (bar_right - half_stroke, center_y - half_stroke),
                (bar_right, center_y),
                (bar_right - half_stroke, center_y + half_stroke),
                (bar_left + half_stroke, center_y + half_stroke),
            ],
            color,
        );
    };
    if lit[0] {
        horizontal(y + half_stroke);
    }
    if lit[3] {
        horizontal(y + height - half_stroke);
    }
    if lit[6] {
        horizontal(middle);
    }

    let mut vertical = |center_x: f32, top: f32, bottom: f32| {
        canvas.fill_convex(
            &[
                (center_x, top),
                (center_x + half_stroke, top + half_stroke),
                (center_x + half_stroke, bottom - half_stroke),
                (center_x, bottom),
                (center_x - half_stroke, bottom - half_stroke),
                (center_x - half_stroke, top + half_stroke),
            ],
            color,
        );
    };
    let upper = (y + half_stroke + gap, middle - gap);
    let lower = (middle + gap, y + height - half_stroke - gap);
    if lit[1] {
        vertical(right, upper.0, upper.1);
    }
    if lit[2] {
        vertical(right, lower.0, lower.1);
    }
    if lit[4] {
        vertical(left, lower.0, lower.1);
    }
    if lit[5] {
        vertical(left, upper.0, upper.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb = Rgb::new(0xff, 0xff, 0xff);
    const BLACK: Rgb = Rgb::BLACK;

    /// How many pixels the string lit, which is what "did it draw" means here.
    fn lit_pixels(canvas: &Canvas) -> usize {
        canvas.pixels().iter().filter(|p| **p != BLACK).count()
    }

    #[test]
    fn the_face_covers_every_character_the_panel_can_be_asked_to_draw() {
        // The captions, the units, the wordmark and every reading share one
        // face. A character with no glyph would silently vanish from a label,
        // so the set the product can produce is asserted rather than assumed.
        let required: String = nzxt_core::PRODUCT_NAME
            .chars()
            .chain(
                nzxt_core::display::LcdMetric::ALL
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
                glyph(character).is_some(),
                "the bitmap face has no glyph for {character:?}"
            );
        }
    }

    #[test]
    fn an_unknown_character_is_skipped_without_losing_the_spacing() {
        let mut with = Canvas::filled(60, 12, BLACK);
        let mut without = Canvas::filled(60, 12, BLACK);
        // The tab has no glyph. The A after it must land in the same place in
        // both, so a caption never shifts because one character was missing.
        draw_text(&mut with, "A\tA", 0, 0, 1, WHITE);
        draw_text(&mut without, "A A", 0, 0, 1, WHITE);
        assert_eq!(text_width("A\tA", 1), text_width("A A", 1));
        assert_eq!(with.pixel(12, 0), without.pixel(12, 0));
        assert!(glyph('\t').is_none());
    }

    #[test]
    fn text_width_counts_the_gaps_between_glyphs_and_not_after_them() {
        assert_eq!(text_width("", 2), 0);
        assert_eq!(text_width("A", 1), GLYPH_WIDTH);
        assert_eq!(text_width("AB", 1), GLYPH_WIDTH * 2 + 1);
        assert_eq!(text_width("AB", 3), (GLYPH_WIDTH * 2 + 1) * 3);
    }

    #[test]
    fn centering_puts_equal_room_on_both_sides() {
        let mut canvas = Canvas::filled(41, 9, BLACK);
        draw_text_centered(&mut canvas, "AB", 20.5, 1, 1, WHITE);
        let columns: Vec<u32> = (0..41)
            .filter(|x| (0..9).any(|y| canvas.pixel(*x, y) != Some(BLACK)))
            .collect();
        let first = *columns.first().unwrap();
        let last = *columns.last().unwrap();
        assert_eq!(41 - 1 - last, first, "left and right margins match");
    }

    #[test]
    fn lowercase_is_drawn_as_capitals_rather_than_dropped() {
        let mut lower = Canvas::filled(40, 10, BLACK);
        let mut upper = Canvas::filled(40, 10, BLACK);
        draw_text(&mut lower, "cpu", 0, 0, 1, WHITE);
        draw_text(&mut upper, "CPU", 0, 0, 1, WHITE);
        assert_eq!(lower, upper);
        assert!(lit_pixels(&lower) > 0);
    }

    #[test]
    fn every_digit_lights_a_different_set_of_bars() {
        let mut seen = std::collections::HashSet::new();
        for digit in '0'..='9' {
            let lit = segments(digit).expect("every digit has a shape");
            assert!(seen.insert(lit), "{digit} shares its shape with another");
        }
        // Eight lights all seven, one lights the fewest. That ordering is what
        // makes the face readable at a glance.
        assert_eq!(segments('8').unwrap().iter().filter(|l| **l).count(), 7);
        assert_eq!(segments('1').unwrap().iter().filter(|l| **l).count(), 2);
        assert_eq!(segments('-').unwrap().iter().filter(|l| **l).count(), 1);
        assert_eq!(segments(' ').unwrap().iter().filter(|l| **l).count(), 0);
        assert!(segments('A').is_none());
    }

    #[test]
    fn a_reading_and_its_unavailable_marker_occupy_the_same_room() {
        // `--` stands in for a two-digit reading, so the layout must not jump
        // when a collector drops out.
        assert_eq!(
            seven_segment_width("--", 40.0),
            seven_segment_width("42", 40.0)
        );
        assert!(seven_segment_width("100", 40.0) > seven_segment_width("99", 40.0));
        assert_eq!(seven_segment_width("", 40.0), 0.0);
    }

    #[test]
    fn a_drawn_reading_stays_inside_the_box_it_was_given() {
        let height = 40.0;
        let mut canvas = Canvas::filled(120, 60, BLACK);
        draw_seven_segment_centered(&mut canvas, "88", 60.0, 10.0, height, WHITE);

        let mut min_x = u32::MAX;
        let mut max_x = 0;
        let mut min_y = u32::MAX;
        let mut max_y = 0;
        for y in 0..60 {
            for x in 0..120 {
                if canvas.pixel(x, y) != Some(BLACK) {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        let width = seven_segment_width("88", height);
        assert!(min_x as f32 >= 60.0 - width / 2.0 - 1.0, "{min_x}");
        assert!(max_x as f32 <= 60.0 + width / 2.0 + 1.0, "{max_x}");
        assert!(min_y >= 9, "{min_y} is above the top it was given");
        assert!(max_y <= 51, "{max_y} is below the height it was given");
    }
}
