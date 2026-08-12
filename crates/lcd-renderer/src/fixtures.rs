// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The panel this crate's own tests measure against, and the ways they walk a
//! frame.
//!
//! One geometry, named once. Every assertion that counts pixels derives its
//! bounds from [`SIDE`] rather than restating 240, so a fixture panel of
//! another size moves the assertions with it instead of leaving them measuring
//! the wrong part of the glass.

use std::path::Path;

use kori_core::capability::{LcdPanel, LcdPanelShape};
use kori_core::display::{DisplayMode, DisplayPreset, LcdMetric, MetricSample};
use kori_core::lighting::Rgb;

use crate::Framebuffer;
use crate::dial::layout;

/// The side of the fixture panel, in pixels.
pub const SIDE: u32 = 240;

/// Half of it, which is where the dial is centered.
pub const RADIUS: f32 = SIDE as f32 / 2.0;

/// The panel every layout assertion is made against.
pub fn panel() -> LcdPanel {
    LcdPanel {
        width: SIDE as u16,
        height: SIDE as u16,
        shape: LcdPanelShape::Circular,
        pixel_format: "RGB565 big-endian".into(),
        frame_bytes: SIDE * SIDE * 2,
        bulk_endpoint: 0x02,
        bulk_interface: 0,
    }
}

/// The two readings the default preset points at, at the given values.
pub fn samples(first: Option<f32>, second: Option<f32>) -> [MetricSample; 2] {
    [
        MetricSample {
            metric: LcdMetric::CpuTemperature,
            value: first,
        },
        MetricSample {
            metric: LcdMetric::GpuTemperature,
            value: second,
        },
    ]
}

/// A preset in image mode pointing at `path`.
pub fn image_preset(path: &Path) -> DisplayPreset {
    DisplayPreset {
        mode: DisplayMode::Image,
        image: Some(path.to_path_buf()),
        ..DisplayPreset::default_infographic()
    }
}

/// The color one pixel of a frame ended up.
pub fn at(frame: &Framebuffer, x: u32, y: u32) -> Rgb {
    frame.pixels()[(y * frame.width() + x) as usize]
}

/// Every pixel of a frame, with its column, its row and how far it sits from
/// the center of the dial.
///
/// The one traversal, and it takes the geometry from the frame it was handed:
/// a test that wrote its own would have to restate the panel's width to turn an
/// index into a coordinate, and would then measure somewhere else the day the
/// fixture changed size.
pub fn scan(frame: &Framebuffer) -> impl Iterator<Item = (u32, u32, f32, Rgb)> + '_ {
    let width = frame.width();
    let center = (width as f32 / 2.0, frame.height() as f32 / 2.0);
    frame
        .pixels()
        .iter()
        .enumerate()
        .map(move |(index, pixel)| {
            let (x, y) = (index as u32 % width, index as u32 / width);
            let (dx, dy) = (x as f32 - center.0, y as f32 - center.1);
            (x, y, (dx * dx + dy * dy).sqrt(), *pixel)
        })
}

/// Every pixel of the frame that lies on the gauge band, with its column.
///
/// The inner edge is derived the way the drawing derives it, from the frame's
/// smaller side, so the band this walks is the band that was painted.
pub fn in_ring(frame: &Framebuffer) -> impl Iterator<Item = (u32, Rgb)> + '_ {
    let inner = frame.width().min(frame.height()) as f32 / 2.0 * layout::TRACK_INNER;
    scan(frame)
        .filter(move |(_, _, distance, _)| *distance > inner)
        .map(|(x, _, _, pixel)| (x, pixel))
}

/// How many pixels differ from the preset's background.
pub fn drawn(frame: &Framebuffer, background: Rgb) -> usize {
    scan(frame)
        .filter(|(_, _, _, pixel)| *pixel != background)
        .count()
}
