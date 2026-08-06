//! The reusable interface primitives.
//!
//! Every control shares one interaction model: it is either enabled, disabled
//! with a reason, or in error. Disabled is never decorative here. It is how the
//! product refuses to expose a write the hardware has not proven it supports,
//! so each primitive carries the reason rather than silently greying out.

use gpui::{
    Bounds, Div, ElementId, Hsla, PathBuilder, Pixels, Point, SharedString, Stateful, Window,
    canvas, div, prelude::*, px,
};

use crate::theme::{Color, FOCUS_RING, RADIUS, TARGET_MIN, color, numeric_font, space};

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

    fn border_color(&self) -> Hsla {
        match self {
            Self::Enabled => color::SEPARATOR.hsla(),
            Self::Disabled { .. } => color::SEPARATOR.alpha(0.5),
            Self::Error { .. } => color::DANGER.hsla(),
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

/// A base interactive surface with the shared focus, hover and active states.
///
/// Every primitive builds on this, so focus looks and behaves the same
/// everywhere and no control can accidentally ship without a focus ring.
fn interactive(id: impl Into<ElementId>, state: &ControlState, tab_index: isize) -> Stateful<Div> {
    let enabled = state.is_enabled() || matches!(state, ControlState::Error { .. });
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .min_h(TARGET_MIN)
        .rounded(RADIUS)
        .border_1()
        .border_color(state.border_color())
        .text_color(state.text_color());

    if enabled {
        base.tab_index(tab_index)
            .tab_stop(true)
            .cursor_pointer()
            .hover(|this| this.bg(color::CONTROL.alpha(0.75)))
            .active(|this| this.bg(color::ACCENT_ACTIVE.alpha(0.35)))
            .focus(|this| {
                this.border_color(color::FOCUS.hsla())
                    .border(FOCUS_RING)
                    .bg(color::CONTROL.alpha(0.6))
            })
    } else {
        // A disabled control is not a tab stop: keyboard traversal must not
        // stop on something that cannot be operated.
        base.cursor_default().opacity(0.6)
    }
}

/// A labelled action.
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
            ButtonVariant::Danger => color::DANGER.alpha(0.18),
        }
    }

    fn label_color(&self) -> Hsla {
        if !self.state.is_enabled() {
            return color::TEXT_DISABLED.hsla();
        }
        match self.variant {
            ButtonVariant::Primary => color::TEXT_ON_ACCENT.hsla(),
            ButtonVariant::Secondary => color::TEXT.hsla(),
            ButtonVariant::Danger => color::DANGER.hsla(),
        }
    }

    pub fn render(self) -> Stateful<Div> {
        let fill = self.fill();
        let label_color = self.label_color();
        let primary = self.variant == ButtonVariant::Primary && self.state.is_enabled();

        interactive(self.key.clone(), &self.state, self.tab_index)
            .justify_center()
            .px(space::LG)
            .bg(fill)
            .text_color(label_color)
            .when(primary, |this| {
                this.hover(|style| style.bg(color::ACCENT_HOVER.hsla()))
                    .active(|style| style.bg(color::ACCENT_ACTIVE.hsla()))
            })
            .child(self.label)
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
        let label = self.label.clone();
        let message = self.state.message().map(str::to_string);
        let state = self.state.clone();
        let field_id = SharedString::from(format!("{}-field", self.key));

        field(field_id, label, message, state.clone(), {
            interactive(self.key.clone(), &state, self.tab_index)
                .w_full()
                .justify_between()
                .px(space::MD)
                .bg(color::CONTROL.hsla())
                .child(value)
                .child(div().text_color(color::TEXT_MUTED.hsla()).child("▾"))
        })
    }
}

/// A two-state switch.
pub struct Toggle {
    key: SharedString,
    label: SharedString,
    on: bool,
    state: ControlState,
    tab_index: isize,
}

impl Toggle {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>, on: bool) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            on,
            state: ControlState::Enabled,
            tab_index: 0,
        }
    }

    pub fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        self
    }

    pub fn render(self) -> Stateful<Div> {
        let track = if self.on && self.state.is_enabled() {
            color::ACCENT.hsla()
        } else {
            color::CONTROL.hsla()
        };
        let knob_side = if self.on { "flex-end" } else { "flex-start" };
        let state = self.state.clone();
        let message = state.message().map(str::to_string);
        let field_id = SharedString::from(format!("{}-field", self.key));

        field(field_id, self.label.clone(), message, state.clone(), {
            interactive(self.key.clone(), &state, self.tab_index)
                .w_full()
                .justify_between()
                .px(space::MD)
                .bg(color::CONTROL.alpha(0.4))
                .child(if self.on { "On" } else { "Off" })
                .child(
                    div()
                        .w(px(44.0))
                        .h(px(24.0))
                        .rounded(px(12.0))
                        .bg(track)
                        .flex()
                        .items_center()
                        .when(knob_side == "flex-end", |this| this.justify_end())
                        .px(px(3.0))
                        .child(
                            div()
                                .w(px(18.0))
                                .h(px(18.0))
                                .rounded(px(9.0))
                                .bg(color::TEXT.hsla()),
                        ),
                )
        })
    }
}

