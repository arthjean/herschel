// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The four destinations, and the vocabulary they share.
//!
//! One module per screen, each holding the state that screen owns and the
//! `impl Shell` block that draws it. The shell itself keeps the frame: the
//! rail, the window chrome, the focus model and the one write schedule every
//! screen queues into.
//!
//! What lives here is what more than one screen needs: the surfaces a screen is
//! laid out on, the menu skin every popover wears, which rows each screen has
//! open, and the select that is the only control all four use.

use std::rc::Rc;

use gpui::{Div, Pixels, SharedString, Stateful, div, prelude::*, px};

use kori_core::profile::{CURVE_NODE_COUNT, Channel};

use crate::components::{ControlState, Select, SelectOption, focus_visible};
use crate::shell::Shell;
use crate::theme::{
    FOCUS_RING, MENU_MAX_HEIGHT, MENU_MAX_WIDTH, MENU_MIN_WIDTH, MENU_OFFSET, MENU_RADIUS,
    MENU_ROW_GAP, MENU_ROW_HEIGHT, RADIUS, color, space,
};
use gpui::Context;
use keyed::{Keyed, Set};
use row::LightingRow;
use swatch::ColorPicker;
use tab::MENU_TAB_BASE;

pub mod channel;
pub mod cooling;
pub mod drag;
pub mod keyed;
pub mod lighting;
pub mod monitoring;
pub mod panel;
pub mod row;
pub mod settings;
pub mod swatch;
pub mod tab;
pub mod write;

/// Which popover, if any, is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popover {
    /// A color swatch list anchored to one color field.
    Swatches { picker: ColorPicker },
    /// An option list anchored to one select.
    Options { select: SelectId },
}

/// Which select a control and the list it opens are.
///
/// Typed rather than a string built at each call site. The popover is keyed on
/// this, and the swatch popover next door was already typed on [`ColorPicker`]
/// while this one compared a `SharedString` the caller spelled by hand: a
/// mismatch between the id given to the control and the id compared against the
/// open popover is invisible, because the list simply never opens.
///
/// It carries the caption rule too. Whether a select shows its name is a
/// property of which select it is rather than a flag threaded through the
/// builder: the two that sit on a device row are named by the row they are on,
/// and every other one names itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectId {
    CoolingMode,
    Profile,
    ChannelMode(u8),
    ChannelSpeed(u8),
    ChannelDirection(u8),
    LcdMode,
    /// One of the panel's reading slots, by index.
    LcdMetric(usize),
}

impl SelectId {
    /// Stable fragment for the element ids this control and its rows carry.
    pub fn key(self) -> SharedString {
        match self {
            Self::CoolingMode => "cooling-mode".into(),
            Self::Profile => "profile".into(),
            Self::ChannelMode(channel) => format!("lighting-mode-{channel}").into(),
            Self::ChannelSpeed(channel) => format!("lighting-speed-{channel}").into(),
            Self::ChannelDirection(channel) => format!("lighting-direction-{channel}").into(),
            Self::LcdMode => "lcd-mode".into(),
            Self::LcdMetric(slot) => format!("lcd-metric-{}", slot + 1).into(),
        }
    }

    /// The caption above the control, which an error message also names.
    pub fn label(self) -> SharedString {
        match self {
            Self::CoolingMode | Self::ChannelMode(_) => "Mode".into(),
            Self::Profile => "Active profile".into(),
            Self::ChannelSpeed(_) => "Speed".into(),
            Self::ChannelDirection(_) => "Direction".into(),
            Self::LcdMode => "Display mode".into(),
            Self::LcdMetric(slot) => format!("Reading {}", slot + 1).into(),
        }
    }

    /// Whether the caption is drawn above the control.
    ///
    /// A control in an open detail carries its own name, because nothing else
    /// around it does. A control on a device row does not: the row already
    /// names the device and the control is one of two on the line, so a caption
    /// over each one is a second line of text saying what the first line said.
    fn shows_label(self) -> bool {
        !matches!(self, Self::ChannelMode(_) | Self::LcdMode)
    }
}

/// Everything one select needs beyond what its [`SelectId`] already says.
///
/// One value rather than six more positional arguments. Four of them were a
/// `Vec`, a `String`, a state and a number, which is exactly the set a call site
/// can reorder without the compiler noticing, and naming them is also what took
/// the builder back inside the argument budget: it carried a
/// `#[allow(clippy::too_many_arguments)]` for nine.
pub(crate) struct SelectField {
    pub(crate) id: SelectId,
    pub(crate) options: Vec<SelectOption>,
    pub(crate) selected: String,
    pub(crate) state: ControlState,
    pub(crate) tab_index: isize,
}

