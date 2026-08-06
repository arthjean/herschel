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
    Bounds, Div, ElementId, Hsla, PathBuilder, Pixels, Point, SharedString, Stateful, Window,
    canvas, div, prelude::*, px,
};
use nzxt_core::profile::{CURVE_NODE_COUNT, CurveNodes};
use nzxt_core::telemetry::{History, MetricView};

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

    /// Text label, so status is never carried by color alone.
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
        let line_color = color::ACCENT.hsla();
        let gap_color = color::TEXT_DISABLED.hsla();
        let baseline_color = color::SEPARATOR.alpha(0.7);
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
            .bg(color::SURFACE.hsla())
            .border_1()
            .border_color(color::SEPARATOR.hsla())
            .child(
                canvas(
                    move |_, _, _| {},
                    move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                        paint_sparkline(
                            window,
                            bounds,
                            &values,
                            &segments,
                            line_color,
                            gap_color,
                            baseline_color,
                        );
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
fn paint_sparkline(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    values: &[Option<f32>],
    segments: &[(usize, usize)],
    line_color: Hsla,
    gap_color: Hsla,
    baseline_color: Hsla,
) {
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
        }
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
        let grid_color = color::SEPARATOR.alpha(0.6);
        let sink = self.bounds_sink.clone();

        let mut plot = div()
            .id("curve-plot")
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
                        if let Some(sink) = &sink {
                            sink.set(bounds);
                        }
                        paint_curve(
                            window,
                            bounds,
                            &nodes,
                            selected,
                            line_color,
                            marker_color,
                            grid_color,
                        );
                    },
                )
                .size_full(),
            );

        if enabled {
            plot = plot
                .tab_index(self.tab_index)
                .tab_stop(true)
                .cursor_pointer()
                .focus(|this| this.border_color(color::FOCUS.hsla()).border(FOCUS_RING));
        }

        plot
    }
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
pub fn node_at(bounds: Bounds<Pixels>, position: Point<Pixels>) -> (usize, u8) {
    let width = f32::from(bounds.size.width).max(1.0);
    let height = f32::from(bounds.size.height).max(1.0);
    let across = ((f32::from(position.x) - f32::from(bounds.origin.x)) / width).clamp(0.0, 1.0);
    let up = 1.0 - ((f32::from(position.y) - f32::from(bounds.origin.y)) / height).clamp(0.0, 1.0);

    let index = (across * (CURVE_NODE_COUNT - 1) as f32).round() as usize;
    let duty = (up * 255.0).round().clamp(0.0, 255.0) as u8;
    (index.min(CURVE_NODE_COUNT - 1), duty)
}

#[allow(clippy::too_many_arguments)]
fn paint_curve(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    nodes: &CurveNodes,
    selected: usize,
    line_color: Hsla,
    marker_color: Hsla,
    grid_color: Hsla,
) {
    for step in 1..4 {
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
            grid_color,
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
    // selected one is drawn wider so keyboard focus is legible without color.
    for (index, duty) in nodes.duty.iter().enumerate() {
        let center = plot_node(index, *duty, bounds);
        let half = if index == selected { px(7.0) } else { px(3.0) };
        let thickness = if index == selected { px(6.0) } else { px(4.0) };
        stroke_line(
            window,
            Point {
                x: center.x - half,
                y: center.y,
            },
            Point {
                x: center.x + half,
                y: center.y,
            },
            thickness,
            if index == selected {
                marker_color
            } else {
                line_color
            },
        );
    }
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

/// A labeled control with its optional state message underneath.
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
            assert!(!health.glyph().is_empty());
        }
        assert_ne!(DeviceHealth::Ready.glyph(), DeviceHealth::ReadOnly.glyph());
        assert_ne!(
            DeviceHealth::ReadOnly.glyph(),
            DeviceHealth::Unavailable.glyph()
        );
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
        let mut history = History::new(nzxt_core::telemetry::HISTORY_WINDOW_MS);
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
        let mut history = History::new(nzxt_core::telemetry::HISTORY_WINDOW_MS);
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
    fn a_note_states_its_severity_in_words() {
        for level in [NoteLevel::Info, NoteLevel::Warning, NoteLevel::Critical] {
            assert!(!level.label().is_empty());
        }
        assert_ne!(NoteLevel::Warning.label(), NoteLevel::Critical.label());
    }
}
