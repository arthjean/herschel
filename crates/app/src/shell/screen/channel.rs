// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! One cooling channel as a line, with its controls one activation away.
//!
//! Collapsed, the line is readback only: what the channel is doing, from which
//! temperature source, in what mode. Opening it reveals the controls for that
//! channel alone, so the curve editor never has to ask which channel it is
//! editing.
//!
//! Every reading on the line comes from the hardware rather than from the
//! pending edit. A row that still names the previous program after a mode
//! change is a write that has not landed, and that is exactly what this line
//! has to be able to say.

use std::rc::Rc;

use gpui::{Bounds, Context, Div, Pixels, SharedString, div, prelude::*, px};

use kori_core::KRAKEN_BASE;
use kori_core::profile::{Channel, MAX_DUTY, MAX_DUTY_PERCENT, duty_to_percent};
use kori_core::telemetry::{MetricView, PwmMode, SafetyAlert};

use crate::components::{ControlState, CurveEditor, Slider, chevron, focus_visible};
use crate::cooling::CoolingMode;
use crate::shell::Shell;
use crate::theme::{
    FOCUS_RING, META_SEPARATOR, RADIUS, ROW_RADIUS, TARGET_MIN, color, numeric_font, space,
};

use super::row::ROW_DETAIL_INDENT;
use super::tab::{COOLING_OFFSET_CURVE, COOLING_OFFSET_DUTY, cooling_row_tab};
use super::write::WriteTarget;

