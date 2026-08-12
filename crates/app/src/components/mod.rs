// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The reusable interface primitives.
//!
//! Every control shares one interaction model: it is either enabled, disabled
//! with a reason, or in error. Disabled is never decorative here. It is how the
//! product refuses to expose a write the hardware has not proven it supports,
//! so each primitive carries the reason rather than silently graying out.
//!
//! Split by what a primitive is rather than by what it is made of: the pill,
//! the surfaces and the state vocabulary live here because every other module
//! builds on them, and each family of widget owns the painting it needs. A
//! shared `paint` module would have been a place for one widget's geometry to
//! be read as another's.

use std::cell::Cell;

use gpui::{
    Div, ElementId, Hsla, PathBuilder, Pixels, Point, SharedString, Stateful, Svg, Window, div,
    prelude::*, px, svg,
};

use crate::assets::Icon;
use crate::theme::{CONTROL_HEIGHT, Color, FOCUS_RING, MENU_GLYPH_SIZE, RADIUS, color, space};

mod chart;
mod control;
mod curve;
mod readout;

pub use chart::Sparkline;
pub use control::{Button, ColorField, Select, SelectOption, Slider, parse_hex_color};
pub use curve::{CurveEditor, node_at};
pub use readout::{DeviceHealth, DeviceRow, Metric};

/// What a control is allowed to do right now.
///
/// The reason is a [`SharedString`] rather than an owned `String`: one gate
/// answer is handed to every control on a screen, and the panel's open row
/// alone clones it ten times per repaint. A refusal sentence copied ten times a
/// frame is an allocation per control for a value none of them mutate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlState {
    Enabled,
    /// Interaction is refused, and `reason` says why in operator language.
    Disabled {
        reason: SharedString,
    },
    /// The current value is invalid, and `message` names the accepted input.
    Error {
        message: SharedString,
    },
}

impl ControlState {
    pub fn disabled(reason: impl Into<SharedString>) -> Self {
        Self::Disabled {
            reason: reason.into(),
        }
    }

