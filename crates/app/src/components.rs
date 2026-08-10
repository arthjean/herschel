// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The reusable interface primitives.
//!
//! Every control shares one interaction model: it is either enabled, disabled
//! with a reason, or in error. Disabled is never decorative here. It is how the
//! product refuses to expose a write the hardware has not proven it supports,
//! so each primitive carries the reason rather than silently graying out.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Div, ElementId, Hsla, PathBuilder, Pixels, Point, SharedString, Stateful, Svg, Window,
    canvas, div, fill, point, prelude::*, px, size, svg,
};
use herschel_core::profile::{CURVE_NODE_COUNT, CurveNodes, MAX_DUTY_PERCENT, duty_from_percent};
use herschel_core::telemetry::{History, MetricView};

use crate::assets::Icon;
use crate::theme::{
    CONTROL_HEIGHT, Color, FOCUS_RING, MENU_GLYPH_SIZE, META_SEPARATOR, RADIUS, SWATCH_RADIUS,
    SWATCH_SIZE, color, numeric_font, space,
};

/// What a control is allowed to do right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlState {
    Enabled,
    /// Interaction is refused, and `reason` says why in operator language.
    Disabled {
        reason: String,
    },
    /// The current value is invalid, and `message` names the accepted input.
    Error {
        message: String,
    },
}

impl ControlState {
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self::Disabled {
            reason: reason.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled { .. })
    }

    /// The sentence shown next to the control, when there is one.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Enabled => None,
            Self::Disabled { reason } => Some(reason),
            Self::Error { message } => Some(message),
        }
    }

    fn text_color(&self) -> Hsla {
        match self {
            Self::Enabled => color::TEXT.hsla(),
            Self::Disabled { .. } => color::TEXT_DISABLED.hsla(),
            Self::Error { .. } => color::TEXT.hsla(),
        }
    }
}

/// Visual weight of a [`Button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    /// The one accented action of a screen.
    Primary,
    Secondary,
    /// Destructive or safety-relevant action.
    Danger,
}

thread_local! {
    /// Whether focus arrived by keyboard, and a ring is therefore wanted.
    static FOCUS_VISIBLE: Cell<bool> = const { Cell::new(true) };
}

/// Whether controls should draw their focus ring right now.
///
/// This is what a browser calls `:focus-visible`, which GPUI has no equivalent
/// of: its `focus` style applies however focus arrived, so every clicked
/// control keeps a ring afterwards. The ring is what tells someone navigating
/// by keyboard where they are, and noise to someone who just pointed at the
/// control they were already looking at.
///
/// It is thread-local process state rather than a parameter because that is
/// what it describes: how the person is driving the window right now, not a
/// property of any one control. Threading it through would put the same value
/// in every primitive's signature and in every call site. The interface renders
/// on one thread, so a `Cell` is the whole synchronization story.
pub fn focus_visible() -> bool {
    FOCUS_VISIBLE.with(Cell::get)
}

/// Record how focus last moved. The shell sets this; controls only read it.
pub fn set_focus_visible(visible: bool) {
    FOCUS_VISIBLE.with(|flag| flag.set(visible));
}

/// The pill every value control is built on, matched to Paneflow's
/// `select_trigger`.
///
/// A subtle-gray pill with no outline, 10 by 6 of padding, and a fill that
/// sinks rather than lifts under the pointer. The focus ring is taken out of
/// that padding rather than added to it, so reserving the ring leaves the pill
/// exactly the size of the one it matches and focusing it moves nothing.
///
/// Shared by the select, the color field and the slider rather than copied into
/// each: they sit on the same lines as each other, and a pill that is a pixel
/// taller in one of them is visible as a step in the row.
///
/// An outline comes back for one state only: a control holding a value that
/// cannot be parsed. Nothing else on the pill says so, and the message under it
/// is not in the same place the eye is.
fn control_pill(id: impl Into<ElementId>, state: &ControlState, tab_index: isize) -> Stateful<Div> {
    pill(id, state, tab_index, true)
}

/// The same pill, unfilled: a slider's own surface.
///
/// A track needs the height and the reserved ring, so it stays on the baseline
/// of the control beside it, but not the fill. A slider already draws a shape
/// of its own, and a fill behind it reads as a second card parked inside the
/// row rather than as part of it.
fn slider_surface(
    id: impl Into<ElementId>,
    state: &ControlState,
    tab_index: isize,
) -> Stateful<Div> {
    pill(id, state, tab_index, false)
}

fn pill(
    id: impl Into<ElementId>,
    state: &ControlState,
    tab_index: isize,
    filled: bool,
) -> Stateful<Div> {
    let enabled = state.is_enabled() || matches!(state, ControlState::Error { .. });
    let resting = match state {
        ControlState::Error { .. } => color::DANGER.hsla(),
        _ => color::CONTROL.alpha(0.0),
    };
    let base = div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap(space::SM)
        .min_h(CONTROL_HEIGHT)
        .px(space::SM)
        .py(space::XS)
        .border(FOCUS_RING)
        .border_color(resting)
        .rounded(RADIUS)
        .bg(if filled {
            color::CONTROL.hsla()
        } else {
            color::CONTROL.alpha(0.0)
        })
        .text_xs()
        .text_color(state.text_color());

    if enabled {
        base.tab_index(tab_index)
            .tab_stop(true)
            .cursor_pointer()
            // An unfilled surface has no fill to change: pointing at it would
            // make a card appear that was not there, which says the wrong
            // thing about what is under the pointer.
            .when(filled, |this| {
                this.hover(|this| this.bg(color::CONTROL_HOVER.hsla()))
                    .active(|this| this.bg(color::ACCENT_ACTIVE.alpha(0.35)))
            })
            .when(focus_visible(), |this| {
                this.focus(|this| this.border_color(color::FOCUS.hsla()))
            })
    } else {
        // A disabled control is not a tab stop: keyboard traversal must not
        // stop on something that cannot be operated.
        base.cursor_default().opacity(0.6)
    }
}

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

