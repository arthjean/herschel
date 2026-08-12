// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Centralized design tokens.
//!
//! Every color, size and font used by a component comes from here, so the
//! interface can be checked as a system: the contrast tests below run against
//! these values, not against a screenshot.
//!
//! The neutral surfaces and the status hues are Paneflow's dark UI palette
//! (`theme::model::ui_colors_with`, dark branch), so the two windows read as the
//! same desktop chrome rather than as two products that happen to both be dark.
//! The accent stays this project's violet: Paneflow signs its own chrome in
//! teal, and an operator running both should still know which window is holding
//! the pump. Nothing here reuses a vendor logo, asset or wordmark.

use gpui::{Font, FontFeatures, FontStyle, FontWeight, Hsla, Pixels, Rgba, px};
use std::sync::Arc;

/// Product name shown in the shell. Deliberately not a vendor trademark.
pub const PRODUCT_NAME: &str = "Kori";
/// The identifier the window hands to the compositor, and the one
/// `packaging/desktop/kori.desktop` declares as `StartupWMClass`. A desktop
/// finds an icon for a window by matching these two strings, so they are one
/// constant and a test, not two spellings that happen to agree today.
pub const APP_ID: &str = "kori";
/// Shown wherever the product identifies itself.
pub const UNOFFICIAL_NOTICE: &str = "Unofficial. Not affiliated with or endorsed by NZXT.";

