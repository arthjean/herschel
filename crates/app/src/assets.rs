// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The icons the interface draws, compiled into the binary.
//!
//! GPUI reaches an SVG through one interface only, [`gpui::AssetSource`], and
//! the path it resolves is a plain string looked up while painting. A wrong
//! path is therefore a silently missing icon, not a compile error: `Svg::paint`
//! logs the miss and draws nothing. Two things push that failure back to build
//! time. The bytes are embedded with `include_bytes!`, so a moved file stops the
//! build, and the only way to name an icon is [`Icon`], so a caller cannot spell
//! a path at all.
//!
//! Embedding also keeps the binary self-contained, which the packaged install
//! needs: the upstream GPUI example reads assets from `CARGO_MANIFEST_DIR` at
//! runtime, which is a developer's checkout and not a deployed program.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// An icon this interface can draw.
///
/// The enum is the whole vocabulary: [`Assets`] resolves nothing else, so an
/// icon exists here or it cannot be requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ChevronDown,
    ChevronRight,
    /// Monitoring, in the rail.
    ChartLine,
    /// Cooling, in the rail.
    Snowflake,
    /// Lighting, in the rail.
    Bulb,
    /// Settings, in the rail.
    Settings,
    /// A lighting channel the controller answered an accessory for, at the head
    /// of its row.
    Windmill,
    /// A lighting channel that answered nothing, in the same place.
    CircleDashed,
    /// The panel, at the head of the LCD row.
    Photo,
    /// The dim end of a brightness slider.
    SunLow,
    /// The bright end of one.
    SunHigh,
    /// A device that is present and writable.
    CircleCheck,
    /// A device that is present and read-only.
    Lock,
    /// A device that is absent, or whose ownership was refused.
    AlertCircle,
    /// Minimize, in the title bar.
    WindowMinimize,
    /// Maximize, in the title bar.
    WindowMaximize,
    /// Restore a maximized window, in the title bar.
    WindowRestore,
    /// Close, in the title bar.
    WindowClose,
}

impl Icon {
    pub const ALL: [Icon; 18] = [
        Self::ChevronDown,
        Self::ChevronRight,
        Self::ChartLine,
        Self::Snowflake,
        Self::Bulb,
        Self::Settings,
        Self::Windmill,
        Self::CircleDashed,
        Self::Photo,
        Self::SunLow,
        Self::SunHigh,
        Self::CircleCheck,
        Self::Lock,
        Self::AlertCircle,
        Self::WindowMinimize,
        Self::WindowMaximize,
        Self::WindowRestore,
        Self::WindowClose,
    ];

    /// The asset path GPUI resolves for this icon.
    pub fn path(self) -> &'static str {
        match self {
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::ChartLine => "icons/chart-line.svg",
            Self::Snowflake => "icons/snowflake.svg",
            Self::Bulb => "icons/bulb.svg",
            Self::Settings => "icons/settings.svg",
            Self::Windmill => "icons/windmill.svg",
            Self::CircleDashed => "icons/circle-dashed.svg",
            Self::Photo => "icons/photo.svg",
            Self::SunLow => "icons/sun-low.svg",
            Self::SunHigh => "icons/sun-high.svg",
            Self::CircleCheck => "icons/circle-check.svg",
            Self::Lock => "icons/lock.svg",
            Self::AlertCircle => "icons/alert-circle.svg",
            Self::WindowMinimize => "icons/window/minimize.svg",
            Self::WindowMaximize => "icons/window/maximize.svg",
            Self::WindowRestore => "icons/window/restore.svg",
            Self::WindowClose => "icons/window/close.svg",
        }
    }
}

/// Every asset the interface can name, with the bytes it resolves to.
///
/// Tabler Icons, MIT, recorded in `REUSE.toml` and kept byte-identical to
/// upstream so the annotation there stays checkable against the source. The
/// four glyphs under `icons/window/` are this project's own, drawn on the same
/// 16-pixel grid: no icon set ships the exact minimize, maximize, restore and
/// close marks a desktop caption bar expects.
const ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/chevron-down.svg",
        include_bytes!("../assets/icons/chevron-down.svg"),
    ),
    (
        "icons/chevron-right.svg",
        include_bytes!("../assets/icons/chevron-right.svg"),
    ),
    (
        "icons/chart-line.svg",
        include_bytes!("../assets/icons/chart-line.svg"),
    ),
    (
        "icons/snowflake.svg",
        include_bytes!("../assets/icons/snowflake.svg"),
    ),
    ("icons/bulb.svg", include_bytes!("../assets/icons/bulb.svg")),
    (
        "icons/settings.svg",
        include_bytes!("../assets/icons/settings.svg"),
    ),
    (
        "icons/windmill.svg",
        include_bytes!("../assets/icons/windmill.svg"),
    ),
    (
        "icons/circle-dashed.svg",
        include_bytes!("../assets/icons/circle-dashed.svg"),
    ),
    (
        "icons/photo.svg",
        include_bytes!("../assets/icons/photo.svg"),
    ),
    (
        "icons/sun-low.svg",
        include_bytes!("../assets/icons/sun-low.svg"),
    ),
    (
        "icons/sun-high.svg",
        include_bytes!("../assets/icons/sun-high.svg"),
    ),
    (
        "icons/circle-check.svg",
        include_bytes!("../assets/icons/circle-check.svg"),
    ),
    ("icons/lock.svg", include_bytes!("../assets/icons/lock.svg")),
    (
        "icons/alert-circle.svg",
        include_bytes!("../assets/icons/alert-circle.svg"),
    ),
    (
        "icons/window/minimize.svg",
        include_bytes!("../assets/icons/window/minimize.svg"),
    ),
    (
        "icons/window/maximize.svg",
        include_bytes!("../assets/icons/window/maximize.svg"),
    ),
    (
        "icons/window/restore.svg",
        include_bytes!("../assets/icons/window/restore.svg"),
    ),
    (
        "icons/window/close.svg",
        include_bytes!("../assets/icons/window/close.svg"),
    ),
];

/// The application's compiled-in asset source.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let prefix = path.trim_end_matches('/');
        Ok(ASSETS
            .iter()
            .filter_map(|(name, _)| name.strip_prefix(prefix))
            .map(|name| SharedString::from(name.trim_start_matches('/').to_string()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_resolves_to_an_svg_that_is_compiled_in() {
        // The path is resolved while painting, so a name with no bytes behind
        // it is an icon that quietly fails to draw. Asserting it here is what
        // keeps that from reaching the screen.
        for icon in Icon::ALL {
            let bytes = Assets
                .load(icon.path())
                .expect("loading a compiled-in asset cannot fail")
                .unwrap_or_else(|| panic!("{} resolves to nothing", icon.path()));
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains("<svg"), "{} is not an SVG", icon.path());
            // Tabler strokes its icons and leaves the fill empty. GPUI renders
            // the file to an alpha mask and tints it with the element's text
            // color, so a file that painted its own fill would ignore the
            // theme; `currentColor` is what keeps the theme in charge.
            assert!(
                text.contains("stroke=\"currentColor\""),
                "{} does not follow the current color",
                icon.path()
            );
        }
    }

    #[test]
    fn an_unknown_path_resolves_to_nothing_rather_than_an_error() {
        assert!(
            Assets
                .load("icons/does-not-exist.svg")
                .expect("a miss is not a failure")
                .is_none()
        );
        assert_eq!(
            Assets.list("icons").expect("listing cannot fail").len(),
            Icon::ALL.len()
        );
    }
}