/// A six-digit hexadecimal color input with a live swatch.
pub struct ColorField {
    key: SharedString,
    label: SharedString,
    value: String,
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
            tab_index: 0,
        }
    }

    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    pub fn render(self) -> Stateful<Div> {
        let parsed = parse_hex_color(&self.value);
        let state = match &parsed {
            Ok(_) => ControlState::Enabled,
            Err(error) => ControlState::error(error.clone()),
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

/// A titled section of the work surface.
pub struct Panel {
    title: SharedString,
    subtitle: Option<SharedString>,
}

impl Panel {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn render(self) -> Div {
        panel_surface().child(
            div()
                .flex()
                .flex_col()
                .gap(space::XS)
                .child(
                    div()
                        .text_color(color::TEXT.hsla())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(self.title),
                )
                .children(self.subtitle.map(|subtitle| {
                    div()
                        .text_sm()
                        .text_color(color::TEXT_MUTED.hsla())
                        .child(subtitle)
                })),
        )
    }
}

/// Side of an icon drawn inline with text, in logical pixels.
pub const ICON_SIZE: Pixels = px(16.0);

/// One icon at one size, in one color.
///
/// The color is not optional and is not decoration: GPUI renders an SVG to an
/// alpha mask and tints it with the element's text color, so an icon without
/// one resolves to `None` in `Svg::paint` and draws nothing at all. Routing
/// every icon through here is what keeps that from being a per-call-site
/// mistake, and what keeps icon color coming from `theme.rs` like every other
/// color in this interface.
pub fn icon(icon: Icon, size: Pixels, color: Hsla) -> Svg {
    svg().path(icon.path()).size(size).text_color(color)
}

/// The glyph at the right edge of a menu trigger.
///
/// Smaller than [`ICON_SIZE`] and dimmer than the value beside it, as Paneflow's
/// `select_chevron` is: it says the control opens, and says it under the value
/// rather than beside it in the same weight.
pub fn select_chevron() -> Svg {
    icon(
        Icon::ChevronDown,
        MENU_GLYPH_SIZE,
        color::TEXT_MUTED.alpha(0.7),
    )
    .flex_none()
}

/// The disclosure chevron of a collapsible row.
pub fn chevron(open: bool, color: Hsla) -> Svg {
    let name = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    icon(name, ICON_SIZE, color).flex_none()
}

/// The raised surface a [`Panel`] draws on, without its heading.
///
/// Shared rather than duplicated so a section that carries no title still sits
/// on exactly the same surface as every titled one. It carries no outline: a
/// card is told apart from the ground under it by its luminance, and the panel
/// fill already clears the work surface it sits on.
pub fn panel_surface() -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .gap(space::MD)
        .p(space::LG)
        .rounded(RADIUS)
        .bg(color::PANEL.hsla())
}

/// How a device presents in a [`DeviceRow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceHealth {
    /// Present and writable.
    Ready,
    /// Present but read-only.
    ReadOnly,
    /// Not present, or ownership was refused.
    Unavailable,
}

impl DeviceHealth {
    /// The status color, shared with any screen that names a device's state.
    pub fn color(self) -> Hsla {
        match self {
            Self::Ready => color::SUCCESS.hsla(),
            Self::ReadOnly => color::WARNING.hsla(),
            Self::Unavailable => color::DANGER.hsla(),
        }
    }

    /// Text label, so status is never carried by color alone.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::ReadOnly => "Read-only",
            Self::Unavailable => "Unavailable",
        }
    }

    /// A shape cue, for the same reason.
    pub fn icon(self) -> Icon {
        match self {
            Self::Ready => Icon::CircleCheck,
            Self::ReadOnly => Icon::Lock,
            Self::Unavailable => Icon::AlertCircle,
        }
    }
}

/// One hardware device and its current state.
pub struct DeviceRow {
    name: SharedString,
    identifier: SharedString,
    health: DeviceHealth,
    detail: Option<SharedString>,
}