/// Which rows each screen has open, and where the keyboard sits on each plot.
///
/// View state, held by the shell rather than by the editors. It used to be
/// split: the Lighting screen's open rows were a field of `Shell` and the
/// Cooling screen's were inside `CoolingEditor`, alongside the duties that
/// editor writes to hardware and the curve node the keyboard is on. Neither is
/// an edit and neither is ever sent, so the same category of state lived in two
/// layers and one of the editors could not be read as "what this screen would
/// send".
#[derive(Debug, Default)]
pub struct Disclosure {
    /// Cooling channels whose rows are open, each editable on its own.
    ///
    /// Rows open independently: each plot publishes its own rectangle and
    /// carries its own selected node, so two open at once edit two curves
    /// rather than one meaning two things.
    pub cooling: Set<Channel>,
    /// Lighting rows whose controls are revealed, on the same terms.
    ///
    /// A controller with three channels and a panel is four appearances of one
    /// machine, and comparing two of them means having both on screen.
    pub lighting: Set<LightingRow>,
    node: Keyed<Channel, usize>,
}

impl Disclosure {
    /// The curve node the keyboard is on for `channel`.
    ///
    /// One per channel rather than one for the screen: with both rows open, a
    /// shared index would move a node on whichever plot was focused last.
    pub fn node(&self, channel: Channel) -> usize {
        self.node.get(channel).copied().unwrap_or(0)
    }

    pub fn select_node(&mut self, channel: Channel, index: usize) {
        self.node.set(channel, index.min(CURVE_NODE_COUNT - 1));
    }

    /// Move the selection along the curve, staying on the plot.
    pub fn step_node(&mut self, channel: Channel, delta: isize) {
        let next = self.node(channel) as isize + delta;
        self.select_node(
            channel,
            next.clamp(0, CURVE_NODE_COUNT as isize - 1) as usize,
        );
    }

    /// Open a cooling row, or close it, putting the keyboard back on the first
    /// node of the plot it reveals.
    pub fn toggle_cooling(&mut self, channel: Channel) {
        self.cooling.toggle(channel);
        self.select_node(channel, 0);
    }
}

/// Width of one field in an open row's detail, two of which fit side by side
/// in the column left of the preview.
pub const FIELD_WIDTH: Pixels = px(168.0);

