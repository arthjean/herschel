// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Choosing a color, wherever the interface offers one.
//!
//! The panel's colors and a channel's differ in what choosing one writes and in
//! nothing else: the field, the swatch list, the enabled gate and the popover
//! are one control offered in two places. They were two functions whose bodies
//! matched line for line, so every change to how a color is picked had two
//! places to land and one of them to be forgotten.

use gpui::{AnyElement, Div, SharedString, Stateful, div, prelude::*};

use crate::components::{ColorField, ControlState, focus_ring};
use crate::display::DisplayColorField;
use crate::shell::Shell;
use crate::theme::{Color, SWATCH_RADIUS, SWATCH_SIZE, color, space};
use gpui::Context;

use super::row::LightingRow;
use super::write::WriteTarget;
use super::{Popover, menu_surface, popover_surface};

/// Which color a field edits.
///
/// The panel's colors and a channel's differ in what choosing one writes and in
/// nothing else: the field, the swatch list, the enabled gate and the popover
/// are one control offered in two places. Naming the difference here is what
/// lets there be one builder rather than two that have to be kept identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorPicker {
    /// One of the panel's colors.
    Panel(DisplayColorField),
    /// One lighting channel's fixed color.
    Channel(u8),
}
impl ColorPicker {
    /// Stable identifier for the control and the swatches under it.
    fn id(self) -> SharedString {
        match self {
            Self::Panel(field) => SharedString::from(format!("lcd-color-{}", field.key())),
            Self::Channel(channel) => SharedString::from(format!("lighting-color-{channel}")),
        }
    }

    /// The caption above the field.
    ///
    /// The panel's fields carry their whole name because nothing beside them
    /// says which slot they belong to; a channel's field sits under a row that
    /// already names the device.
    fn label(self) -> SharedString {
        match self {
            Self::Panel(field) => SharedString::from(field.label()),
            Self::Channel(_) => SharedString::from("Color"),
        }
    }

    /// The row whose quiet period a choice here waits out.
    fn row(self) -> LightingRow {
        match self {
            Self::Panel(_) => LightingRow::Lcd,
            Self::Channel(channel) => LightingRow::Channel(channel),
        }
    }
}
/// Swatches offered by a color popover.
pub const SWATCHES: [Color; 6] = [
    Color::rgb(0x6f4ef2),
    Color::rgb(0x30c8a0),
    Color::rgb(0xf5c451),
    Color::rgb(0xff8a8a),
    Color::rgb(0xe8eaee),
    Color::rgb(0x14161a),
];
/// How many swatches a color list puts on one line.
pub const SWATCH_COLUMNS: usize = 3;
/// One offered color, without what choosing it does.
///
/// Shared by the two lists that offer colors, so a swatch in the panel's list
/// and one in a channel's are the same object rather than two that happen to
/// match today.
fn swatch_cell(id: String, swatch: Color, tab_index: isize) -> Stateful<Div> {
    // Nothing outlines the color at rest: the swatch is the color and an edge
    // around it is one more thing to read.
    focus_ring(
        div()
            .id(SharedString::from(id))
            .tab_index(tab_index)
            .tab_stop(true)
            .flex_none()
            .w(SWATCH_SIZE)
            .h(SWATCH_SIZE)
            .rounded(SWATCH_RADIUS)
            .cursor_pointer(),
        true,
    )
    .bg(swatch.hsla())
    .hover(|this| this.border_color(color::FOCUS.alpha(0.6)))
}
/// The offered colors, [`SWATCH_COLUMNS`] to a line.
///
/// Laid out in fixed rows rather than wrapped on the available width, so the
/// shape of the list is the same wherever it opens and however wide the menu
/// around it happens to be.
fn swatch_grid(mut swatches: Vec<AnyElement>) -> Div {
    let mut rows = Vec::new();
    while !swatches.is_empty() {
        let taken = SWATCH_COLUMNS.min(swatches.len());
        let line: Vec<AnyElement> = swatches.drain(..taken).collect();
        rows.push(div().flex().gap(space::SM).children(line));
    }
    div().flex().flex_col().gap(space::SM).children(rows)
}
/// A list of swatches: the same skin, sized to the grid inside it.
///
/// The clamp of [`option_menu`] would leave the menu wider than its own
/// content, which is empty surface beside the colors. Paneflow makes the same
/// split, for the same reason: a menu that must size to its content takes the
/// skin without the container.
fn swatch_menu(content: impl IntoElement) -> impl IntoElement {
    popover_surface(menu_surface(content))
}

