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
//! laid out on, the menu skin every popover wears, and the select that is the
//! only control all four use.

use std::rc::Rc;

use gpui::{Div, Pixels, SharedString, Stateful, div, prelude::*, px};

use kori_core::display::DisplayMode;

use crate::components::{ControlState, Select, SelectOption, focus_visible};
use crate::shell::Shell;
use crate::theme::{
    FOCUS_RING, MENU_MAX_HEIGHT, MENU_MAX_WIDTH, MENU_MIN_WIDTH, MENU_OFFSET, MENU_RADIUS,
    MENU_ROW_GAP, MENU_ROW_HEIGHT, RADIUS, color, space,
};
use gpui::Context;
use swatch::ColorPicker;

pub mod channel;
pub mod cooling;
pub mod drag;
pub mod lighting;
pub mod monitoring;
pub mod panel;
pub mod row;
pub mod settings;
pub mod swatch;
pub mod tab;
pub mod write;

/// Which popover, if any, is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popover {
    /// A color swatch list anchored to one color field.
    Swatches { picker: ColorPicker },
    /// An option list anchored to one select.
    Options { select: SharedString },
}
/// Whether a control shows the caption naming it.
///
/// A control in an open detail carries its own name, because nothing else
/// around it does. A control on a device row does not: the row already names
/// the device and the control is one of two on the line, so a caption over each
/// one is a second line of text that says what the first line said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caption {
    Shown,
    Hidden,
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

/// Modes the screen can configure completely.
///
/// [`DisplayMode::Image`] was absent while the screen had no control that could
/// name a file: a mode the daemon could only ever refuse is an entry that says
/// the feature is here and then does nothing. What was missing was never a text
/// input, which this codebase still has none of, but a file picker, and the
/// toolkit publishes the platform's own through `prompt_for_paths`.
pub const SCREEN_MODES: [DisplayMode; 4] = [
    DisplayMode::DualInfographic,
    DisplayMode::SingleReading,
    DisplayMode::Solid,
    DisplayMode::Image,
];

impl Shell {
    /// A select plus its popover, placed so it stays inside the window.
    #[allow(clippy::too_many_arguments)]
    fn select(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        caption: Caption,
        options: Vec<SelectOption>,
        selected: String,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, &str, &mut Context<Self>) + 'static,
    ) -> Div {
        // The identifier is owned rather than borrowed: a screen with one
        // select per channel has to name them apart, and the number of channels
        // is whatever the controller reported.
        let id: SharedString = id.into();
        // GPUI has no disabled semantics of its own: a handler left attached
        // still fires. Withholding it is what makes the refusal real rather
        // than a matter of styling.
        let enabled = state.is_enabled();
        let open = enabled && self.popover == Some(Popover::Options { select: id.clone() });
        let current = selected.clone();
        let mut built = Select::new(id.clone(), label);
        if caption == Caption::Hidden {
            built = built.label_hidden();
        }
        let control = built
            .options(options.clone())
            .selected(selected)
            .state(state)
            .tab_index(tab_index)
            .render()
            .when(enabled, |this| {
                let id = id.clone();
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_popover(Popover::Options { select: id.clone() }, cx)
                }))
            });

        let on_select = Rc::new(on_select);

        div().relative().child(control).when(open, |this| {
            this.child(option_menu(
                div()
                    .flex()
                    .flex_col()
                    .gap(MENU_ROW_GAP)
                    .children(options.into_iter().map(|option| {
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
                            .id(SharedString::from(format!("{id}-{}", option.value)))
                            .tab_index(tab_index)
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
                                this.popover = None;
                                cx.notify();
                            }))
                    })),
            ))
        })
    }
}