    pub fn error(message: impl Into<SharedString>) -> Self {
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
    pub fn message(&self) -> Option<&SharedString> {
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

thread_local! {
    /// Whether focus arrived by keyboard, and a ring is therefore wanted.
    static FOCUS_VISIBLE: Cell<bool> = const { Cell::new(true) };
}

/// Whether controls should draw their focus ring right now.
///
/// This is what a browser calls `:focus-visible`, which GPUI has no equivalent
/// of: its `focus` style applies however focus arrived, so every clicked
/// control keeps a ring afterwards. The ring is what tells someone navigating
/// by keyboard where they are, and noise to someone who just pointed at the
/// control they were already looking at.
///
/// It is thread-local process state rather than a parameter because that is
/// what it describes: how the person is driving the window right now, not a
/// property of any one control. Threading it through would put the same value
/// in every primitive's signature and in every call site. The interface renders
/// on one thread, so a `Cell` is the whole synchronization story.
pub fn focus_visible() -> bool {
    FOCUS_VISIBLE.with(Cell::get)
}

/// Record how focus last moved. The shell sets this; controls only read it.
pub fn set_focus_visible(visible: bool) {
    FOCUS_VISIBLE.with(|flag| flag.set(visible));
}

/// Reserve the focus ring, and reveal it only while the keyboard is driving.
///
/// Two facts that always travel together, so they are stated once. The ring is
/// reserved rather than added on focus, which is what keeps a control from
/// growing by two pixels and shifting the line it sits on the moment it takes
/// focus; and it is drawn only when focus arrived by keyboard, because it is
/// what tells someone navigating by Tab where they are and noise to someone who
/// just pointed at the control they were already looking at.
///
/// `focusable` is whether this control can hold focus at all. A disabled one
/// still reserves the ring, so it is exactly the size of the enabled control
/// beside it, and never reveals one.
///
/// Six controls carry this: the pill, the button, a rail entry, a device row,
/// the curve plot and a swatch. They carried it six times, three of them with
/// the same comment word for word. A menu row is the deliberate exception, and
/// adds its ring rather than reserving one: it is drawn over the page rather
/// than in it, so nothing moves when it grows.
pub fn focus_ring<E>(element: E, focusable: bool) -> E
where
    E: Styled + InteractiveElement,
{
    // Which token the invisible border borrows does not matter, because the
    // alpha is zero. One expression rather than the three different surfaces
    // this used to be spelled with, so nobody has to wonder whether the
    // difference was a decision.
    let element = element
        .border(FOCUS_RING)
        .border_color(color::PANEL.alpha(0.0));
    if focusable && focus_visible() {
        element.focus(|this| this.border_color(color::FOCUS.hsla()))
    } else {
        element
    }
}

/// The pill every value control is built on, matched to Paneflow's
/// `select_trigger`.
///
/// A subtle-gray pill with no outline, 10 by 6 of padding, and a fill that
/// sinks rather than lifts under the pointer. The focus ring is taken out of
/// that padding rather than added to it, so reserving the ring leaves the pill
/// exactly the size of the one it matches and focusing it moves nothing.
///
/// Shared by the select, the color field and the slider rather than copied into
/// each: they sit on the same lines as each other, and a pill that is a pixel
/// taller in one of them is visible as a step in the row.
///
/// An outline comes back for one state only: a control holding a value that
/// cannot be parsed. Nothing else on the pill says so, and the message under it
/// is not in the same place the eye is.
fn control_pill(id: impl Into<ElementId>, state: &ControlState, tab_index: isize) -> Stateful<Div> {
    pill(id, state, tab_index, true)
}

/// The same pill, unfilled: a slider's own surface.
///
/// A track needs the height and the reserved ring, so it stays on the baseline
/// of the control beside it, but not the fill. A slider already draws a shape
/// of its own, and a fill behind it reads as a second card parked inside the
/// row rather than as part of it.
fn slider_surface(
    id: impl Into<ElementId>,
    state: &ControlState,
    tab_index: isize,
) -> Stateful<Div> {
    pill(id, state, tab_index, false)
}

fn pill(
    id: impl Into<ElementId>,
    state: &ControlState,
    tab_index: isize,
    filled: bool,
) -> Stateful<Div> {
    let enabled = state.is_enabled() || matches!(state, ControlState::Error { .. });
    let base = div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap(space::SM)
        .min_h(CONTROL_HEIGHT)
        .px(space::SM)
        .py(space::XS)
        .rounded(RADIUS)
        .bg(if filled {
            color::CONTROL.hsla()
        } else {
            color::CONTROL.alpha(0.0)
        })
        .text_xs()
        .text_color(state.text_color());

    // The reserved ring, plus the one state that keeps an outline at rest: a
    // control holding a value that cannot be parsed. Nothing else on the pill
    // says so, and the message under it is not where the eye is. Set after the
    // ring, because at rest the last color wins while focus keeps its own.
    let base = focus_ring(base, enabled).when(
        matches!(state, ControlState::Error { .. }),
        |this| this.border_color(color::DANGER.hsla()),
    );

    if enabled {
        base.tab_index(tab_index)
            .tab_stop(true)
            .cursor_pointer()
            // An unfilled surface has no fill to change: pointing at it would
            // make a card appear that was not there, which says the wrong
            // thing about what is under the pointer.
            .when(filled, |this| {
                this.hover(|this| this.bg(color::CONTROL_HOVER.hsla()))
                    .active(|this| this.bg(color::ACCENT_ACTIVE.alpha(0.35)))
            })
    } else {
        // A disabled control is not a tab stop: keyboard traversal must not
        // stop on something that cannot be operated.
        base.cursor_default().opacity(0.6)
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
        panel_surface().child(
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

/// Side of an icon drawn inline with text, in logical pixels.
pub const ICON_SIZE: Pixels = px(16.0);

/// One icon at one size, in one color.
///
/// The color is not optional and is not decoration: GPUI renders an SVG to an
/// alpha mask and tints it with the element's text color, so an icon without
/// one resolves to `None` in `Svg::paint` and draws nothing at all. Routing
/// every icon through here is what keeps that from being a per-call-site
/// mistake, and what keeps icon color coming from `theme.rs` like every other
/// color in this interface.
pub fn icon(icon: Icon, size: Pixels, color: Hsla) -> Svg {
    svg().path(icon.path()).size(size).text_color(color)
}

/// The glyph at the right edge of a menu trigger.
///
/// Smaller than [`ICON_SIZE`] and dimmer than the value beside it, as Paneflow's
/// `select_chevron` is: it says the control opens, and says it under the value
/// rather than beside it in the same weight.
pub fn select_chevron() -> Svg {
    icon(
        Icon::ChevronDown,
        MENU_GLYPH_SIZE,
        color::TEXT_MUTED.alpha(0.7),
    )
    .flex_none()
}

/// The disclosure chevron of a collapsible row.
pub fn chevron(open: bool, color: Hsla) -> Svg {
    let name = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    icon(name, ICON_SIZE, color).flex_none()
}

/// The raised surface a [`Panel`] draws on, without its heading.
///
/// Shared rather than duplicated so a section that carries no title still sits
/// on exactly the same surface as every titled one. It carries no outline: a
/// card is told apart from the ground under it by its luminance, and the panel
/// fill already clears the work surface it sits on.
pub fn panel_surface() -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .gap(space::MD)
        .p(space::LG)
        .rounded(RADIUS)
        .bg(color::PANEL.hsla())
}

/// The same surface, for a card whose content is a list of openable rows.
///
/// One padding step instead of [`panel_surface`]'s four. A row is not a
/// paragraph: it carries its own inset, its own hover fill and its own corner,
/// so a card that also holds it at arm's length stacks three insets between the
/// edge of the card and the control the operator aimed at. A card of prose or
/// of fields keeps the wider step, because nothing inside those pads itself.
pub fn row_panel() -> Div {
    panel_surface().p(space::SM)
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

/// A control with its optional caption above and state message underneath.
///
/// The wrapper carries the element id, so the whole field is one click target
/// rather than just the control box inside it.
///
/// `label` is optional because a control on a device row is named by the row it
/// is on: repeating the name over every control is a caption that says what the
/// line already said, and it doubles the height of the line to do it.
fn field(
    id: impl Into<ElementId>,
    label: Option<SharedString>,
    message: Option<SharedString>,
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
        .children(label.map(|label| {
            div()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child(label)
        }))
        .child(control)
        .children(message.map(|message| div().text_sm().text_color(message_color).child(message)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_control_carries_its_reason() {
        let state = ControlState::disabled("Another process owns this device.");
        assert!(state.is_disabled());
        assert!(!state.is_enabled());
        assert_eq!(
            state.message().map(SharedString::as_ref),
            Some("Another process owns this device.")
        );
    }

    /// The gate answer is fanned out to every control on a screen, so cloning
    /// it must not copy the sentence it carries.
    #[test]
    fn cloning_a_refusal_shares_its_sentence_rather_than_copying_it() {
        let state = ControlState::disabled("The background service is not running.");
        let copy = state.clone();
        let (Some(first), Some(second)) = (state.message(), copy.message()) else {
            panic!("a disabled control carries a reason");
        };
        assert!(
            std::ptr::eq(first.as_ref() as *const str, second.as_ref() as *const str),
            "the clone allocated a second copy of the sentence"
        );
    }

    #[test]
    fn a_note_states_its_severity_in_words() {
        for level in [NoteLevel::Info, NoteLevel::Warning, NoteLevel::Critical] {
            assert!(!level.label().is_empty());
        }
        assert_ne!(NoteLevel::Warning.label(), NoteLevel::Critical.label());
    }
}