/// The standard heading and column of a destination.
fn screen(title: &'static str, subtitle: &'static str) -> Div {
    div().flex().flex_col().gap(space::LG).w_full().child(
        div()
            .flex()
            .flex_col()
            .gap(space::XS)
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(color::TEXT.hsla())
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(subtitle),
            ),
    )
}
/// One line of the panel editor's grid.
///
/// Each control keeps the same fixed width whatever it holds, so the second
/// column starts at the same offset on every line and a line that carries one
/// control leaves the other column empty instead of stretching across it.
fn field_line(controls: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap(space::MD).children(
        controls
            .into_iter()
            .map(|control| div().flex_none().w(FIELD_WIDTH).child(control)),
    )
}
/// A row of metric tiles that wraps rather than scrolling sideways.
fn metric_row() -> Div {
    div().flex().flex_wrap().gap(space::XL).w_full().min_w_0()
}
/// One recorded fact of the diagnostics screen: what it is, and what it says.
///
/// Both sides are taken as `SharedString` rather than as two functions, one for
/// a literal label and one for a built one: the device rows name themselves
/// from the capability record, and a second entry point whose only work was a
/// `to_string` is indirection the caller has to choose between.
fn setting_row(label: impl Into<SharedString>, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .justify_between()
        .gap(space::LG)
        .py(space::XS)
        .child(
            div()
                .text_color(color::TEXT_MUTED.hsla())
                .flex_none()
                .child(label.into()),
        )
        .child(
            div()
                .text_color(color::TEXT.hsla())
                .flex_1()
                .text_align(gpui::TextAlign::Right)
                .child(value.into()),
        )
}
/// A list of options: the menu skin plus the width clamp a row of text needs.
///
/// Rows are as wide as the menu, so without the floor a menu of short options
/// would be a sliver and one long option would set the width for every other.
fn option_menu(content: impl IntoElement) -> impl IntoElement {
    popover_surface(menu_surface(content).min_w(MENU_MIN_WIDTH))
}
/// The menu skin: radius, lifted surface, hairline, and the geometry every menu
/// shares. What sets the width stays with the caller.
fn menu_surface(content: impl IntoElement) -> Stateful<Div> {
    div()
        .id("popover-surface")
        // The list keeps the presses that land on it. Without this the
        // dismissal layer under it would also hear the press, close the
        // list on the way down, and the option the operator was
        // pressing would be gone before the release reached it.
        .occlude()
        .flex()
        .flex_col()
        .gap(MENU_ROW_GAP)
        .max_w(MENU_MAX_WIDTH)
        .max_h(MENU_MAX_HEIGHT)
        .overflow_y_scroll()
        .p(space::XS)
        .rounded(MENU_RADIUS)
        // Lifted surface and a hairline at 0.6, no drop shadow: the menu is
        // told apart from the panel by its luminance, which is how Paneflow's
        // menus read in front without casting anything.
        .bg(color::MENU.hsla())
        .border_1()
        .border_color(color::SEPARATOR.alpha(0.6))
        .child(content)
}
/// Float a built menu over the screen.
///
/// `anchored` is what keeps a menu opened near a window edge fully visible: it
/// repositions itself rather than being clipped. `deferred` paints it above
/// panels laid out after it, so a menu is never covered by its neighbor.
fn popover_surface(menu: Stateful<Div>) -> impl IntoElement {
    gpui::deferred(
        gpui::anchored()
            // Off the control rather than against it. Applied before the snap,
            // so a menu opened near the bottom edge still folds back into the
            // window instead of being pushed out of it by the offset.
            .offset(gpui::point(px(0.0), MENU_OFFSET))
            .snap_to_window_with_margin(px(8.0))
            .child(menu),
    )
    .with_priority(1)
}

