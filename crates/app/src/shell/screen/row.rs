// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! One device row of the Lighting screen: the line, and what it opens.
//!
//! The channels and the panel are the same object with different contents, so
//! the frame is stated once here. It used to be written twice, down to the same
//! four comments word for word, and a layout fact restated in two places is one
//! that only holds until somebody edits one of them.

use std::rc::Rc;

use gpui::{Bounds, Div, Pixels, SharedString, Stateful, div, prelude::*, px};

use crate::assets::Icon;
use crate::components::{ControlState, Slider, chevron, focus_visible, icon};
use crate::shell::Shell;
use crate::theme::{
    Color, FOCUS_RING, META_SEPARATOR, RADIUS, ROW_RADIUS, TARGET_MIN, color, space,
};
use gpui::Context;

use super::tab::ROW_OFFSET_BRIGHTNESS;
use super::write::WriteTarget;

/// One openable row of the Lighting screen.
///
/// The panel is a row here rather than a destination of its own: it is one
/// device's appearance, configured next to the controller's channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingRow {
    Channel(u8),
    Lcd,
}
impl LightingRow {
    /// Stable fragment for the element ids this row's controls carry.
    pub fn key(self) -> String {
        match self {
            Self::Channel(channel) => format!("channel-{channel}"),
            Self::Lcd => "lcd".to_string(),
        }
    }
}
/// The second fact a device row carries, if it has one.
///
/// A fragment rides on the name's own line, muted and after a separator, and
/// leaves the row one line tall. A sentence takes a line of its own. Which shape
/// a row uses is a property of what it has to say rather than of the row: a
/// channel's is "Channel 1", and a panel's is a mode, an orientation and a
/// brightness.
///
/// One value rather than two optional strings, so a row cannot ask for both and
/// end up back at the two lines the fragment exists to avoid.
pub(crate) enum RowNote {
    Fragment(String),
    Sentence(String),
}
/// Everything one device row's line carries.
///
/// One value rather than seven positional arguments on [`Shell::device_row`].
/// Four of them are a `Div`, a `String` and two numbers, which is exactly the
/// set a call site can reorder without the compiler noticing, and naming them
/// is also what keeps the row builder inside the argument budget.
pub(crate) struct RowLine {
    pub(crate) thumbnail: Div,
    pub(crate) title: String,
    pub(crate) note: Option<RowNote>,
    pub(crate) brightness: u8,
    /// Gate carried by the brightness slider: what the row writes with.
    pub(crate) write: ControlState,
    /// The row's own mode select, already built. It is the one control whose
    /// options and handler are specific to the device.
    pub(crate) mode: Div,
    /// First stop of this row's reserved block.
    pub(crate) tab_index: isize,
}
/// Width of the two controls a device row carries on its right side.
///
/// Sized so the head of the row, the stepper and the mode select fit one line
/// at the 920-pixel target: the row wraps below that rather than clipping, but
/// it must not wrap at the size the layout is designed for.
pub const ROW_BRIGHTNESS_WIDTH: Pixels = px(176.0);
pub const ROW_MODE_WIDTH: Pixels = px(156.0);
/// Smallest the head of a row may become before the line wraps.
pub const ROW_HEAD_MIN_WIDTH: Pixels = px(180.0);
/// Left inset of an open row's detail.
///
/// Lines up with what follows the chevron on the line above: the head's own
/// padding, the chevron, and the gap after it. A detail that starts under the
/// chevron reads as another row rather than as the inside of this one.
pub const ROW_DETAIL_INDENT: Pixels = px(32.0);
/// Side of the appearance thumbnail at the head of a device row.
pub const ROW_THUMBNAIL: Pixels = px(34.0);
/// Side of the glyph drawn inside that thumbnail.
///
/// Larger than [`ICON_SIZE`], which is the size of a glyph sitting beside text.
/// This one sits inside a filled tile and has to survive the fill around it, so
/// it takes a little under two thirds of the side, leaving a margin of the
/// color on every edge.
pub const ROW_THUMBNAIL_GLYPH: Pixels = px(20.0);
/// The appearance thumbnail at the head of a device row.
///
/// The fill is still the color, because that is the one thing on the collapsed
/// line that says what the channel is pending. The glyph on top says what the
/// row drives, which a bare rectangle never did: a list of identical squares
/// separated only by hue reads as a palette rather than as hardware.
///
/// `None` is a color the row cannot show: a channel that is off, or an entry
/// the operator has not finished typing. It paints the empty surface rather
/// than a black swatch, because black is also a color a channel can be set to,
/// and dims the glyph to match, so an unset row is quiet rather than absent.
///
/// The ink is chosen against the fill rather than fixed: a white mark vanishes
/// on a pale yellow and a dark one vanishes on the black the panel starts at.
/// [`Color::readable_ink`] carries the measurement.
pub(crate) fn row_thumbnail(color_value: Option<Color>, glyph: Icon, round: bool) -> Div {
    let (fill, ink) = match color_value {
        Some(value) => (value, value.readable_ink()),
        None => (color::SURFACE, color::TEXT_DISABLED),
    };
    div()
        .flex_none()
        .w(ROW_THUMBNAIL)
        .h(ROW_THUMBNAIL)
        .flex()
        .items_center()
        .justify_center()
        .rounded(if round { ROW_THUMBNAIL / 2.0 } else { RADIUS })
        .bg(fill.hsla())
        .child(icon(glyph, ROW_THUMBNAIL_GLYPH, ink.hsla()))
}

