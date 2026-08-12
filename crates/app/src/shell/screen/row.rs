// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! One openable row, and what it opens.
//!
//! Every list of rows in this interface is the same object with different
//! contents: the lighting channels, the panel, and the two cooling channels.
//! The frame is stated once here.
//!
//! It has been restated before. The channels and the panel each wrote their own
//! scaffold, down to the same four comments word for word, until
//! `Shell::device_row` was written to say it once; the Cooling screen then
//! grew a third copy, with three of those comments identical character for
//! character. So the pieces below are the parts, not the whole: the container a
//! row carries while open, the band that lights up under the pointer, and the
//! region that opens it. A screen assembles them with the controls it has.

use gpui::{Div, Pixels, SharedString, Stateful, div, prelude::*, px};

use crate::assets::Icon;
use crate::components::{ControlState, Slider, chevron, focus_ring, icon};
use crate::shell::Shell;
use crate::theme::{Color, META_SEPARATOR, RADIUS, ROW_RADIUS, TARGET_MIN, color, space};
use gpui::{Context, Hsla};

use super::tab::ROW_OFFSET_BRIGHTNESS;
use super::write::WriteTarget;

/// Percentage one press of an arrow key moves a brightness slider.
///
/// A property of the control rather than of either editor. Both rows carry the
/// same slider and the same keyboard, and the constant used to exist twice, once
/// per editor, with only the lighting copy actually driving anything: the panel
/// row's keyboard step came from the lighting one too. Two constants where one
/// is live is two that can drift with nothing to notice.
pub const BRIGHTNESS_STEP: u8 = 5;

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
/// One value rather than seven positional arguments on `Shell::device_row`.
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
/// Larger than [`crate::components::ICON_SIZE`], which is the size of a glyph
/// sitting beside text. This one sits inside a filled tile and has to survive
/// the fill around it, so it takes a little under two thirds of the side,
/// leaving a margin of the color on every edge.
pub const ROW_THUMBNAIL_GLYPH: Pixels = px(20.0);

/// The fill of the line under the pointer.
///
/// A function rather than a constant on each screen: both lists light their
/// whole line, and they differ in what the line holds rather than in how it
/// reads under the pointer.
pub(crate) fn row_hover_fill() -> Hsla {
    color::CONTROL.alpha(0.5)
}

/// The container one openable row sits in: its open state, and the gap between
/// the line and what the line revealed.
///
/// Open is a state the whole row carries, not a stack of two elements that
/// happen to touch: the line and what it revealed sit on one fill, so the
/// detail is read as the inside of this device rather than as the top of the
/// next one. Held at every state, so opening a row does not move the line that
/// opened it.
pub(crate) fn row_shell(open: bool) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .p(space::XS)
        // Between the line and what it opened. Only ever applies when a detail
        // is there, since a closed row has a single child.
        .gap(space::SM)
        .rounded(ROW_RADIUS)
        .when(open, |this| this.bg(color::CONTROL.alpha(0.25)))
}

/// The region of a row that opens it: a pointer target with a reserved ring.
///
/// The caller lays it out and decides how much of the line it covers. A cooling
/// row carries no controls beside its readouts, so the whole line is the
/// target; a lighting row has a slider and a select on the same line, so only
/// the head is, and operating one of them cannot also collapse the row.
pub(crate) fn row_target(id: SharedString, tab_index: isize) -> Stateful<Div> {
    focus_ring(
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(space::SM)
            .min_h(TARGET_MIN)
            .px(space::SM)
            .rounded(RADIUS)
            .cursor_pointer()
            .tab_index(tab_index)
            .tab_stop(true),
        true,
    )
}

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
    /// Whether one row of the Lighting screen is open.
    pub(crate) fn is_open(&self, row: LightingRow) -> bool {
        self.rows.lighting.contains(row)
    }

    /// One device row of the Lighting screen: the line, and whatever it
    /// revealed.
    pub(crate) fn device_row(
        &self,
        row: LightingRow,
        line: RowLine,
        detail: Option<Div>,
        cx: &mut Context<Self>,
    ) -> Div {
        row_shell(detail.is_some())
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
                    .hover(|this| this.bg(row_hover_fill()))
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

    /// The head of a device row: chevron, thumbnail and two lines of text, as
    /// one target that opens the row.
    fn row_disclosure(
        &self,
        row: LightingRow,
        tab_index: isize,
        thumbnail: Div,
        title: String,
        note: Option<RowNote>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open = self.is_open(row);
        let (fragment, sentence) = match note {
            Some(RowNote::Fragment(text)) => (Some(text), None),
            Some(RowNote::Sentence(text)) => (None, Some(text)),
            None => (None, None),
        };

        row_target(
            SharedString::from(format!("lighting-row-{}", row.key())),
            tab_index,
        )
        .flex_1()
        .min_w(ROW_HEAD_MIN_WIDTH)
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
                                .child(div().text_color(color::TEXT_MUTED.hsla()).child(fragment))
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
        if let Some(sink) = self.interaction.brightness_sink(row, enabled) {
            slider = slider.bounds_sink(sink);
        }

        let control = slider.render().when(enabled, |slider| {
            slider.on_key_down(
                cx.listener(move |shell, event: &gpui::KeyDownEvent, _, cx| {
                    let step = i16::from(BRIGHTNESS_STEP);
                    let next = match event.keystroke.key.as_str() {
                        "left" | "down" => i16::from(value) - step,
                        "right" | "up" => i16::from(value) + step,
                        "home" => 0,
                        "end" => i16::from(kori_core::lighting::MAX_BRIGHTNESS),
                        _ => return,
                    };
                    shell.interaction.dismiss();
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