/// A bounded numeric control.
pub struct Slider {
    key: SharedString,
    label: SharedString,
    value: f32,
    min: f32,
    max: f32,
    unit: SharedString,
    state: ControlState,
    tab_index: isize,
}

impl Slider {
    pub fn new(key: impl Into<SharedString>, label: impl Into<SharedString>, value: f32) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            value,
            min: 0.0,
            max: 100.0,
            unit: SharedString::default(),
            state: ControlState::Enabled,
            tab_index: 0,
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

    /// Filled fraction, always inside 0.0 to 1.0.
    pub fn fraction(&self) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    pub fn render(self) -> Stateful<Div> {
        let fraction = self.fraction();
        let fill = if self.state.is_enabled() {
            color::ACCENT.hsla()
        } else {
            color::TEXT_DISABLED.alpha(0.6)
        };
        let readout = format!("{:.0}{}", self.value, self.unit);
        let state = self.state.clone();
        let message = state.message().map(str::to_string);
        let field_id = SharedString::from(format!("{}-field", self.key));

        field(field_id, self.label.clone(), message, state.clone(), {
            interactive(self.key.clone(), &state, self.tab_index)
                .w_full()
                .gap(space::MD)
                .px(space::MD)
                .bg(color::CONTROL.alpha(0.4))
                .child(
                    div()
                        .flex_1()
                        .h(px(6.0))
                        .rounded(px(3.0))
                        .bg(color::SEPARATOR.hsla())
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(fraction))
                                .rounded(px(3.0))
                                .bg(fill),
                        ),
                )
                .child(
                    div()
                        .font(numeric_font())
                        .min_w(px(56.0))
                        .text_align(gpui::TextAlign::Right)
                        .child(readout),
                )
        })
    }
}

/// A six-digit hexadecimal colour input with a live swatch.
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

        field(field_id, self.label.clone(), message, state.clone(), {
            interactive(self.key.clone(), &state, self.tab_index)
                .w_full()
                .gap(space::MD)
                .px(space::MD)
                .bg(color::CONTROL.hsla())
                .child(
                    div()
                        .w(px(22.0))
                        .h(px(22.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(color::SEPARATOR.hsla())
                        .bg(swatch.hsla()),
                )
                .child(div().font(numeric_font()).child(format!("#{}", self.value)))
        })
    }
}

/// Parse a six-digit hexadecimal colour, naming the exact problem.
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
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::MD)
            .p(space::LG)
            .rounded(RADIUS)
            .bg(color::PANEL.hsla())
            .border_1()
            .border_color(color::SEPARATOR.hsla())
            .child(
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
    fn color(self) -> Hsla {
        match self {
            Self::Ready => color::SUCCESS.hsla(),
            Self::ReadOnly => color::WARNING.hsla(),
            Self::Unavailable => color::DANGER.hsla(),
        }
    }

    /// Text label, so status is never carried by colour alone.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::ReadOnly => "Read-only",
            Self::Unavailable => "Unavailable",
        }
    }

    /// A shape cue, for the same reason.
    fn glyph(self) -> &'static str {
        match self {
            Self::Ready => "●",
            Self::ReadOnly => "◐",
            Self::Unavailable => "○",
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

    pub fn render(self) -> Div {
        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::XS)
            .py(space::SM)
            .border_b_1()
            .border_color(color::SEPARATOR.hsla())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(self.health.color())
                                    .child(self.health.glyph()),
                            )
                            .child(div().text_color(color::TEXT.hsla()).child(self.name)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(space::MD)
                            .child(
                                div()
                                    .font(numeric_font())
                                    .text_sm()
                                    .text_color(color::TEXT_MUTED.hsla())
                                    .child(self.identifier),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(self.health.color())
                                    .child(self.health.label()),
                            ),
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

    let centre = bounds.center();
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
            x: centre.x + radius * angle.cos(),
            y: centre.y + radius * angle.sin(),
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

/// A control node of the cooling curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveNode {
    pub temperature_c: f32,
    pub duty_percent: f32,
}

/// The liquid-temperature curve editor.
///
/// Editing here changes nothing on the device: the surface is a pure view of
/// the pending curve, and applying it is a separate, explicit action.
pub struct CurveEditor {
    nodes: Vec<CurveNode>,
    state: ControlState,
    height: Pixels,
}

impl CurveEditor {
    pub fn new(nodes: Vec<CurveNode>) -> Self {
        Self {
            nodes,
            state: ControlState::Enabled,
            height: px(200.0),
        }
    }

    pub fn state(mut self, state: ControlState) -> Self {
        self.state = state;
        self
    }

