// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The controls an operator drives: a button, a list, a track, a color.
//!
//! All four are cut from the one pill [`super::pill`] defines, so a row that
//! carries two of them has them on the same baseline. The slider paints its own
//! track here rather than through a shared painter: the rail, the fill and the
//! handle are the only things that read those three constants, and a geometry
//! shared with nothing is a geometry another widget can be made to depend on.

use std::rc::Rc;

use gpui::{
    Bounds, Div, Hsla, Pixels, Point, SharedString, Stateful, Window, canvas, div, prelude::*, px,
};

use crate::assets::Icon;
use crate::theme::{
    CONTROL_HEIGHT, Color, FOCUS_RING, MENU_GLYPH_SIZE, RADIUS, SWATCH_RADIUS, SWATCH_SIZE, color,
    numeric_font, space,
};

use super::{
    ButtonVariant, ControlState, control_pill, field, focus_visible, icon, select_chevron,
    slider_surface,
};

/// A labeled action.
pub struct Button {
    key: SharedString,
    label: SharedString,
    variant: ButtonVariant,
    state: ControlState,
    tab_index: isize,
}

impl Button {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            variant: ButtonVariant::Secondary,
            state: ControlState::Enabled,
            tab_index: 0,
        }
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    fn fill(&self) -> Hsla {
        if !self.state.is_enabled() {
            return color::CONTROL.alpha(0.4);
        }
        match self.variant {
            ButtonVariant::Primary => color::ACCENT.hsla(),
            ButtonVariant::Secondary => color::CONTROL.hsla(),
            ButtonVariant::Danger => color::DESTRUCTIVE.hsla(),
        }
    }

    fn label_color(&self) -> Hsla {
        if !self.state.is_enabled() {
            return color::TEXT_DISABLED.hsla();
        }
        match self.variant {
            ButtonVariant::Primary => color::TEXT_ON_ACCENT.hsla(),
            ButtonVariant::Secondary => color::TEXT.hsla(),
            ButtonVariant::Danger => color::TEXT_ON_DESTRUCTIVE.hsla(),
        }
    }

    /// The fill under the pointer.
    ///
    /// A button lifts where a field sinks, so the two share a resting fill and
    /// part ways on hover. The accent has a lift of its own; the secondary takes
    /// the raised control fill.
    ///
    /// The destructive button is the exception that deepens instead. It carries
    /// white at full opacity, and lifting a red that already sits near the
    /// contrast floor would take its own label under it.
    fn hover_fill(&self) -> Hsla {
        match self.variant {
            ButtonVariant::Primary => color::ACCENT_HOVER.hsla(),
            ButtonVariant::Secondary => color::CONTROL_RAISED.hsla(),
            ButtonVariant::Danger => color::DESTRUCTIVE_HOVER.hsla(),
        }
    }

    pub fn render(self) -> Stateful<Div> {
        let fill = self.fill();
        let label_color = self.label_color();
        let hover_fill = self.hover_fill();
        let enabled = self.state.is_enabled();
        let pressed = match self.variant {
            ButtonVariant::Primary => color::ACCENT_ACTIVE.hsla(),
            ButtonVariant::Secondary => color::CONTROL_HOVER.hsla(),
            ButtonVariant::Danger => color::DESTRUCTIVE_ACTIVE.hsla(),
        };

        // Paneflow's `secondary_button` geometry, on the pill every control on
        // the screen is cut from: no outline, the same corner, the same 12px
        // medium label. Wider than a field is padded, because a button is read
        // by its label and a label needs air on both sides of it.
        let base = div()
            .id(self.key.clone())
            .flex()
            .items_center()
            .justify_center()
            .min_h(CONTROL_HEIGHT)
            .px(space::MD)
            .py(space::XS)
            .border(FOCUS_RING)
            .border_color(color::CONTROL.alpha(0.0))
            .rounded(RADIUS)
            .bg(fill)
            .text_xs()
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(label_color)
            .child(self.label);

        if enabled {
            base.tab_index(self.tab_index)
                .tab_stop(true)
                .cursor_pointer()
                .hover(|this| this.bg(hover_fill))
                .active(|this| this.bg(pressed))
                .when(focus_visible(), |this| {
                    this.focus(|this| this.border_color(color::FOCUS.hsla()))
                })
        } else {
            // A disabled control is not a tab stop: keyboard traversal must not
            // stop on something that cannot be operated.
            base.cursor_default().opacity(0.6)
        }
    }
}