/// Width reserved for one readback on a channel row, label and value together.
///
/// A floor rather than a fixed width, and for the same reason: an intrinsic
/// width puts `RPM 964` and `RPM 1785` in different columns, and the two rows
/// then read as two layouts. A floor still lets a reading longer than the target
/// size allows for push the column out instead of being clipped by it, which is
/// what the 200% interface scale needs.
pub const COOLING_READBACK_WIDTH: Pixels = px(124.0);
/// A compact labeled readback with its freshness qualifier.
fn readback(label: &'static str, view: &MetricView<f32>, format: impl Fn(f32) -> String) -> Div {
    let (value, qualifier) = match view {
        MetricView::Fresh { value } => (format(*value), None),
        MetricView::Stale { value, .. } => (format(*value), Some("Stale")),
        MetricView::Unavailable { .. } => ("--".to_string(), Some("N/A")),
    };

    div()
        .flex()
        .flex_none()
        .items_baseline()
        .gap(space::XS)
        .child(
            div()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child(label),
        )
        .child(
            div()
                .font(numeric_font())
                .text_color(if qualifier == Some("N/A") {
                    color::TEXT_DISABLED.hsla()
                } else {
                    color::TEXT.hsla()
                })
                .child(value),
        )
        .children(qualifier.map(|qualifier| {
            div()
                .text_sm()
                .text_color(color::WARNING.hsla())
                .child(qualifier)
        }))
}
/// What the device says a channel is running, in the words of the mode select.
///
/// Read back from the hardware rather than taken from the pending edit. A row
/// that still names the previous program after a mode change is a write that has
/// not landed yet, and that is exactly what this line has to be able to say.
/// The failsafe mode is named without a percentage on purpose. The kernel calls
/// it "100% failsafe", but the duty read back beside this line is whatever the
/// firmware is actually running, and a label claiming 100% next to a reading of
/// 52% states a number the hardware is not at.
fn reported_program(mode: Option<PwmMode>, percent: Option<f32>) -> String {
    match (mode, percent) {
        (None, _) => "Mode not reported".to_string(),
        (Some(PwmMode::FullSpeed), _) => "Firmware failsafe".to_string(),
        (Some(PwmMode::Fixed), Some(percent)) => format!("Fixed duty {percent:.0}%"),
        (Some(PwmMode::Fixed), None) => "Fixed duty".to_string(),
        (Some(PwmMode::Curve), _) => "Onboard curve".to_string(),
    }
}
impl Shell {
    /// One channel as a single line, with its controls one activation away.
    ///
    /// Collapsed, the line is readback only: what the channel is doing, from
    /// which temperature source, in what mode. Opening it reveals the controls
    /// for that channel alone, so the curve editor never has to ask which
    /// channel it is editing.
    pub(crate) fn channel_row(
        &self,
        channel: Channel,
        write: &ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        let now = self.now_unix_ms;
        let metrics = self.metrics.channel(channel);
        let rpm = metrics.rpm.view(now);
        let duty = metrics.duty.view(now);
        let mode = metrics.mode.view(now).copied();
        let confirmed_percent = self
            .kraken()
            .and_then(|kraken| kraken.channel(channel).duty_percent());
        let alerts: Vec<SafetyAlert> = self
            .link
            .channel_alerts(channel)
            .into_iter()
            .cloned()
            .collect();
        let open = self.cooling.is_expanded(channel);

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .p(space::XS)
            // Between the line and what it opened. Only ever applies when a
            // detail is there, since a closed row has a single child.
            .gap(space::SM)
            .rounded(ROW_RADIUS)
            // The same open state the Lighting rows carry: one fill under the
            // line and the curve it revealed, so the two read as one channel.
            .when(open, |this| this.bg(color::CONTROL.alpha(0.25)))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "cooling-row-{}",
                        channel.label().to_lowercase()
                    )))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .min_h(TARGET_MIN)
                    .px(space::SM)
                    // The head carries two lines, so it needs air of its own: at
                    // the pointer-target floor alone the pair would sit flush
                    // against the top and bottom of the row.
                    .py(space::XS)
                    .rounded(RADIUS)
                    // The ring is reserved rather than added on focus, so
                    // focusing a row does not move the line it sits on.
                    .border(FOCUS_RING)
                    .border_color(color::PANEL.alpha(0.0))
                    .cursor_pointer()
                    .tab_index(cooling_row_tab(channel))
                    .tab_stop(true)
                    .hover(|this| this.bg(color::CONTROL.alpha(0.5)))
                    // Shown to a keyboard user, who has no other way to know
                    // which row Enter would open, and withheld from a pointer
                    // user, who just aimed at it.
                    .when(focus_visible(), |this| {
                        this.focus(|this| this.border_color(color::FOCUS.hsla()))
                    })
                    .child(chevron(open, color::TEXT_MUTED.hsla()))
                    .child(
                        // Two lines where there was one: what the channel is,
                        // and what it has been told to do. The second line is
                        // the reported mode, not the pending edit, so a row that
                        // still says "Onboard" after a mode change is a write
                        // that has not landed rather than a stale label.
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_color(color::TEXT.hsla())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(channel.label()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(color::TEXT_MUTED.hsla())
                                    .child(reported_program(mode, confirmed_percent)),
                            ),
                    )
                    .child(div().flex_1().min_w_0())
                    .child(
                        div()
                            .flex_none()
                            .min_w(COOLING_READBACK_WIDTH)
                            .child(readback("RPM", &rpm, |value| format!("{value:.0}"))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_w(COOLING_READBACK_WIDTH)
                            .child(readback(
                                "PWM",
                                &duty.map(|value| f32::from(*value)),
                                move |value| {
                                    // The percentage comes from the reported duty,
                                    // not from the pending edit: this line is
                                    // readback, not intent.
                                    match confirmed_percent {
                                        Some(percent) => format!("{value:.0} ({percent:.0}%)"),
                                        None => format!("{value:.0}"),
                                    }
                                },
                            )),
                    )
                    .children(alerts.into_iter().map(|alert| {
                        div()
                            .flex_none()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(color::DANGER.hsla())
                            .child(match alert {
                                SafetyAlert::ChannelStalled { .. } => "Critical: not turning",
                                SafetyAlert::LiquidCritical { .. } => "Critical: coolant",
                            })
                    }))
                    .on_click(cx.listener(move |shell, _, _, cx| {
                        shell.cooling.toggle(channel);
                        // A popover anchored to the row that just moved would
                        // be left pointing at nothing.
                        shell.popover = None;
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(self.channel_detail(channel, write, cx))
            })
    }

    /// What the open row reveals: the curve, plus the stepper the mode uses.
    ///
    /// The curve is editable whenever the hardware accepts one, not only while
    /// the curve mode is selected: an edit sends nothing, and the first one
    /// selects that mode itself. What gates it is the channel's own curve
    /// capability, for the same reason the duty channels are gated separately.
    fn channel_detail(
        &self,
        channel: Channel,
        write: &ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        let base = cooling_row_tab(channel);
        let curve_state =
            self.link
                .cooling_state(KRAKEN_BASE, channel.curve_capability(), self.now_unix_ms);

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::MD)
            .pb(space::MD)
            .pl(ROW_DETAIL_INDENT)
            .when(self.cooling.mode == CoolingMode::Fixed, |this| {
                this.child(self.duty_slider(channel, write, base + COOLING_OFFSET_DUTY, cx))
            })
            .child(self.curve_editor(channel, curve_state, base + COOLING_OFFSET_CURVE, cx))
    }

    /// The fixed duty of one channel, as a slider the pointer drags.
    ///
    /// A track rather than the pair of steppers this used to be. The value runs
    /// over a hundred settings and the steppers moved one of them per press, so
    /// reaching the other end of the range took thirty activations and the
    /// control that did it was three boxes and a trailing sentence on one line.
    /// The track is one object, it says where the value sits without being read,
    /// and it is the same control the Lighting rows already carry.
    ///
    /// The keyboard keeps the precision the steppers had: Left and Right move a
    /// single percentage point, which is exactly one setting on the device, and
    /// Home and End reach the ends of the accepted range in one press.
    fn duty_slider(
        &self,
        channel: Channel,
        write: &ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let target = self.cooling.duty(channel);
        let percent = duty_to_percent(target);
        let floor = duty_to_percent(channel.min_duty());
        let enabled = write.is_enabled();

        // Published per render and captured by this render's listeners, exactly
        // as the brightness tracks are, so a press converts against the
        // rectangle the track was just painted at. Only an operable track is
        // recorded, so a press cannot grab a channel the hardware refused.
        let sink: Option<Rc<dyn Fn(Bounds<Pixels>)>> = enabled.then(|| {
            let tracks = Rc::clone(&self.duty_tracks);
            Rc::new(move |bounds| tracks.record(channel, bounds)) as Rc<dyn Fn(Bounds<Pixels>)>
        });

        let mut slider = Slider::new(
            SharedString::from(format!("{}-duty", channel.label().to_lowercase())),
            "Fixed duty",
            f32::from(percent),
        )
        // In percent, not in raw duty: the driver stores a percentage, so a
        // track over 0-255 would offer two hundred and fifty-six positions for
        // a hundred settings and most moves would change nothing.
        .range(f32::from(floor), f32::from(MAX_DUTY_PERCENT))
        .unit("%")
        .state(write.clone())
        .tab_index(tab_index);
        if let Some(sink) = sink {
            slider = slider.bounds_sink(sink);
        }

        let control = slider.render().when(enabled, |slider| {
            slider.on_key_down(
                cx.listener(move |shell, event: &gpui::KeyDownEvent, _, cx| {
                    let steps = match event.keystroke.key.as_str() {
                        "left" | "down" => -1,
                        "right" | "up" => 1,
                        "home" => i16::from(floor) - i16::from(percent),
                        "end" => i16::from(MAX_DUTY_PERCENT) - i16::from(percent),
                        _ => return,
                    };
                    shell.cooling.adjust_duty(channel, steps);
                    shell.schedule_write(WriteTarget::Cooling, cx);
                }),
            )
        });

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::XS)
            .child(control)
            .child(
                // What the caption cannot hold: the byte the device is actually
                // being told, and the floor this channel refuses to go under.
                // Under the track rather than beside it, so the track keeps the
                // width it needs to be aimed at.
                div()
                    .font(numeric_font())
                    .text_xs()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(format!(
                        "{target}/{MAX_DUTY} {META_SEPARATOR} accepted {floor} to \
                         {MAX_DUTY_PERCENT}%",
                    )),
            )
    }

    /// The curve of one channel and everything that drives it.
    fn curve_editor(
        &self,
        channel: Channel,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let nodes = *self.cooling.curve(channel);
        let node = self.cooling.node(channel);
        let editable = state.is_enabled();
        let refusal = state.message().map(str::to_string);
        // Published per render and read back by this window's press handler,
        // exactly as the two sliders publish theirs. Withheld when the plot
        // cannot be edited, which is what keeps a press from grabbing a curve
        // the hardware refused.
        let sink: Option<Rc<dyn Fn(Bounds<Pixels>)>> = editable.then(|| {
            let plots = Rc::clone(&self.curve_plots);
            Rc::new(move |bounds| plots.record(channel, bounds)) as Rc<dyn Fn(Bounds<Pixels>)>
        });
        let mut curve_editor = CurveEditor::new(nodes)
            .selected(node)
            .state(state.clone())
            .tab_index(tab_index);
        if let Some(sink) = sink {
            curve_editor = curve_editor.bounds_sink(sink);
        }
        let liquid_c = self
            .kraken()
            .and_then(|kraken| kraken.liquid_temperature_c.copied());

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::SM)
            .children(refusal.map(|reason| {
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(reason)
            }))
            .child(
                curve_editor
                    .liquid(liquid_c)
                    .render()
                    // The pointer part of the drag is not handled here. It is
                    // handled by the window, for the reason the brightness
                    // slider already is: a drag routinely leaves the control
                    // that started it, and a plot that only hears its own
                    // hitbox both stutters at the edge and never sees the
                    // release, which leaves the node stuck to the cursor.
                    .when(editable, |plot| {
                        plot.on_key_down(cx.listener(
                            move |shell, event: &gpui::KeyDownEvent, _, cx| {
                                let mut edited = false;
                                let handled = match event.keystroke.key.as_str() {
                                    "left" => {
                                        shell.cooling.step_node(channel, -1);
                                        true
                                    }
                                    "right" => {
                                        shell.cooling.step_node(channel, 1);
                                        true
                                    }
                                    "up" => {
                                        shell.cooling.adjust_node(channel, 1);
                                        edited = true;
                                        true
                                    }
                                    "down" => {
                                        shell.cooling.adjust_node(channel, -1);
                                        edited = true;
                                        true
                                    }
                                    _ => false,
                                };
                                if edited {
                                    shell.schedule_write(WriteTarget::Cooling, cx);
                                } else if handled {
                                    cx.notify();
                                }
                            },
                        ))
                    })
                    .into_any_element(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Point;
    use kori_core::profile::duty_from_percent;

    #[test]
    fn a_channel_row_names_the_program_the_device_reports_not_the_one_pending() {
        // The reported mode is the whole point of this line: after a mode
        // change it keeps naming the old program until the write lands, which
        // is how an operator sees that it has not.
        assert_eq!(
            reported_program(Some(PwmMode::Fixed), Some(70.6)),
            "Fixed duty 71%"
        );
        assert_eq!(
            reported_program(Some(PwmMode::Curve), None),
            "Onboard curve"
        );
        // The failsafe carries no percentage of its own: the duty printed
        // beside it is what the firmware is running, and the kernel's "100%
        // failsafe" wording routinely disagrees with that reading.
        assert_eq!(
            reported_program(Some(PwmMode::FullSpeed), Some(52.0)),
            "Firmware failsafe"
        );
        // A mode nothing answered for is said to be unreported rather than
        // shown as the safe-looking default, which would be a fabricated fact.
        assert_eq!(reported_program(None, Some(50.0)), "Mode not reported");
    }

    #[test]
    fn a_duty_track_spans_the_range_the_channel_actually_accepts() {
        // The pump refuses to stop, so its track starts at its floor instead of
        // carrying a fifth of its length that no press can reach. Both ends of
        // the track have to be values the daemon would take.
        for channel in [Channel::Pump, Channel::Fan] {
            let floor = duty_to_percent(channel.min_duty());
            let track = Bounds {
                origin: Point {
                    x: px(0.0),
                    y: px(0.0),
                },
                size: gpui::size(px(200.0), px(22.0)),
            };
            for (x, expected) in [(px(-40.0), floor), (px(400.0), MAX_DUTY_PERCENT)] {
                let percent = Slider::value_at(
                    track,
                    Point { x, y: px(10.0) },
                    f32::from(floor),
                    f32::from(MAX_DUTY_PERCENT),
                );
                let duty = duty_from_percent(percent.round() as u8);
                assert_eq!(duty_to_percent(duty), expected, "{channel:?} at {x:?}");
                assert!(duty >= channel.min_duty());
            }
        }
    }
}