/// One color token, kept as packed RGB so contrast can be computed on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgb(value: u32) -> Self {
        Self(value)
    }

    /// Relative luminance, per WCAG 2.1.
    pub fn luminance(self) -> f32 {
        fn channel(value: f32) -> f32 {
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        let r = channel(((self.0 >> 16) & 0xff) as f32 / 255.0);
        let g = channel(((self.0 >> 8) & 0xff) as f32 / 255.0);
        let b = channel((self.0 & 0xff) as f32 / 255.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// WCAG contrast ratio between two tokens, from 1.0 to 21.0.
    pub fn contrast(self, other: Self) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    pub fn hsla(self) -> Hsla {
        Rgba {
            r: ((self.0 >> 16) & 0xff) as f32 / 255.0,
            g: ((self.0 >> 8) & 0xff) as f32 / 255.0,
            b: (self.0 & 0xff) as f32 / 255.0,
            a: 1.0,
        }
        .into()
    }

    /// The ink to draw on top of this color.
    ///
    /// Every other pairing in this file is two tokens checked against each
    /// other once, in the tests below. This one cannot be: the color under the
    /// glyph is whatever the operator sent to a channel or set as the panel's
    /// background, so the choice has to be made at paint time. Picking the
    /// better of the two extremes of the palette never drops under 4:1, which
    /// `readable_ink_clears_the_non_text_bar_on_any_color` measures over the
    /// whole cube rather than over the colors this product happens to offer.
    pub fn readable_ink(self) -> Self {
        if self.contrast(color::TEXT_ON_ACCENT) >= self.contrast(color::RAIL) {
            color::TEXT_ON_ACCENT
        } else {
            color::RAIL
        }
    }

    /// The same color at reduced opacity, for overlays and disabled fills.
    pub fn alpha(self, alpha: f32) -> Hsla {
        let mut hsla = self.hsla();
        hsla.a = alpha.clamp(0.0, 1.0);
        hsla
    }
}

/// A color the operator sent to a channel or to the panel, as a theme token.
///
/// The conversion lives here rather than at each call site because this is the
/// type that owns the packed form: a hand-rolled shift-and-or beside every
/// contrast check is the same expression written four times, and one of the
/// four getting the channel order wrong is a defect nothing would catch.
impl From<kori_core::lighting::Rgb> for Color {
    fn from(color: kori_core::lighting::Rgb) -> Self {
        Self::rgb((u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b))
    }
}

/// The palette.
pub mod color {
    use super::Color;

    /// Fixed navigation rail, darkest surface.
    ///
    /// Paneflow's chrome background, the one its title bar carries. The rail is
    /// this window's chrome, so it sits under the work surface for the same
    /// reason: the frame is the thing the content is laid on.
    pub const RAIL: Color = Color::rgb(0x141414);
    /// Charcoal work surface. Paneflow's `base`.
    pub const SURFACE: Color = Color::rgb(0x181818);
    /// Raised panel on the work surface. Paneflow's `surface`.
    pub const PANEL: Color = Color::rgb(0x212121);
    /// Input and control fill. Paneflow's `subtle`.
    pub const CONTROL: Color = Color::rgb(0x2a2a2a);
    /// The same fill under the pointer.
    ///
    /// [`CONTROL`] darkened by 0.04 in HSL lightness rather than lightened, as
    /// Paneflow's `select_trigger` does: on a dark surface a control that sinks
    /// under the pointer reads as pressable, while one that brightens reads as
    /// already selected.
    pub const CONTROL_HOVER: Color = Color::rgb(0x202020);
    /// The same fill under the pointer, on a button.
    ///
    /// [`CONTROL`] mixed 6% toward [`TEXT`], which is Paneflow's
    /// `secondary_button`. A button lifts where a field sinks: one is a thing
    /// to press, the other a thing to open, and the direction of the change is
    /// what says which.
    pub const CONTROL_RAISED: Color = Color::rgb(0x343536);
    /// Low-contrast separator. Paneflow's `border`.
    pub const SEPARATOR: Color = Color::rgb(0x252525);
    /// Surface a floating menu is drawn on.
    ///
    /// [`PANEL`] lifted by 0.035 in HSL lightness, which is how Paneflow's
    /// `select_menu_surface` separates a menu from the panel under it: a menu
    /// hovering over a surface of its own color reads as part of it, and the
    /// lift is what says the menu is in front rather than in the page.
    ///
    /// On this palette that lands on [`CONTROL`] exactly, as it does in
    /// Paneflow. The two stay separate tokens because they are separate
    /// decisions: one is a fill a control carries, the other a layer over the
    /// page, and the day either derivation moves the other must not follow.
    pub const MENU: Color = Color::rgb(0x2a2a2a);

    /// The single selection accent.
    ///
    /// Dark enough for white label text to clear 4.5:1 on top of it, light
    /// enough to clear 3:1 against the work surface behind it.
    pub const ACCENT: Color = Color::rgb(0x6f4ef2);
    pub const ACCENT_HOVER: Color = Color::rgb(0x7a5af0);
    pub const ACCENT_ACTIVE: Color = Color::rgb(0x5f3fd8);
    /// Focus ring, light enough to read on the accent it may sit against.
    pub const FOCUS: Color = Color::rgb(0xcdbcff);

    /// Paneflow's `text`: a cool near-white rather than a neutral one, which is
    /// what keeps a page of readouts from reading as printed paper.
    pub const TEXT: Color = Color::rgb(0xd5deea);
    /// Paneflow's `muted`.
    pub const TEXT_MUTED: Color = Color::rgb(0x96a2b3);
    pub const TEXT_DISABLED: Color = Color::rgb(0x6b7280);
    /// Text drawn on top of the accent.
    pub const TEXT_ON_ACCENT: Color = Color::rgb(0xffffff);

    /// Paneflow's `vc_added`.
    pub const SUCCESS: Color = Color::rgb(0x57d992);
    /// Paneflow's `vc_modified`.
    pub const WARNING: Color = Color::rgb(0xffd166);
    /// The word a failure is named in: an error, a stalled channel, a device
    /// that did not answer. A text color, held to the full 4.5:1 on every
    /// surface it is written on, which is why it is a light red rather than a
    /// saturated one. Paneflow's `vc_deleted`, which is the same tradeoff.
    pub const DANGER: Color = Color::rgb(0xff6f6a);

    /// Fill of a destructive action, at full opacity.
    ///
    /// Apple's dark-mode `systemRed`. A tinted fill was the previous answer and
    /// it made the one irreversible control on the screen look like a quieter
    /// version of the two beside it. A destructive button is the one thing that
    /// should never be pressed by mistake, so it is the one thing that carries a
    /// solid fill.
    ///
    /// Deliberately *not* [`DANGER`]. That token is a word on a surface and is
    /// held to the body-text bar; this one is a surface a word sits on, and the
    /// two cannot be the same color without one of them failing its own job.
    pub const DESTRUCTIVE: Color = Color::rgb(0xff453a);
    /// The same fill under the pointer, and pressed.
    ///
    /// Deepening rather than lightening, which is the opposite of what
    /// [`ACCENT_HOVER`] does. A lighter red loses contrast against the white
    /// label it carries: the same lift the accent uses would take this button
    /// under 3:1, where deepening walks it up past 4:1 instead.
    pub const DESTRUCTIVE_HOVER: Color = Color::rgb(0xf03028);
    pub const DESTRUCTIVE_ACTIVE: Color = Color::rgb(0xcc2a22);
    /// Text drawn on top of a destructive fill.
    pub const TEXT_ON_DESTRUCTIVE: Color = Color::rgb(0xffffff);
}

/// Spacing scale, in logical pixels.
pub mod space {
    use gpui::{Pixels, px};

    pub const XS: Pixels = px(4.0);
    pub const SM: Pixels = px(8.0);
    pub const MD: Pixels = px(12.0);
    pub const LG: Pixels = px(16.0);
    pub const XL: Pixels = px(24.0);
}

/// Minimum size of any pointer target, in logical pixels.
pub const TARGET_MIN: Pixels = px(40.0);
/// Width of the fixed navigation rail.
pub const RAIL_WIDTH: Pixels = px(196.0);
/// Corner radius shared by controls and panels.
///
/// Two pixels under [`CARD_RADIUS`], which is what keeps a rail entry or a
/// panel reading as nested inside the card that holds it rather than as a
/// second card of the same shape.
pub const RADIUS: Pixels = px(8.0);
/// Corner radius of a device row.
///
/// One padding step outside [`RADIUS`], because that is what a row holds: a
/// line of controls of [`RADIUS`] inset by [`space::XS`]. Concentric rather
/// than equal, so the row does not read as a second control of the same shape.
pub const ROW_RADIUS: Pixels = px(12.0);
/// Corner radius of a card: the navigation rail, and the window itself.
///
/// One radius for both, as in Paneflow: a card inset from a window corner reads
/// as concentric with it only while the two curves match.
pub const CARD_RADIUS: Pixels = px(10.0);
/// Gap between the window edge and a card, and between two cards.
///
/// Paneflow's `SIDEBAR_CARD_INSET`. Narrow on purpose: the card is read as a
/// raised surface by its luminance and its radius, so the gap only has to keep
/// the two curves from touching.
pub const CARD_INSET: Pixels = px(4.0);
/// Width of the visible focus ring, in logical pixels.
pub const FOCUS_RING: Pixels = px(2.0);

/// Height of one line of the device strip.
///
/// The strip is provenance, not content: it names which hardware answered and
/// in what state, above the readouts the screen is actually about. So the line
/// is sized like a caption rather than like a row: tall enough that two of them
/// do not touch, short enough that the pair costs less than a single metric
/// tile. Deliberately far under [`TARGET_MIN`], which is a floor for things the
/// pointer aims at and nothing on this strip is.
pub const DEVICE_LINE_HEIGHT: Pixels = px(22.0);

/// Height of a control pill: a select, a color field, a slider.
///
/// One height for all three, so a row that carries two different controls has
/// them on the same baseline. Set by the tallest thing one can hold, which is a
/// [`SWATCH_SIZE`] swatch inside the padding and the reserved ring.
pub const CONTROL_HEIGHT: Pixels = px(34.0);

/// Side of a color swatch, and the radius that goes with it.
///
/// One size wherever a color is shown as a square: inside a color field and in
/// the list that field opens. A swatch is a sample of a color rather than a
/// control shaped like one, so the list matches the field it belongs to instead
/// of growing to [`TARGET_MIN`].
pub const SWATCH_SIZE: Pixels = px(22.0);
pub const SWATCH_RADIUS: Pixels = px(4.0);

/// Geometry of a floating menu, taken from Paneflow's `select_menu`.
///
/// A menu is not a card and not a control: it is its own object, so it carries
/// its own radius rather than borrowing [`RADIUS`] or [`CARD_RADIUS`] and
/// following whichever of them moves next. The width clamp is what keeps a menu
/// from being as narrow as a short option or as wide as a long one.
pub const MENU_RADIUS: Pixels = px(10.0);
pub const MENU_MIN_WIDTH: Pixels = px(200.0);
pub const MENU_MAX_WIDTH: Pixels = px(280.0);
pub const MENU_MAX_HEIGHT: Pixels = px(320.0);
/// Side of the glyph on a menu trigger, smaller than a content icon.
pub const MENU_GLYPH_SIZE: Pixels = px(12.0);
/// Gap between the control a menu belongs to and the menu itself.
///
/// A menu flush against its trigger reads as part of the control rather than as
/// a layer over it, and the two rounded edges meet in a line neither of them
/// owns.
pub const MENU_OFFSET: Pixels = px(6.0);
/// Gap between two rows of a menu: a hairline, so the highlights of two
/// neighbouring rows do not touch.
pub const MENU_ROW_GAP: Pixels = px(1.0);
/// Height of one menu row. Below [`TARGET_MIN`] on purpose, because that is the
/// row height of the menus this one is matched to.
pub const MENU_ROW_HEIGHT: Pixels = px(28.0);

/// Window size the layout is designed for.
pub const WINDOW_WIDTH: Pixels = px(920.0);
pub const WINDOW_HEIGHT: Pixels = px(640.0);

/// Client-side window decoration geometry.
///
/// The window draws its own caption bar and its own frame, so these sizes are
/// what the compositor would otherwise decide. They are the ones Paneflow uses,
/// which is what keeps the two windows reading as the same desktop chrome.
///
/// The bar itself is `1.75 * rem_size` and never shorter than this floor, so it
/// grows with the interface scale instead of clipping its own controls.
pub const TITLE_BAR_MIN_HEIGHT: Pixels = px(36.0);
/// Side of one window control button.
pub const TITLE_BAR_CONTROL: Pixels = px(20.0);
/// Gap between two window control buttons.
pub const TITLE_BAR_CONTROL_GAP: Pixels = px(6.0);
/// Inset between the window edge and the control group.
///
/// Narrower than the 8 pixels Paneflow keeps: the card underneath already holds
/// its own [`CARD_INSET`] off the window, and the two insets stack.
pub const TITLE_BAR_INSET: Pixels = px(6.0);
/// Corner radius of the window itself, dropped edge by edge when tiled.
pub const WINDOW_RADIUS: Pixels = CARD_RADIUS;
/// Border the decorated surface draws around itself.
pub const WINDOW_BORDER: Pixels = px(1.0);
/// Invisible band around the window that starts a resize.
pub const RESIZE_BORDER: Pixels = px(10.0);

/// Glyph placed between two fragments of one metadata line.
///
/// A middle dot rather than a comma or a dash: it reads as a column break at
/// the small size these lines are set in, and it does not compete with the
/// punctuation inside the fragments it separates.
pub const META_SEPARATOR: &str = "\u{00b7}";

/// Unit every temperature in the interface is written in.
///
/// One constant rather than a literal per readout: a Celsius reading spelled
/// `31.4 C` on one screen and `31.4 °C` on the next reads as two different
/// products, and the degree sign is the one part of the unit a reader looks for.
pub const DEGREE_C: &str = " \u{00b0}C";

/// Font for numeric readouts.
///
/// A fixed-advance family plus `tnum` keeps digit width constant, so a value
/// changing from `9` to `10` cannot shift the label next to it.
pub fn numeric_font() -> Font {
    Font {
        family: "monospace".into(),
        features: FontFeatures(Arc::new(vec![("tnum".into(), 1), ("lnum".into(), 1)])),
        fallbacks: None,
        weight: FontWeight::MEDIUM,
        style: FontStyle::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::color::*;
    use super::*;

    /// WCAG AA for body text.
    const TEXT_MIN: f32 = 4.5;
    /// WCAG AA for interface components and their states.
    const NON_TEXT_MIN: f32 = 3.0;

    #[test]
    fn body_text_meets_aa_on_every_surface() {
        for surface in [RAIL, SURFACE, PANEL, CONTROL, MENU] {
            let ratio = TEXT.contrast(surface);
            assert!(ratio >= TEXT_MIN, "TEXT on {surface:?} is {ratio:.2}:1");
        }
    }

    #[test]
    fn muted_text_meets_aa_on_every_surface() {
        for surface in [RAIL, SURFACE, PANEL, CONTROL, MENU] {
            let ratio = TEXT_MUTED.contrast(surface);
            assert!(
                ratio >= TEXT_MIN,
                "TEXT_MUTED on {surface:?} is {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn text_on_the_accent_meets_aa_in_every_interaction_state() {
        for state in [ACCENT, ACCENT_HOVER, ACCENT_ACTIVE] {
            let ratio = TEXT_ON_ACCENT.contrast(state);
            assert!(ratio >= TEXT_MIN, "on-accent text is {ratio:.2}:1");
        }
    }

    #[test]
    fn the_accent_itself_is_distinguishable_from_the_surface() {
        assert!(ACCENT.contrast(SURFACE) >= NON_TEXT_MIN);
    }

    /// The glyph a device row draws over an operator's color.
    ///
    /// Swept rather than sampled: the fill is a color a channel or the panel was
    /// sent, so the only honest check covers the cube. The worst pairing sits
    /// near the luminance where white and [`RAIL`] are equally far away, which
    /// the assertion below measures at just over 4:1, comfortably past the 3:1
    /// bar a non-text mark has to clear.
    #[test]
    fn readable_ink_clears_the_non_text_bar_on_any_color() {
        let mut worst = f32::INFINITY;
        for r in (0u32..=255).step_by(15) {
            for g in (0u32..=255).step_by(15) {
                for b in (0u32..=255).step_by(15) {
                    let fill = Color::rgb((r << 16) | (g << 8) | b);
                    let ratio = fill.readable_ink().contrast(fill);
                    assert!(ratio >= NON_TEXT_MIN, "ink on {fill:?} is {ratio:.2}:1");
                    worst = worst.min(ratio);
                }
            }
        }
        assert!(worst >= 4.0, "worst pairing is {worst:.2}:1");
    }

    /// The destructive fill is the one place this interface knowingly sits under
    /// the body-text bar.
    ///
    /// A saturated red carrying white cannot clear 4.5:1: the luminance a red
    /// that bright has puts the ceiling near 3.5:1, which is why Apple ships
    /// `systemRed` with white on its own destructive buttons at the same figure.
    /// The alternative is a deep maroon that no longer reads as a warning, and a
    /// destructive control that does not read as one is the worse failure.
    ///
    /// So the bar here is the 3:1 one, checked in every interaction state, and
    /// the label is never the only thing carrying the meaning: the button says
    /// "Delete profile" in words and asks a second time before it acts.
    ///
    /// Measured, so the tradeoff is auditable rather than asserted:
    ///
    /// | fill | white on it | on SURFACE | on PANEL |
    /// |---|---|---|---|
    /// | `DESTRUCTIVE` | 3.41:1 | 5.21:1 | 4.72:1 |
    /// | `DESTRUCTIVE_HOVER` | 4.07:1 | 4.36:1 | 3.95:1 |
    /// | `DESTRUCTIVE_ACTIVE` | 5.35:1 | 3.32:1 | 3.01:1 |
    #[test]
    fn the_destructive_button_stays_legible_in_every_interaction_state() {
        for state in [DESTRUCTIVE, DESTRUCTIVE_HOVER, DESTRUCTIVE_ACTIVE] {
            let ratio = TEXT_ON_DESTRUCTIVE.contrast(state);
            assert!(ratio >= NON_TEXT_MIN, "on-destructive text is {ratio:.2}:1");
        }

        // Against the surfaces it is laid on, so the button is a shape before
        // it is a label. Resting and hovered only: the pressed fill exists for
        // as long as a button is held, with the pointer on top of it, and
        // holding a momentary state to the bar that says "this component can be
        // found on the page" would price the press feedback out of existing.
        for state in [DESTRUCTIVE, DESTRUCTIVE_HOVER] {
            for surface in [SURFACE, PANEL] {
                let ratio = state.contrast(surface);
                assert!(
                    ratio >= NON_TEXT_MIN,
                    "{state:?} on {surface:?} is {ratio:.2}:1"
                );
            }
        }

        // Pressing deepens the fill rather than lifting it, which is the
        // opposite of what the accent does and the reason the label survives:
        // a lifted red would take its own white under 3:1.
        assert!(DESTRUCTIVE_HOVER.luminance() < DESTRUCTIVE.luminance());
        assert!(DESTRUCTIVE_ACTIVE.luminance() < DESTRUCTIVE_HOVER.luminance());
        assert!(
            TEXT_ON_DESTRUCTIVE.contrast(DESTRUCTIVE_ACTIVE)
                > TEXT_ON_DESTRUCTIVE.contrast(DESTRUCTIVE)
        );

        // The fill and the word a failure is named in are separate tokens on
        // purpose. Collapsing them would put a body-text red behind white, or
        // a saturated red on a panel as text, and each fails its own bar.
        assert_ne!(DESTRUCTIVE, DANGER);
        assert!(DANGER.contrast(PANEL) >= TEXT_MIN);
    }

    #[test]
    fn status_colors_meet_aa_on_the_panel() {
        for status in [SUCCESS, WARNING, DANGER] {
            let ratio = status.contrast(PANEL);
            assert!(ratio >= TEXT_MIN, "{status:?} on PANEL is {ratio:.2}:1");
        }
    }

    #[test]
    fn the_focus_ring_is_visible_against_every_background_it_sits_on() {
        for surface in [RAIL, SURFACE, PANEL, CONTROL, MENU, ACCENT] {
            let ratio = FOCUS.contrast(surface);
            assert!(
                ratio >= NON_TEXT_MIN,
                "FOCUS on {surface:?} is {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn disabled_text_is_visibly_dimmer_but_still_perceivable() {
        assert!(TEXT_DISABLED.contrast(CONTROL) < TEXT.contrast(CONTROL));
        assert!(TEXT_DISABLED.contrast(CONTROL) >= 2.0);
    }

    #[test]
    fn how_much_of_a_track_is_filled_is_visible_without_reading_the_value() {
        // The one thing a slider says at a glance is where the fill stops, so
        // the boundary between the filled part and the empty one is a
        // meaningful non-text element and takes the full 3:1. The handle rides
        // on the filled side and is held to the same bar against it.
        let ratio = ACCENT.contrast(RAIL);
        assert!(ratio >= NON_TEXT_MIN, "fill against track is {ratio:.2}:1");
        let handle = TEXT.contrast(ACCENT);
        assert!(
            handle >= NON_TEXT_MIN,
            "handle against fill is {handle:.2}:1"
        );
    }

    #[test]
    fn a_menu_reads_as_lifted_off_the_panel_it_covers() {
        // The lift is small on purpose: enough to see the edge of the menu
        // where it overlaps the panel, not so much that the menu reads as a
        // separate window. It only has to be lighter, and by less than a
        // separator is.
        assert!(
            MENU.luminance() > PANEL.luminance(),
            "the menu is not lifted"
        );
        let ratio = MENU.contrast(PANEL);
        assert!(
            (1.05..1.4).contains(&ratio),
            "menu over panel is {ratio:.3}:1"
        );
    }

    #[test]
    fn separators_stay_low_contrast_without_disappearing() {
        let ratio = SEPARATOR.contrast(SURFACE);
        assert!((1.1..2.5).contains(&ratio), "separator is {ratio:.2}:1");
    }

    #[test]
    fn contrast_is_symmetric_and_bounded() {
        assert!((TEXT.contrast(SURFACE) - SURFACE.contrast(TEXT)).abs() < 0.001);
        let white = Color::rgb(0xffffff);
        let black = Color::rgb(0x000000);
        assert!((white.contrast(black) - 21.0).abs() < 0.01);
        assert!((white.contrast(white) - 1.0).abs() < 0.001);
    }

    #[test]
    fn pointer_targets_are_at_least_forty_logical_pixels() {
        assert!(TARGET_MIN >= px(40.0));
    }

    #[test]
    fn a_core_color_packs_into_the_themes_own_form() {
        use kori_core::lighting::Rgb;
        assert_eq!(
            Color::from(Rgb::new(0x6f, 0x4e, 0xf2)),
            Color::rgb(0x6f4ef2)
        );
        assert_eq!(Color::from(Rgb::BLACK), Color::rgb(0x000000));
        assert_eq!(
            Color::from(Rgb::new(0xff, 0xff, 0xff)),
            Color::rgb(0xffffff)
        );
        // The channel order is the whole risk: a red-only color must not come
        // back as a blue one.
        assert_eq!(
            Color::from(Rgb::new(0xff, 0x00, 0x00)),
            Color::rgb(0xff0000)
        );
    }
}