impl DeviceRow {
    pub fn new(
        name: impl Into<SharedString>,
        identifier: impl Into<SharedString>,
        health: DeviceHealth,
    ) -> Self {
        Self {
            name: name.into(),
            identifier: identifier.into(),
            health,
            detail: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Two lines: what the device is and what state it is in, then everything
    /// that only matters once one of those two raises a question.
    ///
    /// The row carries no separator and no leading status glyph. It is a list
    /// of two devices inside a panel that already draws its own surface, so the
    /// panel's own gap is the only rhythm it needs, and dropping the glyph is
    /// what lets the device name start on the same vertical as the panel title
    /// above it. State is still named in words, never by color alone; the
    /// colored label is the word.
    /// `min_w_0` belongs on the flex containers, never on the element that
    /// holds the text. On a text element it removes the intrinsic minimum a
    /// line needs, and GPUI then wraps the name one glyph per line rather than
    /// letting the row be as wide as its content.
    pub fn render(self) -> Div {
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(px(2.0))
            .child(
                div()
                    .flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .child(div().text_color(color::TEXT.hsla()).child(self.name))
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(self.health.color())
                            .child(self.health.label()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::SM)
                    .text_xs()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(div().font(numeric_font()).child(self.identifier))
                    .children(self.detail.map(|detail| {
                        div()
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                div()
                                    .text_color(color::TEXT_DISABLED.hsla())
                                    .child(META_SEPARATOR),
                            )
                            .child(div().child(detail))
                    })),
            )
    }
}

/// A circular readout painted directly, not composed from boxes.
pub struct Gauge {
    label: SharedString,
    /// `None` renders the unavailable state instead of a zero.
    value: Option<f32>,
    min: f32,
    max: f32,
    unit: SharedString,
    diameter: Pixels,
    arc_color: Color,
}

impl Gauge {
    pub fn new(label: impl Into<SharedString>, value: Option<f32>) -> Self {
        Self {
            label: label.into(),
            value,
            min: 0.0,
            max: 100.0,
            unit: SharedString::default(),
            diameter: px(120.0),
            arc_color: color::ACCENT,
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

    pub fn diameter(mut self, diameter: Pixels) -> Self {
        self.diameter = diameter;
        self
    }

    pub fn arc_color(mut self, arc_color: Color) -> Self {
        self.arc_color = arc_color;
        self
    }

    /// Filled fraction, or `None` when the metric is unavailable.
    pub fn fraction(&self) -> Option<f32> {
        let value = self.value?;
        if self.max <= self.min || !value.is_finite() {
            return Some(0.0);
        }
        Some(((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0))
    }

    /// The readout, using an explicit marker rather than a fabricated zero.
    pub fn readout(&self) -> String {
        match self.value {
            Some(value) => format!("{value:.1}{}", self.unit),
            None => "--".to_string(),
        }
    }

    pub fn render(self) -> Div {
        let diameter = self.diameter;
        let fraction = self.fraction();
        let readout = self.readout();
        let arc_color = if fraction.is_some() {
            self.arc_color.hsla()
        } else {
            color::TEXT_DISABLED.hsla()
        };
        let track_color = color::SEPARATOR.hsla();

        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(space::SM)
            .child(
                div()
                    .relative()
                    .w(diameter)
                    .h(diameter)
                    .child(
                        canvas(
                            move |_, _, _| {},
                            move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                                paint_ring(window, bounds, 1.0, track_color, px(10.0));
                                if let Some(fraction) = fraction {
                                    paint_ring(window, bounds, fraction, arc_color, px(10.0));
                                }
                            },
                        )
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .font(numeric_font())
                            .text_color(color::TEXT.hsla())
                            .child(readout),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(self.label),
            )
    }
}

/// Paint a fraction of a ring inside `bounds`.
///
/// The sweep starts at the top and runs clockwise, which is the direction a
/// rising value is read in.
fn paint_ring(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    fraction: f32,
    color: Hsla,
    thickness: Pixels,
) {
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }

    let center = bounds.center();
    let radius = (bounds.size.width.min(bounds.size.height) - thickness) / 2.0;
    if radius <= px(0.0) {
        return;
    }

    let segments = ((fraction * 96.0).ceil() as usize).max(2);
    let mut builder = PathBuilder::stroke(thickness);
    for step in 0..=segments {
        let progress = step as f32 / segments as f32 * fraction;
        let angle = -std::f32::consts::FRAC_PI_2 + progress * std::f32::consts::TAU;
        let point = Point {
            x: center.x + radius * angle.cos(),
            y: center.y + radius * angle.sin(),
        };
        if step == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// A numeric readout with its unit, freshness and one dominant bar.
///
/// The qualifier is a word, not a color: "Stale" and "N/A" are what carry the
/// meaning, and the color only reinforces them.
pub struct Metric {
    label: SharedString,
    value: Option<String>,
    unit: SharedString,
    qualifier: Option<&'static str>,
    detail: Option<String>,
    fraction: Option<f32>,
    stale: bool,
}

impl Metric {
    /// Build from a formatted value, or `None` when there is nothing to show.
    pub fn new(label: impl Into<SharedString>, value: Option<String>) -> Self {
        Self {
            label: label.into(),
            value,
            unit: SharedString::default(),
            qualifier: None,
            detail: None,
            fraction: None,
            stale: false,
        }
    }

    /// Build straight from a metric view, taking its value and its freshness.
    pub fn from_view(
        label: impl Into<SharedString>,
        view: &MetricView<f32>,
        format: impl Fn(f32) -> String,
    ) -> Self {
        let mut metric = Self::new(label, view.copied().map(format))
            .qualifier(view.qualifier())
            .detail(view.detail().map(str::to_string));
        // Taken from the view rather than inferred from the qualifier string:
        // freshness is a state, not a label to parse back.
        metric.stale = view.is_stale();
        debug_assert_eq!(metric.value.is_none(), view.is_unavailable());
        metric
    }

    pub fn unit(mut self, unit: impl Into<SharedString>) -> Self {
        self.unit = unit.into();
        self
    }

    pub fn qualifier(mut self, qualifier: Option<&'static str>) -> Self {
        self.qualifier = qualifier;
        self
    }

    pub fn detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }

    /// Add the section's dominant bar, as a fraction of full scale.
    pub fn bar(mut self, fraction: Option<f32>) -> Self {
        self.fraction = fraction.map(|value| value.clamp(0.0, 1.0));
        self
    }

    /// The number as shown, using an explicit marker rather than a zero.
    pub fn readout(&self) -> String {
        match &self.value {
            Some(value) => format!("{value}{}", self.unit),
            None => "--".to_string(),
        }
    }

    pub fn render(self) -> Div {
        let readout = self.readout();
        let available = self.value.is_some();
        let fill = if !available {
            color::TEXT_DISABLED.hsla()
        } else if self.stale {
            color::WARNING.hsla()
        } else {
            color::ACCENT.hsla()
        };

        div()
            .flex()
            .flex_col()
            // Shares the row with its siblings so the bar spans the width it
            // is given: a dominant bar per section is the point, and a bar
            // sized to the width of its label is not one.
            .flex_1()
            .min_w(px(150.0))
            .gap(space::XS)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .gap(space::SM)
                    .child(
                        div()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child(self.label),
                    )
                    .children(self.qualifier.map(|qualifier| {
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(if available {
                                color::WARNING.hsla()
                            } else {
                                color::TEXT_DISABLED.hsla()
                            })
                            .child(qualifier)
                    })),
            )
            .child(
                div()
                    .font(numeric_font())
                    .text_color(if available {
                        color::TEXT.hsla()
                    } else {
                        color::TEXT_DISABLED.hsla()
                    })
                    .child(readout),
            )
            .child(
                div()
                    .w_full()
                    .h(px(6.0))
                    .rounded(px(3.0))
                    .bg(color::SEPARATOR.hsla())
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(self.fraction.unwrap_or(0.0)))
                            .rounded(px(3.0))
                            .bg(fill),
                    ),
            )
            .children(self.detail.map(|detail| {
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(detail)
            }))
    }
}

/// A rolling series drawn as a line, with holes where samples are missing.
///
/// A gap is drawn as a gap. Joining across it would invent a value, and
/// flattening it to zero would invent a plunge.
pub struct Sparkline {
    history: Vec<Option<f32>>,
    min: f32,
    max: f32,
}

/// Height of every chart, so four sections read as one system.
const SPARKLINE_HEIGHT: Pixels = px(56.0);

/// Most points a chart ever plots.
///
/// A fifteen-minute window holds nine hundred samples, and a chart this size is
/// a few hundred pixels wide: plotting every sample would build a path with
/// more vertices than the chart has columns, once per second, for every
/// section. The cost of that is measurable in the idle CPU budget and none of
/// it is visible.
pub const MAX_PLOTTED_POINTS: usize = 180;

impl Sparkline {
    pub fn new(history: &History, min: f32, max: f32) -> Self {
        Self {
            history: downsample(history, MAX_PLOTTED_POINTS),
            min,
            max,
        }
    }