impl Shell {
    /// A color field plus the swatch list it opens.
    ///
    /// One builder for the panel's colors and the controller's. They were two
    /// functions whose bodies matched line for line, down to the comments, and
    /// differed only in which popover they owned and what a chosen swatch
    /// wrote, so every change to how a color is picked had two places to land
    /// and one of them to be forgotten.
    ///
    /// The field shows the digits as typed, so an entry the operator has not
    /// finished keeps its own error rather than being replaced by a value it
    /// never held. It carries the device's refusal too: choosing a swatch is
    /// what sends the command, so a picker that opens over hardware nothing can
    /// be written to would be a control that acts and changes nothing.
    pub(crate) fn color_picker(
        &self,
        picker: ColorPicker,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        // GPUI has no disabled semantics of its own: a handler left attached
        // still fires, so withholding it is what makes the refusal real rather
        // than a matter of styling.
        let enabled = state.is_enabled();
        let open = enabled && self.popover == Some(Popover::Swatches { picker });
        let id = picker.id();

        // `ColorField` renders its own error state, so a stored color that
        // cannot be parsed names the problem on the field itself. A refused
        // capability outranks it: correcting the digits would not make the
        // write happen.
        let control = ColorField::new(id.clone(), picker.label(), self.color_text(picker))
            .state(state)
            .tab_index(tab_index)
            .render()
            .when(enabled, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_popover(Popover::Swatches { picker }, cx)
                }))
            });

        div().relative().child(control).when(open, |this| {
            this.child(swatch_menu(swatch_grid(
                SWATCHES
                    .into_iter()
                    .enumerate()
                    .map(|(index, swatch)| {
                        swatch_cell(format!("{id}-swatch-{index}"), swatch, tab_index)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_color(picker, swatch, cx)
                            }))
                            .into_any_element()
                    })
                    .collect(),
            )))
        })
    }

    /// The digits currently in one picker's field.
    fn color_text(&self, picker: ColorPicker) -> String {
        match picker {
            ColorPicker::Panel(field) => self.lcd.editor().color_text(field).to_string(),
            ColorPicker::Channel(channel) => self
                .lighting
                .channel(channel)
                .map(|editor| editor.color.clone())
                .unwrap_or_default(),
        }
    }

    /// Put a chosen color in one picker's field and schedule what it implies.
    ///
    /// Closing the list is part of choosing: leaving it open over the control
    /// it just changed would hide the result of the press.
    fn choose_color(&mut self, picker: ColorPicker, swatch: Color, cx: &mut Context<Self>) {
        let digits = format!("{:06X}", swatch.0);
        match picker {
            ColorPicker::Panel(field) => {
                self.lcd.edit(|editor| editor.set_color_text(field, digits))
            }
            ColorPicker::Channel(channel) => {
                let Some(editor) = self.lighting.channel_mut(channel) else {
                    return;
                };
                editor.color = digits;
            }
        }
        self.popover = None;
        self.schedule_write(WriteTarget::Lighting(picker.row()), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swatches_are_distinct() {
        let mut values: Vec<u32> = SWATCHES.iter().map(|color| color.0).collect();
        values.sort();
        values.dedup();
        assert_eq!(values.len(), SWATCHES.len());
    }
}