/// One option of a [`Select`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

impl SelectOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
        }
    }
}

/// A single-choice control.
pub struct Select {
    key: SharedString,
    label: SharedString,
    show_label: bool,
    options: Vec<SelectOption>,
    selected: Option<String>,
    state: ControlState,
    tab_index: isize,
}

impl Select {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            show_label: true,
            options: Vec::new(),
            selected: None,
            state: ControlState::Enabled,
            tab_index: 0,
        }
    }

    pub fn options(mut self, options: Vec<SelectOption>) -> Self {
        self.options = options;
        self
    }

    pub fn selected(mut self, value: impl Into<String>) -> Self {
        self.selected = Some(value.into());
        self
    }

    pub fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    /// Drop the caption above the control.
    ///
    /// For a control on a device row, where the row already names what is being
    /// configured. The label is still carried, because an error message names
    /// the control it came from.
    pub fn label_hidden(mut self) -> Self {
        self.show_label = false;
        self
    }

    /// Label of the selected option, or a placeholder when nothing matches.
    pub fn display_value(&self) -> String {
        self.selected
            .as_ref()
            .and_then(|value| {
                self.options
                    .iter()
                    .find(|option| &option.value == value)
                    .map(|option| option.label.clone())
            })
            .unwrap_or_else(|| "Not available".to_string())
    }

    pub fn render(self) -> Stateful<Div> {
        let value = self.display_value();
        let label = self.show_label.then(|| self.label.clone());
        let message = self.state.message().map(str::to_string);
        let state = self.state.clone();
        let field_id = SharedString::from(format!("{}-field", self.key));

        field(field_id, label, message, state.clone(), {
            control_pill(self.key.clone(), &state, self.tab_index)
                .w_full()
                .child(div().min_w_0().truncate().child(value))
                .child(select_chevron())
        })
    }
}

/// Height of a slider's track.
const TRACK_HEIGHT: Pixels = px(4.0);
/// Size of the handle that rides the track.
const HANDLE_WIDTH: Pixels = px(14.0);
const HANDLE_HEIGHT: Pixels = px(22.0);

/// A bounded numeric control the pointer can drag.
///
/// The track publishes its own painted rectangle through `bounds_sink`, and the
/// caller converts a pointer position into a value with [`Slider::value_at`].
/// That indirection is what makes the control real rather than decorative: GPUI
/// hands a listener a window position, and only the element that painted the
/// track knows where the track ended up.
///
/// The two icons are part of the control, not ornament. They say which end is
/// which for a value that is otherwise a bare bar, and they mark the ends the
/// pointer can snap to.
pub struct Slider {
    key: SharedString,
    label: SharedString,
    show_label: bool,
    value: f32,
    min: f32,
    max: f32,
    unit: SharedString,
    state: ControlState,
    tab_index: isize,
    icons: Option<(Icon, Icon)>,
    bounds_sink: Option<Rc<dyn Fn(Bounds<Pixels>)>>,
}