    /// Vertical position of a value inside the plot, from 0.0 at the bottom.
    pub fn fraction(&self, value: f32) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    /// Runs of consecutive present samples, as index ranges.
    ///
    /// Each run becomes one path, which is what leaves the holes visible.
    pub fn segments(&self) -> Vec<(usize, usize)> {
        let mut runs = Vec::new();
        let mut start: Option<usize> = None;
        for (index, value) in self.history.iter().enumerate() {
            match (value, start) {
                (Some(_), None) => start = Some(index),
                (None, Some(first)) => {
                    runs.push((first, index - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(first) = start {
            runs.push((first, self.history.len() - 1));
        }
        runs
    }

    pub fn render(self) -> Div {
        let colors = SparklineColors {
            line: color::ACCENT.hsla(),
            // The same accent, at full opacity. Coverage is what makes the
            // ramp, so a translucent cell would fade a texture that is already
            // fading and leave the baseline weaker than the value it stands for.
            area: color::ACCENT.hsla(),
            gap: color::TEXT_DISABLED.hsla(),
            baseline: color::SEPARATOR.alpha(0.7),
        };
        let segments = self.segments();
        // Normalized once here, so the painter only places points and the
        // scale is exercised by the same code a test can call.
        let values: Vec<Option<f32>> = self
            .history
            .iter()
            .map(|value| value.map(|value| self.fraction(value)))
            .collect();

        div()
            .w_full()
            .h(SPARKLINE_HEIGHT)
            .rounded(RADIUS)
            // Darker than the panel it sits in, which is what sets the plot
            // area apart now that no card in this interface is outlined.
            .bg(color::SURFACE.hsla())
            .child(
                canvas(
                    move |_, _, _| {},
                    move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                        paint_sparkline(window, bounds, &values, &segments, &colors);
                    },
                )
                .size_full(),
            )
    }
}

/// Reduce a series to at most `limit` plotted points.
///
/// A bucket is a gap when *any* sample inside it is missing, never when only
/// some are. Widening a hole by one bucket overstates how much is missing;
/// averaging it away would claim data that was never read, and that is the
/// error this product refuses to make.
fn downsample(history: &History, limit: usize) -> Vec<Option<f32>> {
    let samples: Vec<Option<f32>> = history.points().map(|point| point.value).collect();
    if samples.len() <= limit || limit == 0 {
        return samples;
    }

    let bucket = samples.len().div_ceil(limit);
    samples
        .chunks(bucket)
        .map(|chunk| {
            if chunk.iter().any(Option::is_none) {
                return None;
            }
            let sum: f32 = chunk.iter().flatten().sum();
            Some(sum / chunk.len() as f32)
        })
        .collect()
}

/// Paint a series whose values are already normalized into 0.0 to 1.0.
#[allow(clippy::too_many_arguments)]
/// Side of one dither cell, in logical pixels.
///
/// Three pixels rather than one. A one-pixel cell is the honest ordered dither,
/// and on a plot a few hundred pixels wide it is also tens of thousands of
/// quads repainted every sample, on four sections at once, which is spendable
/// only against a budget this process does not have. At three the texture still
/// reads as a texture and the cell count stays in the low thousands.
const DITHER_CELL: Pixels = px(3.0);

/// Bayer ordered-dither threshold map, as its integer ranks.
///
/// The classic recursive 4x4 matrix. Kept as ranks rather than as fractions so
/// the table can be checked for what it has to be, a permutation of `0..16`: a
/// transposed or duplicated entry does not break the fill, it just makes the
/// texture quietly uneven, which is the kind of defect nobody finds by looking.
const BAYER_RANKS: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Distinct densities the map resolves.
const BAYER_LEVELS: f32 = 16.0;

/// Whether the cell at this grid position is lit at this density.
///
/// The dither is what carries the tone: a cell is either the full color or
/// nothing, and the eye integrates the coverage. Nothing here is translucent,
/// which is what keeps the texture crisp at any window scale.
pub fn dither_cell(column: usize, row: usize, density: f32) -> bool {
    let rank = f32::from(BAYER_RANKS[row % 4][column % 4]);
    density.clamp(0.0, 1.0) * BAYER_LEVELS > rank
}

/// Height of the plotted series at one horizontal position, from 0.0 at the
/// baseline, or `None` where the series has a hole there.
///
/// A hole on either side of the position is a hole at the position. The line
/// leaves gaps visible for the same reason, and an area that closed over one
/// would invent the volume the line refuses to invent.
pub fn column_fraction(values: &[Option<f32>], position: f32) -> Option<f32> {
    if values.len() < 2 {
        return None;
    }
    let last = values.len() - 1;
    let scaled = position.clamp(0.0, 1.0) * last as f32;
    let low = scaled.floor() as usize;
    let high = scaled.ceil() as usize;
    let start = values.get(low).copied().flatten()?;
    let end = values.get(high).copied().flatten()?;
    Some(start + (end - start) * (scaled - low as f32))
}

/// Dither density at a depth below the curve, over the depth of the column.
///
/// Dense at the baseline and thinning out toward the line, which is what makes
/// the fill read as volume under a value rather than as a second series.
pub fn area_density(depth: f32, span: f32) -> f32 {
    if span <= 0.0 || depth <= 0.0 {
        return 0.0;
    }
    (depth / span).clamp(0.0, 1.0)
}

/// Whether a point falls inside a rounded rectangle.
///
/// GPUI masks paint to a rectangle, so a cell landing in a corner would square
/// off the curve the container's own fill draws. Cells are tested one by one
/// rather than the grid being inset, because an inset would leave a bare band
/// exactly along the baseline, where the texture is densest.
fn inside_rounded(bounds: Bounds<Pixels>, x: f32, y: f32, radius: f32) -> bool {
    let left = f32::from(bounds.origin.x) + radius;
    let right = f32::from(bounds.origin.x + bounds.size.width) - radius;
    let top = f32::from(bounds.origin.y) + radius;
    let bottom = f32::from(bounds.origin.y + bounds.size.height) - radius;
    // A container narrower or shorter than two radii inverts the bounds, and
    // `f32::clamp` panics on an inverted range rather than saturating.
    let dx = x - x.clamp(left.min(right), right.max(left));
    let dy = y - y.clamp(top.min(bottom), bottom.max(top));
    dx * dx + dy * dy <= radius * radius
}

/// Fill under the curve, as an ordered-dither ramp.
fn paint_dithered_area(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[Option<f32>],
    radius: Pixels,
    color: Hsla,
) {
    if values.len() < 2 || bounds.size.width <= px(0.0) || bounds.size.height <= px(0.0) {
        return;
    }

    let columns = (bounds.size.width / DITHER_CELL).ceil() as usize;
    let rows = (bounds.size.height / DITHER_CELL).ceil() as usize;
    let height = f32::from(bounds.size.height);
    let radius = f32::from(radius);

    for column in 0..columns {
        let x = bounds.origin.x + DITHER_CELL * column as f32;
        let center_x = f32::from(x) + f32::from(DITHER_CELL) * 0.5;
        let position = (center_x - f32::from(bounds.origin.x)) / f32::from(bounds.size.width);
        let Some(fraction) = column_fraction(values, position) else {
            continue;
        };

        let top = height * (1.0 - fraction.clamp(0.0, 1.0));
        for row in 0..rows {
            let y = bounds.origin.y + DITHER_CELL * row as f32;
            let center_y = f32::from(y) + f32::from(DITHER_CELL) * 0.5;
            let depth = center_y - f32::from(bounds.origin.y) - top;
            let density = area_density(depth, height - top);
            if density <= 0.0
                || !dither_cell(column, row, density)
                || !inside_rounded(bounds, center_x, center_y, radius)
            {
                continue;
            }

            // Clamped so the last cell of a row or column cannot paint past
            // the plot it belongs to.
            let width = DITHER_CELL.min(bounds.origin.x + bounds.size.width - x);
            let cell_height = DITHER_CELL.min(bounds.origin.y + bounds.size.height - y);
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(width, cell_height)),
                color,
            ));
        }
    }
}

/// The colors one sparkline is painted in.
struct SparklineColors {
    line: Hsla,
    area: Hsla,
    gap: Hsla,
    baseline: Hsla,
}

fn paint_sparkline(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[Option<f32>],
    segments: &[(usize, usize)],
    colors: &SparklineColors,
) {
    let SparklineColors {
        line: line_color,
        area: area_color,
        gap: gap_color,
        baseline: baseline_color,
    } = *colors;

    paint_dithered_area(window, bounds, values, RADIUS, area_color);

    stroke_line(
        window,
        Point {
            x: bounds.origin.x,
            y: bounds.origin.y + bounds.size.height,
        },
        Point {
            x: bounds.origin.x + bounds.size.width,
            y: bounds.origin.y + bounds.size.height,
        },
        px(1.0),
        baseline_color,
    );

    if values.len() < 2 {
        return;
    }

    let last = (values.len() - 1) as f32;
    let position = |index: usize, fraction: f32| Point {
        x: bounds.origin.x + bounds.size.width * (index as f32 / last),
        y: bounds.origin.y + bounds.size.height * (1.0 - fraction.clamp(0.0, 1.0)),
    };

    for (first, last_index) in segments {
        if last_index == first {
            continue;
        }
        let mut builder = PathBuilder::stroke(px(2.0));
        for (offset, value) in values[*first..=*last_index].iter().enumerate() {
            let Some(value) = value else { continue };
            let index = first + offset;
            let point = position(index, *value);
            if offset == 0 {
                builder.move_to(point);
            } else {
                builder.line_to(point);
            }
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, line_color);
        }
    }

