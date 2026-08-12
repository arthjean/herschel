// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The finished picture, and the panel geometry it is cut to.
//!
//! Everything here is about the boundary between a drawing surface and the
//! device: how large the content is laid out, where the single quarter turn is
//! applied, and which two byte forms a frame can leave in. Nothing here draws.

use std::time::Duration;

use kori_core::capability::LcdPanel;
use kori_core::display::{DisplayError, Orientation};
use kori_core::lighting::Rgb;

use image::ImageEncoder;

use crate::canvas::Canvas;

/// A rendered panel picture, at exactly the panel's own size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Vec<Rgb>,
}

impl Framebuffer {
    /// Turn a finished canvas and take its buffer.
    ///
    /// This is the single place a quarter turn is applied. The panel is left on
    /// its own orientation zero, so the preview and the glass cannot disagree
    /// about which way up the picture is: a rotation applied in two places
    /// would cancel or double.
    ///
    /// The canvas is consumed rather than borrowed, so the common orientation
    /// hands its buffer straight over instead of copying it twice on a path the
    /// preview runs sixty times a second.
    pub(crate) fn from_canvas(canvas: Canvas, orientation: Orientation) -> Self {
        let (width, height) = (canvas.width(), canvas.height());
        let quarter = orientation.quarter_turns();
        if quarter == 0 {
            return Self {
                width,
                height,
                pixels: canvas.into_pixels(),
            };
        }

        let (turned_width, turned_height) = if quarter % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        };
        let source = canvas.pixels();
        let mut pixels = Vec::with_capacity(source.len());
        for y in 0..turned_height {
            for x in 0..turned_width {
                // A turn is a permutation of the buffer, so every target pixel
                // has exactly one source and the index is in range by the
                // construction of the bounds above.
                let (source_x, source_y) = match quarter {
                    1 => (y, height - 1 - x),
                    2 => (width - 1 - x, height - 1 - y),
                    _ => (width - 1 - y, x),
                };
                pixels.push(source[(source_y * width + source_x) as usize]);
            }
        }
        Self {
            width: turned_width,
            height: turned_height,
            pixels,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Straight RGB8, one entry per pixel.
    ///
    /// The two consumers take [`Self::to_rgb565_be`] and [`Self::to_png`]; this
    /// exists for the assertions that have to look at what was drawn.
    #[cfg(test)]
    pub(crate) fn pixels(&self) -> &[Rgb] {
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

    /// The same colors the panel will show, so the preview does not lie.
    ///
    /// A preview drawn from the full-depth pixels would show gradients the
    /// hardware cannot reproduce, so [`Self::to_png`] encodes these instead.
    fn quantized(&self) -> impl Iterator<Item = Rgb> + '_ {
        self.pixels.iter().map(|pixel| Rgb {
            r: (pixel.r >> 3) << 3,
            g: (pixel.g >> 2) << 2,
            b: (pixel.b >> 3) << 3,
        })
    }
}

/// One picture of an animation, and how long it stays on the glass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationFrame {
    pub frame: Framebuffer,
    /// Already clamped to what the transport can hold. See
    /// [`kori_core::display::MIN_FRAME_DELAY_MS`].
    pub delay: Duration,
}

