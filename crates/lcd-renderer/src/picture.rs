// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The file an operator picked, turned into pixels on a canvas.
//!
//! One route in and one set of refusals, whether the caller wants the single
//! picture a tick draws or every picture the file plays. The size a file
//! declares is checked before anything is decoded, so a header claiming an
//! enormous canvas is refused rather than allocated for, and every failure is
//! typed and leaves the canvas as it was: a partial picture must never become a
//! frame.

use std::path::Path;
use std::time::Duration;

use kori_core::display::{
    DEFAULT_FRAME_DELAY_MS, DisplayError, DisplayPreset, MAX_ANIMATION_FRAMES, MAX_IMAGE_DIMENSION,
    MIN_FRAME_DELAY_MS,
};
use kori_core::lighting::Rgb;

use image::{AnimationDecoder, ImageDecoder};

use crate::canvas::Canvas;

/// What [`image::ImageReader::open`] hands back, named once.
type Reader = image::ImageReader<std::io::BufReader<std::fs::File>>;

/// The one picture a still preset draws, decoded to RGBA.
///
/// A GIF carrying several frames answers its first one here, which is the frame
/// the per-tick render is meant to show.
pub(crate) fn still(preset: &DisplayPreset) -> Result<image::RgbaImage, DisplayError> {
    let path = path(preset)?;
    decode_still(open(path)?, path)
}

/// Every picture the file carries, each handed to `compose` as it is decoded.
///
/// A callback rather than a vector of decoded pictures: a source frame may be
/// [`MAX_IMAGE_DIMENSION`] on a side, and holding a hundred of those at once
/// would cost orders of magnitude more than the finished framebuffers do. This
/// way the peak is one source frame, whatever the length of the animation.
///
/// One entry for a still picture or a single-frame GIF, one per frame for an
/// animated one. The caller decides what that means: the daemon installs a
/// sequence of more than one as an animation and sends a single one as an
/// ordinary frame.
pub(crate) fn frames<T>(
    preset: &DisplayPreset,
    mut compose: impl FnMut(&image::RgbaImage, Duration) -> T,
) -> Result<Vec<T>, DisplayError> {
    let path = path(preset)?;
    let reader = open(path)?;

    // Anything that is not a GIF holds still, and is decoded by the same
    // function the per-tick render calls, from the reader already open, so the
    // two cannot disagree about one picture and the file is not opened twice.
    if reader.format() != Some(image::ImageFormat::Gif) {
        let picture = decode_still(reader, path)?;
        return Ok(vec![compose(
            &picture,
            Duration::from_millis(DEFAULT_FRAME_DELAY_MS),
        )]);
    }

    let decoder = image::codecs::gif::GifDecoder::new(reader.into_inner())
        .map_err(|error| undecodable(path, error))?;
    refuse_unusable_size(&decoder, path)?;

    let mut composed = Vec::new();
    for frame in decoder.into_frames() {
        // Refused rather than truncated: an animation quietly cut off at its
        // ceiling is a file the operator picked and this product then played
        // something else instead of.
        if composed.len() == MAX_ANIMATION_FRAMES {
            return Err(DisplayError::AnimationTooLong {
                max: MAX_ANIMATION_FRAMES,
            });
        }
        let frame = frame.map_err(|error| undecodable(path, error))?;
        let delay = clamp_delay(frame.delay().into());
        // `AnimationDecoder` has already composited this frame over the
        // previous ones and applied its disposal method, so what arrives here
        // is the whole canvas rather than the sub-rectangle the format stores.
        composed.push(compose(frame.buffer(), delay));
    }

    if composed.is_empty() {
        return Err(undecodable(path, "the file carries no frames"));
    }
    Ok(composed)
}

/// The file this preset names, or the refusal that it names none.
///
/// Both entry points ask here, so "the picture this preset draws" cannot come
/// to mean two different things depending on which one was called.
/// [`DisplayPreset::validate`] has already refused an image mode with no path
/// and an empty one; what this adds is the mode itself, since a preset that
/// draws no picture has no picture to compile either.
fn path(preset: &DisplayPreset) -> Result<&Path, DisplayError> {
    preset
        .image
        .as_deref()
        .filter(|_| preset.mode.uses_image())
        .ok_or(DisplayError::ImagePathMissing)
}

/// The refusal a decode failure becomes, naming the file it was reading.
fn undecodable(path: &Path, detail: impl std::fmt::Display) -> DisplayError {
    DisplayError::ImageUndecodable {
        path: path.display().to_string(),
        detail: detail.to_string(),
    }
}