    // A tick at the baseline under every hole: the break in the line is the
    // primary cue, and this makes it legible even for a single missing sample.
    for (index, value) in values.iter().enumerate() {
        if value.is_some() {
            continue;
        }
        let x = bounds.origin.x + bounds.size.width * (index as f32 / last);
        stroke_line(
            window,
            Point {
                x,
                y: bounds.origin.y + bounds.size.height,
            },
            Point {
                x,
                y: bounds.origin.y + bounds.size.height - px(6.0),
            },
            px(1.5),
            gap_color,
        );
    }
}

/// The liquid-temperature curve editor.
///
/// Editing here changes nothing on the device: the surface is a pure view of
/// the pending curve, and applying it is a separate, explicit action.
pub struct CurveEditor {
    nodes: CurveNodes,
    selected: usize,
    state: ControlState,
    height: Pixels,
    tab_index: isize,
    /// Filled during paint so pointer events can be mapped back onto a node.
    bounds_sink: Option<Rc<Cell<Bounds<Pixels>>>>,
    /// Liquid temperature to mark on the plot, when one has been read.
    liquid_c: Option<f32>,
}

impl CurveEditor {
    pub fn new(nodes: CurveNodes) -> Self {
        Self {
            nodes,
            selected: 0,
            state: ControlState::Enabled,
            height: px(200.0),
            tab_index: 0,
            bounds_sink: None,
            liquid_c: None,
        }
    }

    /// The reading the curve is actually steered by, marked on the plot.
    ///
    /// `None` when nothing has been read, which is drawn as no marker rather
    /// than as a marker at zero.
    pub fn liquid(mut self, celsius: Option<f32>) -> Self {
        self.liquid_c = celsius;
        self
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index.min(CURVE_NODE_COUNT - 1);
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

    /// Where the plot area is published for hit testing.
    pub fn bounds_sink(mut self, sink: Rc<Cell<Bounds<Pixels>>>) -> Self {
        self.bounds_sink = Some(sink);
        self
    }

    pub fn render(self) -> Stateful<Div> {
        let nodes = self.nodes;
        let selected = self.selected;
        let enabled = self.state.is_enabled();
        let line_color = if enabled {
            color::ACCENT.hsla()
        } else {
            color::TEXT_DISABLED.hsla()
        };
        let marker_color = if enabled {
            color::FOCUS.hsla()
        } else {
            color::TEXT_DISABLED.hsla()
        };
        let sink = self.bounds_sink.clone();
        let liquid_c = self.liquid_c;

        // No fill and no border: the grid and the axis labels are what bound
        // the plot, and a frame around them would be a second boundary inside
        // the panel that already provides one.
        let plot = div().w_full().h(self.height).child(
            canvas(
                move |_, _, _| {},
                move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                    // The drawing area is inset so a node sitting at the
                    // top or at either end is a whole marker rather than
                    // half of one clipped by the border. Hit testing reads
                    // the same rectangle, so a pointer lands where the
                    // marker is drawn.
                    let area = bounds.inset(PLOT_INSET);
                    if let Some(sink) = &sink {
                        sink.set(area);
                    }
                    paint_curve(window, area, &nodes, selected, line_color, marker_color);
                    paint_liquid_marker(window, area, liquid_c);
                },
            )
            .size_full(),
        );

        let mut frame = div()
            .id("curve-plot")
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::XS)
            .rounded(RADIUS)
            // The ring is reserved rather than added on focus, so focusing the
            // plot does not move the axes around it.
            .border(FOCUS_RING)
            .border_color(color::PANEL.alpha(0.0))
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .gap(space::XS)
                    .child(duty_axis(self.height))
                    .child(div().flex_1().min_w_0().child(plot)),
            )
            .child(
                div()
                    .flex()
                    .w_full()
                    .min_w_0()
                    .gap(space::XS)
                    .child(div().flex_none().w(AXIS_LABEL_WIDTH))
                    .child(temperature_axis()),
            );

        if enabled {
            frame = frame
                .tab_index(self.tab_index)
                .tab_stop(true)
                .cursor_pointer()
                .when(focus_visible(), |this| {
                    this.focus(|this| this.border_color(color::FOCUS.hsla()))
                });
        }

        frame
    }
}

/// Margin kept between the plot's border and the outermost node.
const PLOT_INSET: Pixels = px(10.0);
/// Width reserved for the duty labels down the left of the plot.
const AXIS_LABEL_WIDTH: Pixels = px(34.0);