impl Shell {
    /// A select plus its popover, placed so it stays inside the window.
    fn select(
        &self,
        field: SelectField,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> Div {
        let SelectField {
            id,
            options,
            selected,
            state,
            tab_index,
        } = field;
        let key = id.key();
        // GPUI has no disabled semantics of its own: a handler left attached
        // still fires. Withholding it is what makes the refusal real rather
        // than a matter of styling.
        let enabled = state.is_enabled();
        let open = enabled && self.interaction.showing(&Popover::Options { select: id });
        let current = selected.clone();

        let mut built = Select::new(key.clone(), id.label());
        if !id.shows_label() {
            built = built.label_hidden();
        }
        let control = built
            .options(options.clone())
            .selected(selected)
            .state(state)
            .tab_index(tab_index)
            .render()
            .when(enabled, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_popover(Popover::Options { select: id }, cx)
                }))
            });

        let on_select = Rc::new(on_select);

        div().relative().child(control).when(open, |this| {
            this.child(option_menu(
                div().flex().flex_col().gap(MENU_ROW_GAP).children(
                    options.into_iter().enumerate().map(|(index, option)| {
                        let value = option.value.clone();
                        let chosen = option.value == current;
                        let on_select = Rc::clone(&on_select);
                        // Whisper highlights, the selected row a step stronger
                        // than a hovered one: a menu that marks its current
                        // value with the selection accent reads as if choosing
                        // had already happened.
                        let resting = if chosen {
                            color::TEXT.alpha(0.10)
                        } else {
                            color::TEXT.alpha(0.0)
                        };
                        let hovered = if chosen {
                            color::TEXT.alpha(0.10)
                        } else {
                            color::TEXT.alpha(0.05)
                        };
                        div()
                            .id(SharedString::from(format!("{key}-{}", option.value)))
                            // Its own stop in the reserved menu range. Every row
                            // used to take the trigger's index, which is the
                            // invariant `tab.rs` asserts broken in the one place
                            // its tests do not reach.
                            .tab_index(MENU_TAB_BASE + index as isize)
                            .tab_stop(true)
                            .flex_none()
                            .w_full()
                            .h(MENU_ROW_HEIGHT)
                            .flex()
                            .items_center()
                            .gap(space::SM)
                            .px(space::SM)
                            .rounded(RADIUS)
                            .cursor_pointer()
                            .text_xs()
                            .text_color(color::TEXT.hsla())
                            .bg(resting)
                            .hover(|this| this.bg(hovered))
                            .when(focus_visible(), |this| {
                                this.focus(|this| {
                                    this.border(FOCUS_RING).border_color(color::FOCUS.hsla())
                                })
                            })
                            .child(div().flex_1().min_w_0().truncate().child(option.label))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                on_select(this, &value, cx);
                                this.interaction.dismiss();
                                cx.notify();
                            }))
                    }),
                ),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is the popover key, so two selects that could ever be on screen
    /// together must not answer to the same one: a collision opens the wrong
    /// list, or the right one under the wrong control.
    #[test]
    fn every_select_on_a_screen_carries_its_own_identity() {
        let ids = [
            SelectId::CoolingMode,
            SelectId::Profile,
            SelectId::ChannelMode(1),
            SelectId::ChannelMode(2),
            SelectId::ChannelSpeed(1),
            SelectId::ChannelDirection(1),
            SelectId::LcdMode,
            SelectId::LcdMetric(0),
            SelectId::LcdMetric(1),
        ];

        let mut keys: Vec<String> = ids.iter().map(|id| id.key().to_string()).collect();
        let spelled = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), spelled, "two selects share an element id");

        for id in ids {
            assert!(!id.label().is_empty(), "{id:?} has no caption to show");
        }
    }

    /// The caption is a property of where the control sits. The two selects on
    /// a device row are named by the row; every other one names itself.
    #[test]
    fn only_the_selects_a_row_already_names_hide_their_caption() {
        assert!(!SelectId::ChannelMode(1).shows_label());
        assert!(!SelectId::LcdMode.shows_label());
        for id in [
            SelectId::CoolingMode,
            SelectId::Profile,
            SelectId::ChannelSpeed(1),
            SelectId::ChannelDirection(1),
            SelectId::LcdMetric(0),
        ] {
            assert!(id.shows_label(), "{id:?} has nothing else naming it");
        }
    }

    #[test]
    fn a_reading_slot_is_numbered_from_one_wherever_it_is_named() {
        assert_eq!(SelectId::LcdMetric(0).label(), "Reading 1");
        assert_eq!(SelectId::LcdMetric(1).label(), "Reading 2");
        assert_eq!(SelectId::LcdMetric(0).key(), "lcd-metric-1");
        assert_eq!(SelectId::LcdMetric(1).key(), "lcd-metric-2");
    }

    #[test]
    fn opening_a_cooling_row_puts_the_keyboard_on_its_first_node() {
        let mut rows = Disclosure::default();
        assert!(!rows.cooling.contains(Channel::Pump), "rows start closed");

        rows.step_node(Channel::Pump, 4);
        assert_eq!(rows.node(Channel::Pump), 4);

        rows.toggle_cooling(Channel::Pump);
        assert!(rows.cooling.contains(Channel::Pump));
        assert_eq!(
            rows.node(Channel::Pump),
            0,
            "the plot opens on its first node"
        );

        // Each channel keeps its own selection: with both rows open, walking
        // the nodes of one plot must not move the point selected on the other.
        rows.step_node(Channel::Pump, 3);
        assert_eq!(rows.node(Channel::Fan), 0, "the fan kept its own node");

        rows.toggle_cooling(Channel::Pump);
        assert!(
            !rows.cooling.contains(Channel::Pump),
            "a second press closes it"
        );
    }

    #[test]
    fn node_selection_stays_inside_the_curve() {
        let mut rows = Disclosure::default();
        rows.step_node(Channel::Pump, -5);
        assert_eq!(rows.node(Channel::Pump), 0);
        rows.step_node(Channel::Pump, 100);
        assert_eq!(rows.node(Channel::Pump), CURVE_NODE_COUNT - 1);
        rows.select_node(Channel::Pump, 999);
        assert_eq!(rows.node(Channel::Pump), CURVE_NODE_COUNT - 1);
    }

    #[test]
    fn lighting_rows_open_independently_of_each_other() {
        let mut rows = Disclosure::default();
        rows.lighting.insert(LightingRow::Lcd);

        rows.lighting.toggle(LightingRow::Channel(1));
        assert!(rows.lighting.contains(LightingRow::Channel(1)));
        assert!(
            rows.lighting.contains(LightingRow::Lcd),
            "opening one row does not close another"
        );

        rows.lighting.toggle(LightingRow::Lcd);
        assert!(!rows.lighting.contains(LightingRow::Lcd));
        assert!(rows.lighting.contains(LightingRow::Channel(1)));
    }
}