impl Slider {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>, value: f32) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            show_label: true,
            value,
            min: 0.0,
            max: 100.0,
            unit: SharedString::default(),
            state: ControlState::Enabled,
            tab_index: 0,
            icons: None,
            bounds_sink: None,
        }
    }

    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    pub fn unit(mut self, unit: impl Into<SharedString>) -> Self {
        self.unit = unit.into();
        self
    }

    pub fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    /// Drop the caption above the track, and with it the readout it carried.
    ///
    /// For a slider on a device row: the row names the device, and the position
    /// of the handle is the value. See [`Select::label_hidden`].
    pub fn label_hidden(mut self) -> Self {
        self.show_label = false;
        self
    }

    /// The icons that flank the track, from the low end to the high one.
    pub fn icons(mut self, low: Icon, high: Icon) -> Self {
        self.icons = Some((low, high));
        self
    }

    /// Where to publish the track's painted rectangle, for hit testing.
    ///
    /// A callback rather than a cell: a screen with one slider per device row
    /// has to tell them apart, and only the caller knows which row this one is.
    pub fn bounds_sink(mut self, sink: Rc<dyn Fn(Bounds<Pixels>)>) -> Self {
        self.bounds_sink = Some(sink);
        self
    }

    /// Filled fraction, always inside 0.0 to 1.0.
    pub fn fraction(&self) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    pub fn render(self) -> Stateful<Div> {
        let fraction = self.fraction();
        let enabled = self.state.is_enabled();
        let fill = if enabled {
            color::ACCENT.hsla()
        } else {
            color::TEXT_DISABLED.alpha(0.6)
        };
        let handle = if enabled {
            color::TEXT.hsla()
        } else {
            color::TEXT_DISABLED.hsla()
        };
        let icon_color = if enabled {
            color::TEXT_MUTED.hsla()
        } else {
            color::TEXT_DISABLED.hsla()
        };
        let state = self.state.clone();
        let message = state.message().map(str::to_string);
        // The value rides on the label rather than beside the track. A readout
        // in the row costs the track a third of its width, and a track too
        // short to aim at is what pushed the last arrangement back to buttons.
        // A slider with its caption dropped shows no number at all: the handle
        // is the value, which is what the reference does.
        let label = self
            .show_label
            .then(|| SharedString::from(format!("{} {:.0}{}", self.label, self.value, self.unit)));
        let field_id = SharedString::from(format!("{}-field", self.key));
        let sink = self.bounds_sink.clone();
        let icons = self.icons;

        // The whole track is one canvas: it paints the rail, the fill and the
        // handle, and publishes the rectangle it was painted at, so painting
        // and hit testing read the same numbers by construction. The first
        // attempt measured with an absolutely positioned canvas beside an
        // overlay of styled divs, and that canvas was laid out at zero width:
        // every press converted against an empty rectangle and was dropped.
        let track = div().flex_1().min_w_0().h(HANDLE_HEIGHT).child(
            canvas(
                move |bounds: Bounds<Pixels>, _, _| {
                    if let Some(sink) = &sink {
                        sink(bounds);
                    }
                },
                move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                    paint_track(window, bounds, fraction, fill, handle);
                },
            )
            .size_full(),
        );

        field(field_id, label, message, state.clone(), {
            // No fill of its own: the track and its two glyphs sit straight on
            // the row, so the line reads as one card rather than as a pill
            // parked inside it. The pill's height and the reserved focus ring
            // stay, which is what keeps this control on the same baseline as
            // the select beside it.
            slider_surface(self.key.clone(), &state, self.tab_index)
                .w_full()
                .children(icons.map(|(low, _)| icon(low, MENU_GLYPH_SIZE, icon_color).flex_none()))
                .child(track)
                .children(
                    icons.map(|(_, high)| icon(high, MENU_GLYPH_SIZE, icon_color).flex_none()),
                )
        })
    }

    /// The value a pointer position selects on a track painted at `bounds`.
    ///
    /// Clamped to the range at both ends, so a drag that leaves the track keeps
    /// moving the value to whichever end it left by rather than stopping where
    /// the pointer crossed the edge.
    pub fn value_at(bounds: Bounds<Pixels>, position: Point<Pixels>, min: f32, max: f32) -> f32 {
        let width = f32::from(bounds.size.width).max(1.0);
        let across = ((f32::from(position.x) - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0);
        min + (max - min) * across
    }
}

/// The rail, the filled part and the handle of a slider.
///
/// `bounds` is the rectangle the canvas was given, which is also the rectangle
/// [`Slider::value_at`] converts against. The handle is centered on the value
/// and held inside the rail at both ends, so it never hangs off the track it
/// belongs to while still marking the end it reached.
fn paint_track(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    fraction: f32,
    fill: Hsla,
    handle: Hsla,
) {
    let radius = TRACK_HEIGHT / 2.0;
    let rail = Bounds {
        origin: Point {
            x: bounds.origin.x,
            y: bounds.origin.y + (bounds.size.height - TRACK_HEIGHT) / 2.0,
        },
        size: gpui::size(bounds.size.width, TRACK_HEIGHT),
    };
    // A channel cut into the pill rather than a line drawn on it. What has to
    // be legible is the boundary between the filled part and the empty one,
    // and the darkest surface puts that boundary at 3.5:1 against the accent
    // where a separator left it at 2.2:1.
    window.paint_quad(gpui::fill(rail, color::RAIL.hsla()).corner_radii(radius));

    let filled = Bounds {
        origin: rail.origin,
        size: gpui::size(bounds.size.width * fraction, TRACK_HEIGHT),
    };
    window.paint_quad(gpui::fill(filled, fill).corner_radii(radius));

    let left = (bounds.size.width * fraction - HANDLE_WIDTH / 2.0)
        .clamp(px(0.0), (bounds.size.width - HANDLE_WIDTH).max(px(0.0)));
    let knob = Bounds {
        origin: Point {
            x: bounds.origin.x + left,
            y: bounds.origin.y + (bounds.size.height - HANDLE_HEIGHT) / 2.0,
        },
        size: gpui::size(HANDLE_WIDTH, HANDLE_HEIGHT),
    };
    // Concentric with the pill around it: the handle sits one padding step
    // inside a corner of [`RADIUS`], so its own corner is that much tighter.
    // Matched radii is what keeps a nested shape from reading as a sticker.
    let handle_radius = RADIUS - space::XS;
    window.paint_quad(
        gpui::quad(
            knob,
            handle_radius,
            handle,
            px(1.0),
            color::RAIL.hsla(),
            gpui::BorderStyle::Solid,
        )
        .corner_radii(handle_radius),
    );
}