/// The duty scale, aligned with the grid lines it names.
fn duty_axis(height: Pixels) -> Div {
    div()
        .flex()
        .flex_none()
        .flex_col()
        .justify_between()
        .items_end()
        .w(AXIS_LABEL_WIDTH)
        .h(height)
        // Matches the inset the plot is drawn with, so 100% sits on the top
        // grid line rather than on the border above it.
        .py(PLOT_INSET)
        .children(["100%", "75%", "50%", "25%", "0%"].map(|label| {
            div()
                .font(numeric_font())
                .text_xs()
                .text_color(color::TEXT_MUTED.hsla())
                .child(label)
        }))
}

/// The temperature scale, one label per node.
fn temperature_axis() -> Div {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .justify_between()
        .px(PLOT_INSET)
        .children((0..CURVE_NODE_COUNT).map(|index| {
            div()
                .font(numeric_font())
                .text_xs()
                .text_color(color::TEXT_MUTED.hsla())
                .child(format!("{:.0}", CurveNodes::temperature_at(index)))
        }))
}

/// Position of one curve node inside the plot area.
///
/// Temperature runs left to right over the kernel's 20-59 C range, duty runs
/// bottom to top over the full 0-255 PWM scale.
pub fn plot_node(index: usize, duty: u8, bounds: Bounds<Pixels>) -> Point<Pixels> {
    let across = index.min(CURVE_NODE_COUNT - 1) as f32 / (CURVE_NODE_COUNT - 1) as f32;
    let up = duty as f32 / 255.0;
    Point {
        x: bounds.origin.x + bounds.size.width * across,
        y: bounds.origin.y + bounds.size.height * (1.0 - up),
    }
}

/// The node a pointer position selects, and the duty that height represents.
///
/// The height is read as whole percent rather than as one of 256 duties. That
/// is the scale the device actually has: the driver stores a percentage, so
/// two duties a step apart routinely mean the same setting. Reading finer than
/// the hardware can hold would make some values unreachable, since the plot is
/// shorter than 256 pixels, and would let a pixel of hand tremor produce an
/// edit that writes a full curve and changes nothing.
pub fn node_at(bounds: Bounds<Pixels>, position: Point<Pixels>) -> (usize, u8) {
    let width = f32::from(bounds.size.width).max(1.0);
    let height = f32::from(bounds.size.height).max(1.0);
    let across = ((f32::from(position.x) - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0);
    let up = 1.0 - ((f32::from(position.y) - f32::from(bounds.origin.y)) / height).clamp(0.0, 1.0);

    let index = (across * (CURVE_NODE_COUNT - 1) as f32).round() as usize;
    let percent = (up * MAX_DUTY_PERCENT as f32).round().clamp(0.0, 100.0) as u8;
    (index.min(CURVE_NODE_COUNT - 1), duty_from_percent(percent))
}

#[allow(clippy::too_many_arguments)]
fn paint_curve(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    nodes: &CurveNodes,
    selected: usize,
    line_color: Hsla,
    marker_color: Hsla,
) {
    // The duty grid, one line per labeled step including the two edges, so
    // every label on the axis has a line to sit against.
    for step in 0..=4 {
        let y = bounds.origin.y + bounds.size.height * (step as f32 / 4.0);
        stroke_line(
            window,
            Point {
                x: bounds.origin.x,
                y,
            },
            Point {
                x: bounds.origin.x + bounds.size.width,
                y,
            },
            px(1.0),
            color::SEPARATOR.alpha(if step == 0 || step == 4 { 0.75 } else { 0.5 }),
        );
    }

    // One vertical line per node, so a duty can be read back to the
    // temperature that produces it without counting markers.
    for index in 0..CURVE_NODE_COUNT {
        let x = plot_node(index, 0, bounds).x;
        stroke_line(
            window,
            Point {
                x,
                y: bounds.origin.y,
            },
            Point {
                x,
                y: bounds.origin.y + bounds.size.height,
            },
            px(1.0),
            color::SEPARATOR.alpha(0.28),
        );
    }

    let mut builder = PathBuilder::stroke(px(2.0));
    for (index, duty) in nodes.duty.iter().enumerate() {
        let point = plot_node(index, *duty, bounds);
        if index == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, line_color);
    }

    // Node markers, so each control point is visible without hovering, and the
    // selected one is drawn wider and ringed so keyboard focus is legible
    // without relying on color alone.
    for (index, duty) in nodes.duty.iter().enumerate() {
        let center = plot_node(index, *duty, bounds);
        if index == selected {
            paint_dot(window, center, px(7.0), marker_color);
            paint_dot(window, center, px(3.5), color::SURFACE.hsla());
        } else {
            paint_dot(window, center, px(4.5), line_color);
        }
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

/// A filled circle of `radius` centered on `center`.
fn paint_dot(window: &mut Window, center: Point<Pixels>, radius: Pixels, color: Hsla) {
    let bounds = Bounds {
        origin: Point {
            x: center.x - radius,
            y: center.y - radius,
        },
        size: gpui::size(radius * 2.0, radius * 2.0),
    };
    window.paint_quad(gpui::fill(bounds, color).corner_radii(radius));
}

/// Where the coolant currently sits on the temperature axis.
///
/// Drawn only when the reading falls inside the range the curve covers: a
/// marker pinned to an edge would claim a temperature the plot cannot show.
fn paint_liquid_marker(window: &mut Window, bounds: Bounds<Pixels>, liquid_c: Option<f32>) {
    let Some(celsius) = liquid_c else { return };
    let first = CurveNodes::temperature_at(0);
    let last = CurveNodes::temperature_at(CURVE_NODE_COUNT - 1);
    if celsius < first || celsius > last {
        return;
    }

    let across = (celsius - first) / (last - first);
    let x = bounds.origin.x + bounds.size.width * across;
    stroke_line(
        window,
        Point {
            x,
            y: bounds.origin.y,
        },
        Point {
            x,
            y: bounds.origin.y + bounds.size.height,
        },
        px(1.5),
        color::TEXT_MUTED.alpha(0.7),
    );
}

fn stroke_line(
    window: &mut Window,
    from: Point<Pixels>,
    to: Point<Pixels>,
    thickness: Pixels,
    color: Hsla,
) {
    let mut builder = PathBuilder::stroke(thickness);
    builder.move_to(from);
    builder.line_to(to);
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// How urgent a [`Note`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    Info,
    Warning,
    Critical,
}

impl NoteLevel {
    fn color(self) -> Color {
        match self {
            Self::Info => color::TEXT_MUTED,
            Self::Warning => color::WARNING,
            Self::Critical => color::DANGER,
        }
    }

    /// A word, so urgency is never carried by color alone.
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Note",
            Self::Warning => "Warning",
            Self::Critical => "Critical",
        }
    }
}

/// One state message with its severity spelled out.
pub struct Note {
    level: NoteLevel,
    message: SharedString,
}

