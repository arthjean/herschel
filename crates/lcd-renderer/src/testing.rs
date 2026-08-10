// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fixtures for the tests that need a real picture on disk.
//!
//! The decode path takes a file, not a buffer, because that is what an operator
//! picks. So a test of that path needs a file, and a test of the animated path
//! needs one carrying more than one frame. Writing the GIF here rather than
//! committing one keeps the frame count, the colors and the delays visible in
//! the test that asserts on them, and lets a test ask for a hundred frames
//! without a hundred frames in the repository.

use std::io;
use std::path::Path;

use kori_core::lighting::Rgb;

/// Write an animated GIF of solid frames, one per entry.
///
/// `frames` pairs a color with the delay it declares, in hundredths of a
/// second, which is the unit the format stores. A single entry produces a GIF
/// that holds still, which is the case the compile step has to tell apart from
/// an animation.
pub fn write_gif(path: &Path, side: u32, frames: &[(Rgb, u16)]) -> io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = image::codecs::gif::GifEncoder::new(std::io::BufWriter::new(file));
    encoder
        .set_repeat(image::codecs::gif::Repeat::Infinite)
        .map_err(io::Error::other)?;

    for (color, delay) in frames {
        let mut buffer = image::RgbaImage::new(side, side);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([color.r, color.g, color.b, 0xff]);
        }
        encoder
            .encode_frame(image::Frame::from_parts(
                buffer,
                0,
                0,
                // The format stores hundredths of a second; the constructor
                // takes a ratio of milliseconds, so the tens are explicit here
                // rather than hidden in a cast.
                image::Delay::from_numer_denom_ms(u32::from(*delay) * 10, 1),
            ))
            .map_err(io::Error::other)?;
    }
    Ok(())
}

/// A directory of this process's own, under the system temporary root.
///
/// Named after the process so two test binaries running at once cannot pick up
/// each other's fixtures.
pub fn scratch(name: &str) -> io::Result<std::path::PathBuf> {
    let directory = std::env::temp_dir().join(format!("kori-lcd-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}