/// A six-digit hexadecimal color input with a live swatch.
pub struct ColorField {
    key: SharedString,
    label: SharedString,
    value: String,
    state: ControlState,
    tab_index: isize,
}

impl ColorField {
    pub fn new(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            value: value.into(),
            state: ControlState::Enabled,
            tab_index: 0,
        }
    }

    /// Whether the hardware behind this field can be written at all.
    ///
    /// Separate from the digits being valid. A field the operator can finish
    /// typing and a field whose device refused the capability are two different
    /// refusals, and the second one outranks the first: correcting the color
    /// would not make the write happen.
    pub fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    pub fn render(self) -> Stateful<Div> {
        let parsed = parse_hex_color(&self.value);
        let state = match (&self.state, &parsed) {
            (ControlState::Disabled { .. }, _) => self.state.clone(),
            (_, Ok(_)) => ControlState::Enabled,
            (_, Err(error)) => ControlState::error(error.clone()),
        };
        let swatch = parsed.unwrap_or(Color::rgb(0x000000));
        let message = state.message().map(str::to_string);
        let field_id = SharedString::from(format!("{}-field", self.key));

        field(
            field_id,
            Some(self.label.clone()),
            message,
            state.clone(),
            {
                // The swatch and its digits read as one thing on the left, and the
                // chevron sits at the right edge like the select's does: both
                // controls open a list, so both say so the same way.
                control_pill(self.key.clone(), &state, self.tab_index)
                    .w_full()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .min_w_0()
                            .gap(space::SM)
                            .child(
                                // No outline: the control's own edge already
                                // separates the swatch from the surface, and a
                                // second edge inside it only crops the color.
                                div()
                                    .flex_none()
                                    .w(SWATCH_SIZE)
                                    .h(SWATCH_SIZE)
                                    .rounded(SWATCH_RADIUS)
                                    .bg(swatch.hsla()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font(numeric_font())
                                    .child(format!("#{}", self.value)),
                            ),
                    )
                    .child(select_chevron())
            },
        )
    }
}