impl Note {
    pub fn new(level: NoteLevel, message: impl Into<SharedString>) -> Self {
        Self {
            level,
            message: message.into(),
        }
    }

    pub fn render(self) -> Div {
        let accent = self.level.color();
        div()
            .flex()
            .w_full()
            .min_w_0()
            .gap(space::SM)
            .p(space::MD)
            .rounded(RADIUS)
            .bg(accent.alpha(0.12))
            .border_1()
            .border_color(accent.alpha(0.5))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(accent.hsla())
                    .child(self.level.label()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(color::TEXT.hsla())
                    .child(self.message),
            )
    }
}

/// A control with its optional caption above and state message underneath.
///
/// The wrapper carries the element id, so the whole field is one click target
/// rather than just the control box inside it.
///
/// `label` is optional because a control on a device row is named by the row it
/// is on: repeating the name over every control is a caption that says what the
/// line already said, and it doubles the height of the line to do it.
fn field(
    id: impl Into<ElementId>,
    label: Option<SharedString>,
    message: Option<String>,
    state: ControlState,
    control: impl IntoElement,
) -> Stateful<Div> {
    let message_color = match state {
        ControlState::Error { .. } => color::DANGER.hsla(),
        _ => color::TEXT_MUTED.hsla(),
    };

    div()
        .id(id)
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .gap(space::XS)
        .children(label.map(|label| {
            div()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child(label)
        }))
        .child(control)
        .children(message.map(|message| div().text_sm().text_color(message_color).child(message)))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn a_disabled_control_carries_its_reason() {
        let state = ControlState::disabled("Another process owns this device.");
        assert!(state.is_disabled());
        assert!(!state.is_enabled());
        assert_eq!(state.message(), Some("Another process owns this device."));
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
    fn an_unavailable_gauge_reads_as_unavailable_not_as_zero() {
        let unavailable = Gauge::new("GPU", None);
        assert_eq!(unavailable.readout(), "--");
        assert_eq!(unavailable.fraction(), None);

        let zero = Gauge::new("GPU", Some(0.0));
        assert_eq!(zero.readout(), "0.0");
        assert_eq!(zero.fraction(), Some(0.0));
    }

    #[test]
    fn gauge_values_outside_the_range_are_clamped() {
        let gauge = Gauge::new("Liquid", Some(80.0)).range(20.0, 60.0);
        assert_eq!(gauge.fraction(), Some(1.0));
        let gauge = Gauge::new("Liquid", Some(0.0)).range(20.0, 60.0);
        assert_eq!(gauge.fraction(), Some(0.0));
    }

    #[test]
    fn device_health_is_never_conveyed_by_color_alone() {
        for health in [
            DeviceHealth::Ready,
            DeviceHealth::ReadOnly,
            DeviceHealth::Unavailable,
        ] {
            assert!(!health.label().is_empty());
        }
        // Three distinct icons, so the state is legible without reading the
        // color and without reading the label either.
        assert_ne!(DeviceHealth::Ready.icon(), DeviceHealth::ReadOnly.icon());
        assert_ne!(
            DeviceHealth::ReadOnly.icon(),
            DeviceHealth::Unavailable.icon()
        );
        assert_ne!(DeviceHealth::Ready.icon(), DeviceHealth::Unavailable.icon());
    }

    fn plot() -> Bounds<Pixels> {
        Bounds {
            origin: Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: size(px(400.0), px(200.0)),
        }
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

    #[test]
    fn curve_nodes_map_onto_the_plot_area() {
        let bounds = plot();

        let bottom_left = plot_node(0, 0, bounds);
        assert_eq!(bottom_left.x, px(0.0));
        assert_eq!(bottom_left.y, px(200.0));

        let top_right = plot_node(CURVE_NODE_COUNT - 1, 255, bounds);
        assert_eq!(top_right.x, px(400.0));
        assert_eq!(top_right.y, px(0.0));

        // An index past the last node is clamped into the plot rather than
        // painted outside it.
        assert_eq!(plot_node(99, 255, bounds).x, px(400.0));
    }

    #[test]
    fn a_pointer_position_resolves_to_a_node_and_a_duty() {
        let bounds = plot();

        assert_eq!(
            node_at(
                bounds,
                Point {
                    x: px(0.0),
                    y: px(200.0)
                }
            ),
            (0, 0)
        );
        assert_eq!(
            node_at(
                bounds,
                Point {
                    x: px(400.0),
                    y: px(0.0)
                }
            ),
            (CURVE_NODE_COUNT - 1, 255)
        );

        // Halfway across selects the middle node; halfway up is half duty.
        let (index, duty) = node_at(
            bounds,
            Point {
                x: px(200.0),
                y: px(100.0),
            },
        );
        assert!((4..=5).contains(&index), "index {index}");
        assert!((126..=129).contains(&duty), "duty {duty}");

        // A position outside the plot is clamped, never wrapped.
        assert_eq!(
            node_at(
                bounds,
                Point {
                    x: px(-40.0),
                    y: px(900.0)
                }
            ),
            (0, 0)
        );
    }

    #[test]
    fn a_metric_without_a_value_reads_as_unavailable_not_as_zero() {
        let unavailable = Metric::new("GPU", None).unit(" C");
        assert_eq!(unavailable.readout(), "--");

        let present = Metric::new("GPU", Some("51.0".to_string())).unit(" C");
        assert_eq!(present.readout(), "51.0 C");
    }

    #[test]
    fn a_metric_takes_its_qualifier_from_the_view_it_was_built_from() {
        let stale = MetricView::Stale {
            value: 46.8,
            age_ms: 3_000,
        };
        let metric = Metric::from_view("CPU", &stale, |value| format!("{value:.1}"));
        assert_eq!(metric.qualifier, Some("Stale"));
        assert_eq!(metric.readout(), "46.8");

        let missing: MetricView<f32> = MetricView::Unavailable { cause: None };
        let metric = Metric::from_view("CPU", &missing, |value| format!("{value:.1}"));
        assert_eq!(metric.qualifier, Some("N/A"));
        assert_eq!(metric.readout(), "--");
    }

    #[test]
    fn a_sparkline_breaks_its_line_at_every_gap() {
        let mut history = History::new(60_000);
        for (step, value) in [Some(1.0), Some(2.0), None, Some(4.0), Some(5.0)]
            .into_iter()
            .enumerate()
        {
            history.push(step as u64 * 1_000, value);
        }

        let sparkline = Sparkline::new(&history, 0.0, 10.0);
        assert_eq!(sparkline.segments(), vec![(0, 1), (3, 4)]);
        assert!((sparkline.fraction(5.0) - 0.5).abs() < 0.001);
        assert_eq!(sparkline.fraction(-4.0), 0.0);
        assert_eq!(sparkline.fraction(40.0), 1.0);
    }

    #[test]
    fn a_full_window_is_reduced_to_a_bounded_number_of_plotted_points() {
        let mut history = History::new(herschel_core::telemetry::HISTORY_WINDOW_MS);
        for step in 0..900u64 {
            history.push(step * 1_000, Some(step as f32 % 100.0));
        }
        assert_eq!(history.len(), 900);

        let sparkline = Sparkline::new(&history, 0.0, 100.0);
        assert!(
            sparkline.history.len() <= MAX_PLOTTED_POINTS,
            "{} points plotted",
            sparkline.history.len()
        );
        assert!(sparkline.history.iter().all(Option::is_some));
    }

    #[test]
    fn downsampling_never_hides_a_gap() {
        let mut history = History::new(herschel_core::telemetry::HISTORY_WINDOW_MS);
        for step in 0..900u64 {
            history.push(step * 1_000, if step == 500 { None } else { Some(1.0) });
        }

        let sparkline = Sparkline::new(&history, 0.0, 10.0);
        assert_eq!(
            sparkline.history.iter().filter(|v| v.is_none()).count(),
            1,
            "the single missing sample must survive as a hole"
        );
        // Two runs remain, which is what makes the break visible.
        assert_eq!(sparkline.segments().len(), 2);
    }

    #[test]
    fn a_short_window_is_plotted_sample_for_sample() {
        let mut history = History::new(60_000);
        for step in 0..20u64 {
            history.push(step * 1_000, Some(step as f32));
        }
        assert_eq!(Sparkline::new(&history, 0.0, 20.0).history.len(), 20);
    }

    #[test]
    fn a_sparkline_of_only_gaps_draws_no_segment() {
        let mut history = History::new(60_000);
        for step in 0..4u64 {
            history.push(step * 1_000, None);
        }
        assert!(Sparkline::new(&history, 0.0, 10.0).segments().is_empty());
    }

    #[test]
    fn the_dither_map_is_a_permutation_of_its_levels() {
        let mut seen: Vec<u8> = BAYER_RANKS.iter().flatten().copied().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..BAYER_LEVELS as u8).collect::<Vec<u8>>());
    }

    #[test]
    fn the_dither_lights_no_cell_when_empty_and_every_cell_when_full() {
        for row in 0..4 {
            for column in 0..4 {
                assert!(!dither_cell(column, row, 0.0));
                assert!(dither_cell(column, row, 1.0));
            }
        }
    }

    /// Coverage has to track density, or the ramp is not a ramp.
    #[test]
    fn the_dither_lights_more_cells_as_the_density_rises() {
        let lit = |density: f32| {
            (0..4)
                .flat_map(|row| (0..4).map(move |column| (column, row)))
                .filter(|(column, row)| dither_cell(*column, *row, density))
                .count()
        };
        assert_eq!(lit(0.5), 8, "half density has to light half the map");
        let mut previous = 0;
        for step in 0..=16 {
            let count = lit(step as f32 / 16.0);
            assert!(count >= previous, "coverage fell at {step}");
            previous = count;
        }
        assert_eq!(previous, 16);
    }

    /// The fill is dense at the baseline and thin at the line, never inverted.
    #[test]
    fn the_area_density_is_highest_at_the_baseline() {
        assert_eq!(area_density(0.0, 40.0), 0.0);
        assert!((area_density(20.0, 40.0) - 0.5).abs() < 0.001);
        assert_eq!(area_density(40.0, 40.0), 1.0);
        assert_eq!(area_density(80.0, 40.0), 1.0);
        // A column with no depth under it, and a curve sitting on the baseline.
        assert_eq!(area_density(5.0, 0.0), 0.0);
        assert_eq!(area_density(-5.0, 40.0), 0.0);
    }

    #[test]
    fn the_area_follows_the_series_between_its_samples() {
        let values = vec![Some(0.0), Some(1.0), Some(0.0)];
        assert_eq!(column_fraction(&values, 0.0), Some(0.0));
        assert_eq!(column_fraction(&values, 0.5), Some(1.0));
        assert_eq!(column_fraction(&values, 1.0), Some(0.0));
        assert!((column_fraction(&values, 0.25).unwrap() - 0.5).abs() < 0.001);
        // Off either end, rather than extrapolating a value nothing measured.
        assert_eq!(column_fraction(&values, -1.0), Some(0.0));
        assert_eq!(column_fraction(&values, 2.0), Some(0.0));
    }

    /// A hole in the series is a hole in the fill, exactly as it is in the line.
    #[test]
    fn the_area_opens_where_the_series_has_a_hole() {
        let values = vec![Some(1.0), None, Some(1.0)];
        assert_eq!(column_fraction(&values, 0.5), None);
        assert_eq!(column_fraction(&values, 0.3), None);
        assert_eq!(column_fraction(&values, 0.0), Some(1.0));
        assert_eq!(column_fraction(&values, 1.0), Some(1.0));
        assert_eq!(column_fraction(&[Some(1.0)], 0.0), None);
        assert_eq!(column_fraction(&[], 0.0), None);
    }

    #[test]
    fn no_dither_cell_lands_outside_the_rounded_plot() {
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(56.0)));
        let radius = f32::from(RADIUS);
        assert!(inside_rounded(bounds, 50.0, 28.0, radius));
        assert!(inside_rounded(bounds, 1.0, 28.0, radius));
        assert!(inside_rounded(bounds, 50.0, 55.0, radius));
        // The four corners, which is the whole reason this test exists.
        for (x, y) in [(0.5, 0.5), (99.5, 0.5), (0.5, 55.5), (99.5, 55.5)] {
            assert!(!inside_rounded(bounds, x, y, radius), "corner {x},{y}");
        }
    }

    /// `f32::clamp` panics on an inverted range, and a plot narrower than two
    /// radii inverts it. The window is resizable, so this is reachable.
    #[test]
    fn a_plot_smaller_than_its_own_radius_still_answers() {
        let radius = f32::from(RADIUS);
        for side in [0.0, 1.0, 4.0, 15.0] {
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(side), px(side)));
            // Reaching the assertion at all is the point. What it checks is
            // that the middle of a plot is never culled, however small it got.
            let middle = side / 2.0;
            assert!(
                inside_rounded(bounds, middle, middle, radius),
                "side {side}"
            );
        }
    }

    #[test]
    fn a_note_states_its_severity_in_words() {
        for level in [NoteLevel::Info, NoteLevel::Warning, NoteLevel::Critical] {
            assert!(!level.label().is_empty());
        }
        assert_ne!(NoteLevel::Warning.label(), NoteLevel::Critical.label());
    }
}
