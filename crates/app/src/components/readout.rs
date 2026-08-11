// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The readouts: what a device is, and what one metric currently says.
//!
//! Nothing here is operable. Each of them states a value and how much it can be
//! trusted, and states the second in a word rather than in a color alone.

use gpui::{Div, Hsla, SharedString, div, prelude::*, px};

use kori_core::telemetry::MetricView;

use crate::assets::Icon;
use crate::theme::{DEVICE_LINE_HEIGHT, META_SEPARATOR, color, numeric_font, space};

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

    /// One line: what the device is, how it was identified, and what state it
    /// is in.
    ///
    /// It used to be two lines inside a titled panel, and on the monitoring
    /// screen that block outweighed the readouts under it: a card, a heading, a
    /// sentence of policy and four lines of prose, all of it answering a
    /// question the operator asks once a session. Provenance is a caption, so it
    /// is set like one. The whole line is [`text_xs`](Div::text_xs) and hierarchy
    /// is carried by color alone: the name in ink, the identity muted, the state
    /// in its own color at the far right.
    ///
    /// The row carries no separator, no fill and no leading status glyph. State
    /// is still named in words, never by color alone; the colored label is the
    /// word. The identity block takes the slack between the two, so the state
    /// column lands on the same right edge on every line whatever the name in
    /// front of it measures, and truncates rather than wrapping: a device that
    /// reports a long firmware string must not push its own state off the line.
    ///
    /// `min_w_0` belongs on the flex containers, never on the element that
    /// holds the text. On a text element it removes the intrinsic minimum a
    /// line needs, and GPUI then wraps the name one glyph per line rather than
    /// letting the row be as wide as its content.
    pub fn render(self) -> Div {
        div()
            .flex()
            // Wrapping is the escape hatch for a window narrow enough that the
            // name and the state alone no longer fit. The identity block shrinks
            // first and reaches zero before that happens, so in practice this
            // only fires on a window narrower than the layout targets.
            .flex_wrap()
            .items_center()
            .w_full()
            .min_w_0()
            .min_h(DEVICE_LINE_HEIGHT)
            .gap(space::SM)
            .text_xs()
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(color::TEXT.hsla())
                    .child(self.name),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(space::XS)
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(
                        div()
                            .flex_none()
                            .font(numeric_font())
                            .child(self.identifier),
                    )
                    .children(self.detail.map(|detail| {
                        div()
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap(space::XS)
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(color::TEXT_DISABLED.hsla())
                                    .child(META_SEPARATOR),
                            )
                            .child(div().min_w_0().truncate().child(detail))
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(self.health.color())
                    .child(self.health.label()),
            )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::DEGREE_C;

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

    #[test]
    fn a_metric_without_a_value_reads_as_unavailable_not_as_zero() {
        let unavailable = Metric::new("GPU", None).unit(DEGREE_C);
        assert_eq!(unavailable.readout(), "--");

        let present = Metric::new("GPU", Some("51.0".to_string())).unit(DEGREE_C);
        assert_eq!(present.readout(), "51.0 \u{00b0}C");
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
}