/// Parse a six-digit hexadecimal color, naming the exact problem.
pub fn parse_hex_color(value: &str) -> Result<Color, String> {
    let trimmed = value.trim().trim_start_matches('#');
    if trimmed.len() != 6 {
        return Err(format!(
            "Enter six hexadecimal digits, for example 7C5CFF. Got {} character{}.",
            trimmed.len(),
            if trimmed.len() == 1 { "" } else { "s" }
        ));
    }
    match u32::from_str_radix(trimmed, 16) {
        Ok(parsed) => Ok(Color::rgb(parsed)),
        Err(_) => Err("Only the digits 0-9 and letters A-F are accepted.".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::set_focus_visible;
    use gpui::size;

    #[test]
    fn the_focus_ring_follows_how_focus_last_moved() {
        // Default true, so the first Tab of a session lands somewhere visible
        // rather than on a control that gives no sign of holding focus.
        assert!(focus_visible(), "a session starts keyboard-visible");

        set_focus_visible(false);
        assert!(!focus_visible());
        // A control built while the pointer is driving carries no focus style,
        // which is what removes the ring; it is still a tab stop.
        let _ = Button::new("apply", "Apply").tab_index(3).render();

        set_focus_visible(true);
        assert!(focus_visible());
        let _ = Button::new("apply", "Apply").tab_index(3).render();
    }

    #[test]
    fn hex_colors_are_parsed_and_their_failures_are_named() {
        assert_eq!(parse_hex_color("7C5CFF").unwrap(), Color::rgb(0x7c5cff));
        assert_eq!(parse_hex_color("#7c5cff").unwrap(), Color::rgb(0x7c5cff));
        assert_eq!(parse_hex_color(" 000000 ").unwrap(), Color::rgb(0x000000));

        let short = parse_hex_color("7C5C").unwrap_err();
        assert!(short.contains("six hexadecimal digits"), "{short}");
        let invalid = parse_hex_color("ZZZZZZ").unwrap_err();
        assert!(invalid.contains("0-9"), "{invalid}");
        assert!(parse_hex_color("").is_err());
    }

    #[test]
    fn every_button_variant_builds_in_each_state() {
        // GPUI asserts that a hover style is set once, so a primitive that
        // layers its own accent over the shared one only fails when the screen
        // holding it is actually rendered. Building them here catches it in
        // `cargo test` instead of on the first click.
        for variant in [
            ButtonVariant::Primary,
            ButtonVariant::Secondary,
            ButtonVariant::Danger,
        ] {
            for state in [
                ControlState::Enabled,
                ControlState::disabled("Read-only."),
                ControlState::error("Invalid."),
            ] {
                let _ = Button::new("action", "Apply")
                    .variant(variant)
                    .state(state)
                    .render();
            }
        }
    }

    #[test]
    fn a_color_field_with_invalid_input_is_in_error_not_silently_reset() {
        let state = match parse_hex_color("12345") {
            Ok(_) => ControlState::Enabled,
            Err(error) => ControlState::error(error),
        };
        assert!(matches!(state, ControlState::Error { .. }));
        assert!(state.message().unwrap().contains("six"));
    }

    #[test]
    fn a_select_without_a_matching_option_says_so_instead_of_showing_a_value() {
        let select = Select::new("mode", "Mode")
            .options(vec![SelectOption::new("fixed", "Fixed")])
            .selected("curve");
        assert_eq!(select.display_value(), "Not available");

        let select = Select::new("mode", "Mode")
            .options(vec![SelectOption::new("fixed", "Fixed")])
            .selected("fixed");
        assert_eq!(select.display_value(), "Fixed");
    }

    #[test]
    fn a_slider_fraction_stays_inside_its_range() {
        let slider = Slider::new("duty", "Duty", 50.0).range(0.0, 100.0);
        assert!((slider.fraction() - 0.5).abs() < 0.001);

        let below = Slider::new("duty", "Duty", -20.0).range(0.0, 100.0);
        assert_eq!(below.fraction(), 0.0);

        let above = Slider::new("duty", "Duty", 400.0).range(0.0, 100.0);
        assert_eq!(above.fraction(), 1.0);

        let degenerate = Slider::new("duty", "Duty", 50.0).range(10.0, 10.0);
        assert_eq!(degenerate.fraction(), 0.0);
    }

    #[test]
    fn a_pointer_on_the_track_selects_the_value_that_position_marks() {
        // A track 200 wide starting at x=100, which is what an offset row
        // produces: reading the position without the origin would put every
        // value half a track too high.
        let track = Bounds {
            origin: Point {
                x: px(100.0),
                y: px(40.0),
            },
            size: size(px(200.0), px(22.0)),
        };
        let at = |x: f32| {
            Slider::value_at(
                track,
                Point {
                    x: px(x),
                    y: px(50.0),
                },
                0.0,
                100.0,
            )
        };

        assert_eq!(at(100.0), 0.0, "the left edge is the low end");
        assert_eq!(at(200.0), 50.0, "the middle is the middle");
        assert_eq!(at(300.0), 100.0, "the right edge is the high end");

        // A drag that leaves the track keeps pinning the end it left by,
        // rather than stopping at wherever the pointer crossed the edge.
        assert_eq!(at(-500.0), 0.0);
        assert_eq!(at(5_000.0), 100.0);
    }

    #[test]
    fn a_slider_fills_the_fraction_its_value_stands_for() {
        let slider = |value: f32| Slider::new("brightness", "Brightness", value).range(0.0, 100.0);
        assert_eq!(slider(0.0).fraction(), 0.0);
        assert_eq!(slider(50.0).fraction(), 0.5);
        assert_eq!(slider(100.0).fraction(), 1.0);
        // A value outside the range is drawn at the end it passed, never off
        // the track or with a negative width.
        assert_eq!(slider(140.0).fraction(), 1.0);
        assert_eq!(slider(-20.0).fraction(), 0.0);
        // A degenerate range cannot divide by zero.
        assert_eq!(Slider::new("x", "x", 5.0).range(3.0, 3.0).fraction(), 0.0);
    }
}