    pub fn render(self) -> Div {
        let nodes = self.nodes.clone();
        let enabled = self.state.is_enabled();
        let line_color = if enabled {
            color::ACCENT.hsla()
        } else {
            color::TEXT_DISABLED.hsla()
        };
        let grid_color = color::SEPARATOR.alpha(0.6);
        let message = self.state.message().map(str::to_string);

        div()
            .flex()
            .flex_col()
            .gap(space::SM)
            .child(
                div()
                    .w_full()
                    .h(self.height)
                    .rounded(RADIUS)
                    .bg(color::SURFACE.hsla())
                    .border_1()
                    .border_color(color::SEPARATOR.hsla())
                    .child(
                        canvas(
                            move |_, _, _| {},
                            move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                                paint_curve(window, bounds, &nodes, line_color, grid_color);
                            },
                        )
                        .size_full(),
                    ),
            )
            .children(message.map(|message| {
                div()
                    .text_sm()
                    .text_color(if enabled {
                        color::TEXT_MUTED.hsla()
                    } else {
                        color::WARNING.hsla()
                    })
                    .child(message)
            }))
    }
}

/// Normalise a node onto the editor's plot area.
///
/// Temperature runs left to right over the kernel's 20-59 C range, duty runs
/// bottom to top over 0-100%.
pub fn plot_point(node: CurveNode, bounds: Bounds<Pixels>) -> Point<Pixels> {
    let temperature = ((node.temperature_c - 20.0) / 39.0).clamp(0.0, 1.0);
    let duty = (node.duty_percent / 100.0).clamp(0.0, 1.0);
    Point {
        x: bounds.origin.x + bounds.size.width * temperature,
        y: bounds.origin.y + bounds.size.height * (1.0 - duty),
    }
}

fn paint_curve(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    nodes: &[CurveNode],
    line_color: Hsla,
    grid_color: Hsla,
) {
    for step in 1..4 {
        let y = bounds.origin.y + bounds.size.height * (step as f32 / 4.0);
        let mut builder = PathBuilder::stroke(px(1.0));
        builder.move_to(Point {
            x: bounds.origin.x,
            y,
        });
        builder.line_to(Point {
            x: bounds.origin.x + bounds.size.width,
            y,
        });
        if let Ok(path) = builder.build() {
            window.paint_path(path, grid_color);
        }
    }

    if nodes.len() < 2 {
        return;
    }

    let mut builder = PathBuilder::stroke(px(2.0));
    for (index, node) in nodes.iter().enumerate() {
        let point = plot_point(*node, bounds);
        if index == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, line_color);
    }

    // Node markers, so each control point is visible without hovering.
    for node in nodes {
        let centre = plot_point(*node, bounds);
        let mut marker = PathBuilder::stroke(px(4.0));
        marker.move_to(Point {
            x: centre.x - px(2.0),
            y: centre.y,
        });
        marker.line_to(Point {
            x: centre.x + px(2.0),
            y: centre.y,
        });
        if let Ok(path) = marker.build() {
            window.paint_path(path, line_color);
        }
    }
}

/// A labelled control with its optional state message underneath.
///
/// The wrapper carries the element id, so the whole field is one click target
/// rather than just the control box inside it.
fn field(
    id: impl Into<ElementId>,
    label: SharedString,
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
        .child(
            div()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child(label),
        )
        .child(control)
        .children(message.map(|message| div().text_sm().text_color(message_color).child(message)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn a_disabled_control_carries_its_reason() {
        let state = ControlState::disabled("Another process owns this device.");
        assert!(state.is_disabled());
        assert!(!state.is_enabled());
        assert_eq!(state.message(), Some("Another process owns this device."));
    }

    #[test]
    fn hex_colours_are_parsed_and_their_failures_are_named() {
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
    fn a_colour_field_with_invalid_input_is_in_error_not_silently_reset() {
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
    fn device_health_is_never_conveyed_by_colour_alone() {
        for health in [
            DeviceHealth::Ready,
            DeviceHealth::ReadOnly,
            DeviceHealth::Unavailable,
        ] {
            assert!(!health.label().is_empty());
            assert!(!health.glyph().is_empty());
        }
        assert_ne!(DeviceHealth::Ready.glyph(), DeviceHealth::ReadOnly.glyph());
        assert_ne!(
            DeviceHealth::ReadOnly.glyph(),
            DeviceHealth::Unavailable.glyph()
        );
    }

    #[test]
    fn curve_nodes_map_onto_the_plot_area() {
        let bounds = Bounds {
            origin: Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: size(px(400.0), px(200.0)),
        };

        let bottom_left = plot_point(
            CurveNode {
                temperature_c: 20.0,
                duty_percent: 0.0,
            },
            bounds,
        );
        assert_eq!(bottom_left.x, px(0.0));
        assert_eq!(bottom_left.y, px(200.0));

        let top_right = plot_point(
            CurveNode {
                temperature_c: 59.0,
                duty_percent: 100.0,
            },
            bounds,
        );
        assert_eq!(top_right.x, px(400.0));
        assert_eq!(top_right.y, px(0.0));
    }

    #[test]
    fn curve_nodes_outside_the_abi_range_are_clamped_into_the_plot() {
        let bounds = Bounds {
            origin: Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: size(px(400.0), px(200.0)),
        };

        let point = plot_point(
            CurveNode {
                temperature_c: 90.0,
                duty_percent: 300.0,
            },
            bounds,
        );
        assert_eq!(point.x, px(400.0));
        assert_eq!(point.y, px(0.0));
    }
}