/// Open the file and let the decoder identify it from its content.
///
/// The format comes from the bytes rather than from the extension, so a PNG
/// named `.gif` is decoded as what it is.
fn open(path: &Path) -> Result<Reader, DisplayError> {
    image::ImageReader::open(path)
        .map_err(|error| undecodable(path, error))?
        .with_guessed_format()
        .map_err(|error| undecodable(path, error))
}

/// Refuse what the header declares, before a single pixel is decoded.
///
/// The decoder answers from the header it has already read, so a file claiming
/// an enormous canvas costs a refusal rather than the allocation it asked for.
fn refuse_unusable_size(decoder: &impl ImageDecoder, path: &Path) -> Result<(), DisplayError> {
    let (width, height) = decoder.dimensions();
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(DisplayError::ImageTooLarge {
            width,
            height,
            max: MAX_IMAGE_DIMENSION,
        });
    }
    if width == 0 || height == 0 {
        return Err(undecodable(path, "the image has no pixels"));
    }
    Ok(())
}

/// One picture from an open reader, in RGBA.
///
/// RGBA rather than RGB: a transparent PNG or a one-frame GIF shows the
/// preset's own background through its holes, which is what the animated path
/// does with every frame and what an operator picking a logo expects.
fn decode_still(reader: Reader, path: &Path) -> Result<image::RgbaImage, DisplayError> {
    let decoder = reader
        .into_decoder()
        .map_err(|error| undecodable(path, error))?;
    refuse_unusable_size(&decoder, path)?;
    Ok(image::DynamicImage::from_decoder(decoder)
        .map_err(|error| undecodable(path, error))?
        .to_rgba8())
}

/// What a frame's declared delay becomes once the transport has its say.
///
/// A zero means "as fast as possible" in the format and a tenth of a second
/// everywhere it is played. Anything else is raised to the floor one picture
/// costs, because a cadence the hardware cannot hold is not played faster, it
/// is played further and further behind.
fn clamp_delay(delay: Duration) -> Duration {
    if delay.is_zero() {
        Duration::from_millis(DEFAULT_FRAME_DELAY_MS)
    } else {
        delay.max(Duration::from_millis(MIN_FRAME_DELAY_MS))
    }
}

/// Paint one decoded picture across the canvas, scaled to cover it.
pub(crate) fn draw(canvas: &mut Canvas, source: &image::RgbaImage) {
    let Some(cover) = Cover::new(canvas, source.width(), source.height()) else {
        return;
    };
    for y in 0..canvas.height() {
        for x in 0..canvas.width() {
            let (source_x, source_y) = cover.source(x, y);
            let pixel = source.get_pixel(source_x, source_y);
            canvas.blend(
                x as i32,
                y as i32,
                Rgb::new(pixel[0], pixel[1], pixel[2]),
                f32::from(pixel[3]) / 255.0,
            );
        }
    }
}

/// Where a canvas pixel reads from, when the picture is scaled to cover it.
///
/// Cover rather than fit: the shorter side fills the panel and the longer one
/// is trimmed evenly, so a wide photograph is not squeezed into a circle.
struct Cover {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    source_width: u32,
    source_height: u32,
}

impl Cover {
    /// `None` when there is nothing to sample, which is the one case the
    /// arithmetic below cannot express.
    fn new(canvas: &Canvas, source_width: u32, source_height: u32) -> Option<Self> {
        if source_width == 0 || source_height == 0 {
            return None;
        }
        let scale = (canvas.width() as f32 / source_width as f32)
            .max(canvas.height() as f32 / source_height as f32);
        Some(Self {
            scale,
            offset_x: (source_width as f32 * scale - canvas.width() as f32) / 2.0,
            offset_y: (source_height as f32 * scale - canvas.height() as f32) / 2.0,
            source_width,
            source_height,
        })
    }