/// The size the content is laid out at, before the quarter turn.
///
/// Laying out at the pre-rotation size is what makes a quarter turn on a
/// non-square panel compose rather than crop.
pub(crate) fn layout_size(
    panel: &LcdPanel,
    orientation: Orientation,
) -> Result<(u32, u32), DisplayError> {
    if panel.width == 0 || panel.height == 0 {
        return Err(DisplayError::PanelUnknown);
    }
    Ok(if orientation.quarter_turns() % 2 == 1 {
        (u32::from(panel.height), u32::from(panel.width))
    } else {
        (u32::from(panel.width), u32::from(panel.height))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{at, panel};

    /// A canvas whose every pixel is distinguishable from every other.
    fn ramp(width: u32, height: u32) -> Canvas {
        let mut canvas = Canvas::filled(width, height, Rgb::BLACK);
        for y in 0..height {
            for x in 0..width {
                let step = (y * width + x + 1) as u8;
                canvas.blend(x as i32, y as i32, Rgb::new(step, 0x40, 0x80), 1.0);
            }
        }
        canvas
    }

    fn frame_of(width: u32, height: u32, orientation: Orientation) -> Framebuffer {
        Framebuffer::from_canvas(ramp(width, height), orientation)
    }

    #[test]
    fn rgb565_packs_five_six_five_most_significant_byte_first() {
        let of = |color: Rgb| {
            Framebuffer::from_canvas(Canvas::filled(1, 1, color), Orientation::Deg0).to_rgb565_be()
        };
        // Pure red is five ones in the top bits: 0xF800.
        assert_eq!(of(Rgb::new(0xff, 0x00, 0x00)), vec![0xf8, 0x00]);
        assert_eq!(of(Rgb::new(0x00, 0xff, 0x00)), vec![0x07, 0xe0]);
        assert_eq!(of(Rgb::new(0x00, 0x00, 0xff)), vec![0x00, 0x1f]);
        // Two bytes per pixel, whatever the geometry.
        assert_eq!(frame_of(7, 3, Orientation::Deg0).to_rgb565_be().len(), 42);
    }

    #[test]
    fn the_preview_shows_the_colors_the_panel_can_actually_reproduce() {
        let white = Framebuffer::from_canvas(
            Canvas::filled(2, 2, Rgb::new(0xff, 0xff, 0xff)),
            Orientation::Deg0,
        );
        // White survives quantization exactly; a value the panel cannot hold
        // is shown as the value it will hold.
        assert!(white.quantized().all(|p| p == Rgb::new(0xf8, 0xfc, 0xf8)));
        assert_eq!(white.quantized().count(), white.pixels().len());

        // The low bits the panel has no room for are dropped, never rounded up
        // past a color it can hold.
        let odd = Framebuffer::from_canvas(
            Canvas::filled(1, 1, Rgb::new(0b1010_0111, 0b1010_0111, 0b1010_0111)),
            Orientation::Deg0,
        );
        assert_eq!(
            odd.quantized().next(),
            Some(Rgb::new(0b1010_0000, 0b1010_0100, 0b1010_0000))
        );
    }

    #[test]
    fn a_frame_encodes_to_a_png_carrying_exactly_the_colors_the_panel_gets() {
        let frame = frame_of(9, 5, Orientation::Deg0);
        let png = frame.to_png().unwrap();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (9, 5));
        for (index, (pixel, expected)) in decoded.pixels().zip(frame.quantized()).enumerate() {
            assert_eq!(
                Rgb::new(pixel[0], pixel[1], pixel[2]),
                expected,
                "pixel {index} differs between the preview and the frame"
            );
        }
    }

    #[test]
    fn a_quarter_turn_is_the_permutation_it_claims_to_be() {
        // Four wide and two high, so a turn that transposed instead of rotating
        // would still produce the right shape and the wrong picture.
        let upright = frame_of(4, 2, Orientation::Deg0);
        let turned = frame_of(4, 2, Orientation::Deg90);
        assert_eq!((turned.width(), turned.height()), (2, 4));

        // Clockwise: the upright top row becomes the right column, read down.
        for x in 0..4 {
            assert_eq!(
                at(&upright, x, 0),
                at(&turned, 1, x),
                "the top row did not land on the right column"
            );
            assert_eq!(
                at(&upright, x, 1),
                at(&turned, 0, x),
                "the bottom row did not land on the left column"
            );
        }
    }

    #[test]
    fn every_turn_keeps_exactly_the_pixels_it_was_given() {
        let upright = frame_of(4, 2, Orientation::Deg0);
        let mut sorted = upright.pixels().to_vec();
        sorted.sort_by_key(|pixel| (pixel.r, pixel.g, pixel.b));

        for orientation in Orientation::ALL {
            let turned = frame_of(4, 2, orientation);
            assert_eq!(
                turned.pixels().len(),
                upright.pixels().len(),
                "{orientation:?} changed the pixel count"
            );
            let mut kept = turned.pixels().to_vec();
            kept.sort_by_key(|pixel| (pixel.r, pixel.g, pixel.b));
            assert_eq!(kept, sorted, "{orientation:?} lost or invented a pixel");
        }
    }

    #[test]
    fn four_quarter_turns_return_the_picture_unchanged() {
        let mut orientation = Orientation::Deg0;
        for _ in 0..4 {
            orientation = orientation.rotated();
        }
        assert_eq!(
            frame_of(4, 2, orientation),
            frame_of(4, 2, Orientation::Deg0)
        );

        // And a half turn keeps the shape and reads the buffer backwards, which
        // is what separates it from the two transpositions of the same size.
        let mut backwards = frame_of(4, 2, Orientation::Deg0).pixels().to_vec();
        backwards.reverse();
        let half = frame_of(4, 2, Orientation::Deg180);
        assert_eq!((half.width(), half.height()), (4, 2));
        assert_eq!(half.pixels(), backwards);
    }

    #[test]
    fn an_odd_quarter_turn_lays_the_content_out_on_the_panels_other_axis() {
        let mut oblong = panel();
        oblong.width = 320;
        oblong.height = 240;

        assert_eq!(layout_size(&oblong, Orientation::Deg0).unwrap(), (320, 240));
        assert_eq!(
            layout_size(&oblong, Orientation::Deg180).unwrap(),
            (320, 240)
        );
        // Laid out on the swapped axes, so the turn composes rather than crops.
        assert_eq!(
            layout_size(&oblong, Orientation::Deg90).unwrap(),
            (240, 320)
        );
        assert_eq!(
            layout_size(&oblong, Orientation::Deg270).unwrap(),
            (240, 320)
        );
    }

    #[test]
    fn a_panel_with_no_geometry_has_no_layout_to_offer() {
        for (width, height) in [(0, 240), (240, 0), (0, 0)] {
            let mut unknown = panel();
            unknown.width = width;
            unknown.height = height;
            assert_eq!(
                layout_size(&unknown, Orientation::Deg0),
                Err(DisplayError::PanelUnknown)
            );
        }
    }
}