impl Shell {
    /// The head of a device row: chevron, thumbnail and two lines of text, as
    /// one target that opens the row.
    ///
    /// Only this part of the line toggles. The brightness and mode controls sit
    /// beside it rather than inside it, so operating one of them cannot also
    /// collapse the row it belongs to.
    ///
    /// `note` is the second fact the row carries, if it has one. See
    /// [`RowNote`] for which of the two shapes to give it.
    /// Whether one row of the Lighting screen is open.
    pub(crate) fn is_open(&self, row: LightingRow) -> bool {
        self.lighting_open.contains(&row)
    }

    /// One device row: the line, and whatever it revealed.
    ///
    /// Written once for the channels and the panel. The two used to state this
    /// scaffold separately, down to the same four comments word for word, and
    /// differed only in the head they carried, the mode select they offered and
    /// the detail they opened. A layout fact restated in two places is one that
    /// only holds until somebody edits one of them.
    pub(crate) fn device_row(
        &self,
        row: LightingRow,
        line: RowLine,
        detail: Option<Div>,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = detail.is_some();
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
            // Open is a state the whole row carries, not a stack of two
            // elements that happen to touch: the line and what it revealed sit
            // on one fill, so the detail is read as the inside of this device
            // rather than as the top of the next one. Held at every state, so
            // opening a row does not move the line that opened it.
            .when(open, |this| this.bg(color::CONTROL.alpha(0.25)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    // Centered because the controls carry no caption above
                    // them: with a label they hung off a shared baseline, and
                    // without one they are boxes of equal height beside a
                    // two-line name.
                    .items_center()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .py(space::SM)
                    // Only on the right: the head of the line carries its own
                    // padding on the left, and the last control would otherwise
                    // sit flush against the edge of the highlight.
                    .pr(space::SM)
                    .rounded(RADIUS)
                    // The whole line lights up, not just the part that opens
                    // it: the controls on the right belong to this device, and
                    // a highlight that stops before them reads as two rows.
                    // What the press does is still decided by what is under
                    // it, which is why only the head carries the handler.
                    .hover(|this| this.bg(color::CONTROL.alpha(0.5)))
                    .child(self.row_disclosure(
                        row,
                        line.tab_index,
                        line.thumbnail,
                        line.title,
                        line.note,
                        cx,
                    ))
                    .child(self.brightness_slider(
                        row,
                        line.brightness,
                        line.write,
                        line.tab_index + ROW_OFFSET_BRIGHTNESS,
                        cx,
                    ))
                    .child(div().flex_none().w(ROW_MODE_WIDTH).child(line.mode)),
            )
            .children(detail)
    }

    fn row_disclosure(
        &self,
        row: LightingRow,
        tab_index: isize,
        thumbnail: Div,
        title: String,
        note: Option<RowNote>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open = self.lighting_open.contains(&row);
        let id = match row {
            LightingRow::Channel(channel) => format!("lighting-row-{channel}"),
            LightingRow::Lcd => "lighting-row-lcd".to_string(),
        };
        let (fragment, sentence) = match note {
            Some(RowNote::Fragment(text)) => (Some(text), None),
            Some(RowNote::Sentence(text)) => (None, Some(text)),
            None => (None, None),
        };

        div()
            .id(SharedString::from(id))
            .flex()
            .flex_1()
            .items_center()
            .gap(space::SM)
            .min_w(ROW_HEAD_MIN_WIDTH)
            .min_h(TARGET_MIN)
            .px(space::SM)
            .rounded(RADIUS)
            // The ring is reserved rather than added on focus, so focusing a
            // row does not move the line it sits on.
            .border(FOCUS_RING)
            .border_color(color::PANEL.alpha(0.0))
            .cursor_pointer()
            .tab_index(tab_index)
            .tab_stop(true)
            // Shown to a keyboard user, who has no other way to know which row
            // Enter would open, and withheld from a pointer user, who just
            // aimed at it.
            .when(focus_visible(), |this| {
                this.focus(|this| this.border_color(color::FOCUS.hsla()))
            })
            .child(chevron(open, color::TEXT_MUTED.hsla()))
            .child(thumbnail)
            .child(
                // Truncated rather than wrapped: a long product string is what
                // the device reported, and letting it wrap makes the line grow
                // until it no longer reads as one row per device.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(space::SM)
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(color::TEXT.hsla())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            // The fragment gives up the width first: the name
                            // is what the row is, and losing the last word of
                            // "Channel 1" costs less than losing the last word
                            // of what is plugged into it.
                            .children(fragment.map(|fragment| {
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_baseline()
                                    .gap(space::SM)
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(color::TEXT_DISABLED.hsla())
                                            .child(META_SEPARATOR),
                                    )
                                    .child(
                                        div().text_color(color::TEXT_MUTED.hsla()).child(fragment),
                                    )
                            })),
                    )
                    .children(sentence.map(|sentence| {
                        div()
                            .truncate()
                            .text_sm()
                            .text_color(color::TEXT_MUTED.hsla())
                            .child(sentence)
                    })),
            )
            .on_click(cx.listener(move |shell, _, _, cx| shell.toggle_lighting_row(row, cx)))
    }

    /// The brightness a row carries on its line, as a slider the pointer drags.
    ///
    /// A real slider rather than a pair of buttons: the track publishes the
    /// rectangle it was painted at, the press captures that rectangle, and
    /// every later move is converted against it. Capturing it is what lets a
    /// drag leave the slider and keep working, and what keeps a second row's
    /// slider from answering for the one under the cursor.
    ///
    /// The keyboard reaches the same values: Left and Right move one step,
    /// Home and End go to the ends. A control that only a pointer can operate
    /// would fail the screen's keyboard-only gate.
    fn brightness_slider(
        &self,
        row: LightingRow,
        value: u8,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let enabled = state.is_enabled();
        let max = f32::from(kori_core::lighting::MAX_BRIGHTNESS);
        // Created per render and captured by this render's listeners, so the
        // rectangle a press reads is the one the track was just painted at.
        // Only an operable track is published, so a press cannot grab a slider
        // the hardware has refused.
        let sink: Option<Rc<dyn Fn(Bounds<Pixels>)>> = enabled.then(|| {
            let tracks = Rc::clone(&self.brightness_tracks);
            Rc::new(move |bounds| tracks.record(row, bounds)) as Rc<dyn Fn(Bounds<Pixels>)>
        });

        let mut slider = Slider::new(
            SharedString::from(format!("brightness-{}", row.key())),
            "Brightness",
            f32::from(value),
        )
        .range(0.0, max)
        .unit("%")
        .icons(Icon::SunLow, Icon::SunHigh)
        // The row names the device and the two glyphs name the axis, so the
        // caption would only repeat the line above it.
        .label_hidden()
        .state(state)
        .tab_index(tab_index);
        if let Some(sink) = sink {
            slider = slider.bounds_sink(sink);
        }

        let control = slider.render().when(enabled, |slider| {
            slider.on_key_down(
                cx.listener(move |shell, event: &gpui::KeyDownEvent, _, cx| {
                    let step = i16::from(crate::lighting::BRIGHTNESS_STEP);
                    let next = match event.keystroke.key.as_str() {
                        "left" | "down" => i16::from(value) - step,
                        "right" | "up" => i16::from(value) + step,
                        "home" => 0,
                        "end" => i16::from(kori_core::lighting::MAX_BRIGHTNESS),
                        _ => return,
                    };
                    shell.popover = None;
                    shell.set_brightness(row, next);
                    // A held arrow key arrives by the dozen and each press
                    // pushes the deadline out, so the value the operator stops
                    // on is the one that is written.
                    shell.schedule_write(WriteTarget::Lighting(row), cx);
                }),
            )
        });

        div().flex_none().w(ROW_BRIGHTNESS_WIDTH).child(control)
    }

    /// Set one row's pending brightness, clamped to what the daemon accepts.
    ///
    /// Takes a signed value so a keyboard step past either end arrives here to
    /// be clamped rather than wrapping around in the caller.
    pub(crate) fn set_brightness(&mut self, row: LightingRow, percent: i16) {
        let percent = percent.clamp(0, i16::from(kori_core::lighting::MAX_BRIGHTNESS)) as u8;
        match row {
            LightingRow::Channel(channel) => {
                if let Some(editor) = self.lighting.channel_mut(channel) {
                    editor.set_brightness(percent);
                }
            }
            LightingRow::Lcd => self.lcd.edit(|editor| editor.set_brightness(percent)),
        }
    }
}