    fn source(&self, x: u32, y: u32) -> (u32, u32) {
        (
            (((x as f32 + 0.5 + self.offset_x) / self.scale) as u32).min(self.source_width - 1),
            (((y as f32 + 0.5 + self.offset_y) / self.scale) as u32).min(self.source_height - 1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::image_preset;
    use crate::testing;
    use kori_core::display::{DisplayMode, DisplayPreset};

    /// The delays a file declares, once the transport has had its say.
    fn delays(preset: &DisplayPreset) -> Result<Vec<Duration>, DisplayError> {
        frames(preset, |_, delay| delay)
    }

    #[test]
    fn a_preset_that_draws_no_picture_names_no_file_to_either_entry_point() {
        // The mode is the check, not the field: a dual infographic carrying a
        // leftover path is still a preset with no picture to compile.
        let mut infographic = DisplayPreset::default_infographic();
        infographic.image = Some(std::path::PathBuf::from("/tmp/leftover.png"));
        assert_eq!(
            still(&infographic).unwrap_err(),
            DisplayError::ImagePathMissing
        );
        assert_eq!(
            delays(&infographic).unwrap_err(),
            DisplayError::ImagePathMissing
        );

        // And an image mode with nothing selected fails the same way.
        let mut empty = DisplayPreset::default_infographic();
        empty.mode = DisplayMode::Image;
        assert_eq!(still(&empty).unwrap_err(), DisplayError::ImagePathMissing);
        assert_eq!(delays(&empty).unwrap_err(), DisplayError::ImagePathMissing);
    }

    #[test]
    fn a_file_that_is_not_an_image_is_rejected_without_panicking() {
        let directory = testing::scratch("not-an-image").unwrap();
        let path = directory.join("not-an-image.png");
        std::fs::write(&path, b"this is not a PNG, whatever the extension says").unwrap();

        match still(&image_preset(&path)) {
            Err(DisplayError::ImageUndecodable { path: named, .. }) => {
                assert!(named.ends_with("not-an-image.png"), "{named}");
            }
            other => panic!("expected a decode refusal, got {other:?}"),
        }

        // A file that is not there at all fails the same way, through both
        // entry points.
        let absent = image_preset(&directory.join("absent.png"));
        assert!(matches!(
            still(&absent),
            Err(DisplayError::ImageUndecodable { .. })
        ));
        assert!(matches!(
            delays(&absent),
            Err(DisplayError::ImageUndecodable { .. })
        ));
    }

    #[test]
    fn an_image_larger_than_the_ceiling_is_refused_before_it_is_decoded() {
        let directory = testing::scratch("enormous").unwrap();
        let path = directory.join("enormous.png");

        // A structurally valid PNG declaring 9000 by 9000 with no pixel data
        // behind it. Under a hundred bytes on disk, so the refusal cannot be
        // coming from the file being large; it comes from the size it claims. A
        // build that decoded first would have to allocate 243 MB to reach the
        // same verdict, which is the mistake this pins.
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

        let expected = DisplayError::ImageTooLarge {
            width: 9000,
            height: 9000,
            max: MAX_IMAGE_DIMENSION,
        };
        // Both entry points share the one guard, so both refuse it.
        assert_eq!(still(&image_preset(&path)).unwrap_err(), expected);
        assert_eq!(delays(&image_preset(&path)).unwrap_err(), expected);
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
    fn a_cadence_the_transport_cannot_hold_is_slowed_to_what_it_can() {
        // A GIF asking for a hundredth of a second is not played faster by
        // trying: one picture costs two transfer sequences, and a delay under
        // the floor would only put the animation further behind every frame.
        let directory = testing::scratch("cadence").unwrap();
        let path = directory.join("fast.gif");
        testing::write_gif(
            &path,
            8,
            &[(Rgb::new(0xff, 0, 0), 1), (Rgb::new(0, 0xff, 0), 50)],
        )
        .unwrap();

        assert_eq!(
            delays(&image_preset(&path)).unwrap(),
            vec![
                Duration::from_millis(MIN_FRAME_DELAY_MS),
                Duration::from_millis(500)
            ],
            "a frame under the floor is raised to it, one above keeps its own"
        );
    }

    #[test]
    fn a_still_picture_declares_the_default_delay_rather_than_nothing() {
        let directory = testing::scratch("still-delay").unwrap();
        let path = directory.join("one.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0x20, 0x80, 0xc0, 0xff]))
            .save(&path)
            .unwrap();

        assert_eq!(
            delays(&image_preset(&path)).unwrap(),
            vec![Duration::from_millis(DEFAULT_FRAME_DELAY_MS)]
        );
    }

    #[test]
    fn an_animation_past_the_ceiling_is_refused_rather_than_truncated() {
        // Cut off at its ceiling, the operator would have picked one file and
        // this product would be playing another. The GIF header declares no
        // frame count, so the refusal has to come from the decoder being
        // stopped rather than from a field read ahead of it.
        let directory = testing::scratch("ceiling").unwrap();
        let path = directory.join("long.gif");
        let pictures: Vec<(Rgb, u16)> = (0..MAX_ANIMATION_FRAMES + 1)
            .map(|index| (Rgb::new(index as u8, 0, 0), 10))
            .collect();
        testing::write_gif(&path, 4, &pictures).unwrap();

        assert_eq!(
            delays(&image_preset(&path)).unwrap_err(),
            DisplayError::AnimationTooLong {
                max: MAX_ANIMATION_FRAMES
            }
        );

        // One frame fewer is accepted, so the ceiling is the ceiling and not
        // an off-by-one below it.
        testing::write_gif(&path, 4, &pictures[..MAX_ANIMATION_FRAMES]).unwrap();
        assert_eq!(
            delays(&image_preset(&path)).unwrap().len(),
            MAX_ANIMATION_FRAMES
        );
    }

    #[test]
    fn a_gif_that_stops_halfway_is_refused_rather_than_played_as_far_as_it_got() {
        // A truncated animation is a file the operator picked and this product
        // would otherwise play a piece of, which is the same objection as the
        // frame ceiling: it fails, it does not shorten.
        let directory = testing::scratch("truncated").unwrap();
        let path = directory.join("cut.gif");
        testing::write_gif(
            &path,
            32,
            &[(Rgb::new(0xff, 0, 0), 10), (Rgb::new(0, 0xff, 0), 10)],
        )
        .unwrap();
        let whole = std::fs::read(&path).unwrap();
        std::fs::write(&path, &whole[..whole.len() * 2 / 3]).unwrap();

        assert!(
            matches!(
                delays(&image_preset(&path)),
                Err(DisplayError::ImageUndecodable { .. })
            ),
            "a stream that ends early is a decode failure, not a shorter animation"
        );
    }

    #[test]
    fn a_gif_header_declaring_more_than_the_ceiling_is_refused_before_a_frame_is_decoded() {
        // The same guard the still path has, on the animated one: the size
        // comes from what the file declares, so an enormous canvas is refused
        // rather than allocated for.
        let directory = testing::scratch("declared").unwrap();
        let path = directory.join("huge.gif");
        // A real GIF whose logical screen descriptor is then overwritten with
        // a canvas past the ceiling. The rest of the file is intact, so the
        // refusal can only have come from those four bytes.
        testing::write_gif(&path, 8, &[(Rgb::new(0xff, 0, 0), 10)]).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[6..10].copy_from_slice(&[0x00, 0x60, 0x00, 0x60]);
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(
            delays(&image_preset(&path)).unwrap_err(),
            DisplayError::ImageTooLarge {
                width: 24576,
                height: 24576,
                max: MAX_IMAGE_DIMENSION,
            }
        );
    }

    #[test]
    fn a_picture_covers_the_canvas_and_is_trimmed_rather_than_squeezed() {
        // A wide picture: the short side fills the canvas and the long one is
        // cut evenly, so the middle column of the source lands on the middle
        // column of the canvas and the ends fall off.
        let mut source = image::RgbaImage::from_pixel(40, 10, image::Rgba([0, 0, 0xff, 0xff]));
        for y in 0..10 {
            for x in 18..22 {
                source.put_pixel(x, y, image::Rgba([0xff, 0, 0, 0xff]));
            }
        }

        let mut canvas = Canvas::filled(10, 10, Rgb::BLACK);
        draw(&mut canvas, &source);
        assert_eq!(
            canvas.pixel(5, 5),
            Some(Rgb::new(0xff, 0, 0)),
            "the middle of the source must land on the middle of the canvas"
        );
        assert_eq!(
            canvas.pixel(0, 0),
            Some(Rgb::new(0, 0, 0xff)),
            "the corner takes the surviving part of the source, not the background"
        );
        assert!(
            canvas.pixels().iter().all(|pixel| *pixel != Rgb::BLACK),
            "cover leaves no part of the canvas unpainted"
        );
    }

    #[test]
    fn a_transparent_picture_shows_the_background_through_its_holes() {
        let mut source = image::RgbaImage::from_pixel(4, 4, image::Rgba([0xff, 0xff, 0xff, 0x00]));
        source.put_pixel(0, 0, image::Rgba([0xff, 0xff, 0xff, 0xff]));

        let background = Rgb::new(0x10, 0x20, 0x30);
        let mut canvas = Canvas::filled(4, 4, background);
        draw(&mut canvas, &source);
        assert_eq!(canvas.pixel(0, 0), Some(Rgb::new(0xff, 0xff, 0xff)));
        assert_eq!(
            canvas.pixel(3, 3),
            Some(background),
            "a fully transparent pixel must leave the preset's own background"
        );
    }
}
