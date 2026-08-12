// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The liquid-temperature curve editor and the plot it draws.
//!
//! Editing here changes nothing on the device: the surface is a pure view of
//! the pending curve, and what turns it into a write happens elsewhere. The
//! plot, its two axes and the marker showing where the coolant currently sits
//! are one object, so the mapping between a node and a pixel is stated once and
//! read by both the painter and the hit test.

use std::rc::Rc;

use gpui::{
    Bounds, Div, Hsla, PathBuilder, Pixels, Point, Stateful, Window, canvas, div, prelude::*, px,
};

use kori_core::profile::{
    CURVE_NODE_COUNT, CurveNodes, MAX_DUTY_PERCENT, duty_from_percent, duty_to_percent,
};

use crate::theme::{DEGREE_C, FOCUS_RING, META_SEPARATOR, RADIUS, color, numeric_font, space};

use super::{ControlState, focus_visible, stroke_line};

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
    /// Where to publish the plot area, so a press can be mapped onto a node.
    bounds_sink: Option<Rc<dyn Fn(Bounds<Pixels>)>>,
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

    /// Where to publish the plot area, for hit testing.
    ///
    /// The same shape the slider publishes through, and withheld for the same
    /// reason: a plot that cannot be edited records nothing, so a press cannot
    /// grab a curve the hardware refused.
    pub fn bounds_sink(mut self, sink: Rc<dyn Fn(Bounds<Pixels>)>) -> Self {
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
                        sink(area);
                    }
                    paint_curve(window, area, &nodes, selected, line_color, marker_color);
                    paint_liquid_marker(window, area, liquid_c, &nodes);
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
            .child(curve_caption(&nodes, selected, liquid_c))
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
/// Opacity of the envelope drawn under the curve.
///
/// Faint enough that the grid and its labels stay readable through it. The fill
/// is there to give the curve a quantity, not to become the plot's background.
const CURVE_AREA_ALPHA: f32 = 0.14;

/// What the plot is showing, in words, above the plot itself.
///
/// Two facts a polyline cannot state: the exact value of the node the keyboard
/// is on, which a pointer user reads off the axes but a keyboard user cannot,
/// and the duty the curve is commanding at the coolant temperature right now.
/// The second is what turns the plot from a drawing into a readout: the
/// operator can see the program they wrote and the point of it in force at the
/// same time.
fn curve_caption(nodes: &CurveNodes, selected: usize, liquid_c: Option<f32>) -> Div {
    let node = format!(
        "Node {:.0}{DEGREE_C} {META_SEPARATOR} {}%",
        CurveNodes::temperature_at(selected),
        duty_to_percent(nodes.duty[selected.min(CURVE_NODE_COUNT - 1)])
    );
    let coolant = liquid_c.map(|celsius| {
        match nodes.interpolate().duty_at(celsius) {
            Some(duty) => format!(
                "Coolant {celsius:.1}{DEGREE_C} {META_SEPARATOR} curve calls {}%",
                duty_to_percent(duty)
            ),
            // A reading with no duty behind it is still worth stating: it is
            // what the curve is steered by, and inventing a percentage for it
            // would be worse than leaving the sentence short.
            None => format!("Coolant {celsius:.1}{DEGREE_C}"),
        }
    });

    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .justify_between()
        .gap(space::SM)
        .w_full()
        .min_w_0()
        .text_xs()
        .font(numeric_font())
        .child(div().flex_none().text_color(color::TEXT.hsla()).child(node))
        .children(coolant.map(|coolant| {
            div()
                .flex_none()
                .text_color(color::TEXT_MUTED.hsla())
                .child(coolant)
        }))
}

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
fn plot_node(index: usize, duty: u8, bounds: Bounds<Pixels>) -> Point<Pixels> {
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

    // The envelope under the line, so a curve reads as an amount of cooling
    // rather than as a bare polyline. Painted over the grid rather than under
    // it: at this opacity the grid still shows through, and drawing it first
    // would put the fill's own edge behind the lines that bound the plot.
    let mut area_color = line_color;
    area_color.a = CURVE_AREA_ALPHA;
    let floor = bounds.origin.y + bounds.size.height;
    let mut area = PathBuilder::fill();
    area.move_to(Point {
        x: bounds.origin.x,
        y: floor,
    });
    for (index, duty) in nodes.duty.iter().enumerate() {
        area.line_to(plot_node(index, *duty, bounds));
    }
    area.line_to(Point {
        x: bounds.origin.x + bounds.size.width,
        y: floor,
    });
    area.close();
    if let Ok(path) = area.build() {
        window.paint_path(path, area_color);
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

/// Where the coolant currently sits on the temperature axis, and what the curve
/// commands there.
///
/// Drawn only when the reading falls inside the range the curve covers: a
/// marker pinned to an edge would claim a temperature the plot cannot show.
///
/// Dashed rather than solid, because everything else on the plot is an
/// intention and this one line is a measurement. The dot where it meets the
/// curve is the duty in force right now, taken from the same interpolation the
/// daemon would write, so the plot shows the program and its current effect
/// without the operator converting a temperature into a percentage by eye.
fn paint_liquid_marker(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    liquid_c: Option<f32>,
    nodes: &CurveNodes,
) {
    let Some(celsius) = liquid_c else { return };
    let first = CurveNodes::temperature_at(0);
    let last = CurveNodes::temperature_at(CURVE_NODE_COUNT - 1);
    if celsius < first || celsius > last {
        return;
    }

    let across = (celsius - first) / (last - first);
    let x = bounds.origin.x + bounds.size.width * across;
    let mut builder = PathBuilder::stroke(px(1.0)).dash_array(&[px(3.0), px(4.0)]);
    builder.move_to(Point {
        x,
        y: bounds.origin.y,
    });
    builder.line_to(Point {
        x,
        y: bounds.origin.y + bounds.size.height,
    });
    if let Ok(path) = builder.build() {
        window.paint_path(path, color::TEXT_MUTED.alpha(0.8));
    }

    let Some(duty) = nodes.interpolate().duty_at(celsius) else {
        return;
    };
    let center = Point {
        x,
        y: bounds.origin.y + bounds.size.height * (1.0 - duty as f32 / 255.0),
    };
    // Ringed in the surface behind the plot so it is legible wherever it lands
    // on the line, and filled in the text color rather than the accent: the
    // accent is what the operator drew, and this dot is what is happening.
    paint_dot(window, center, px(5.5), color::SURFACE.hsla());
    paint_dot(window, center, px(3.5), color::TEXT.hsla());
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

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
}
