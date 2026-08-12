// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The application shell: a fixed navigation rail and one work surface.
//!
//! Four primary destinations, one secondary Settings entry, and nothing else.
//! The rail never scrolls and never changes width, so the work surface has a
//! known width to lay out against at the 920x640 target size.
//!
//! The window holds no hardware handle. It repaints when the worker publishes a
//! new snapshot, and every write control is gated on what the daemon reported.
//!
//! What the shell owns is deliberately grouped rather than spread flat. Every
//! screen module writes into this one value, so a field it can reach is a field
//! any of them can put in a state the others do not expect: the open popover
//! was closed by hand in eleven places across seven files before it had an
//! owner. What is left is the frame, the three editors, and the bookkeeping of
//! edits leaving the process.

use std::time::Instant;

use gpui::{
    App, Context, Div, FocusHandle, Focusable, KeyBinding, MouseButton, Pixels, SharedString,
    Stateful, Window, actions, div, prelude::*,
};
use kori_core::profile::CoolingProgram;
use kori_core::telemetry::KrakenTelemetry;

use crate::assets::Icon;
use crate::components::{
    ICON_SIZE, Note, NoteLevel, focus_ring, focus_visible, icon, set_focus_visible,
};
use crate::cooling::CoolingEditor;
use crate::display::DisplayScreen;
use crate::feed::{CommandOutcome, CommandSubject, Feed, OutcomeSeverity, now_unix_ms};
use crate::lighting::LightingEditor;
use crate::link::LinkState;
use crate::metrics::MetricBook;
use crate::shell::screen::drag::{Drag, Interaction};
use crate::shell::screen::row::LightingRow;
use crate::shell::screen::write::{WriteSchedule, WriteTarget, write_is_held_back};
use crate::shell::screen::{Disclosure, Popover};
use crate::theme::{CARD_INSET, CARD_RADIUS, RADIUS, RAIL_WIDTH, TARGET_MIN, color, space};
use crate::window_chrome::{self, DragLatch};

pub mod screen;

actions!(
    shell,
    [
        FocusNext,
        FocusPrevious,
        GoMonitoring,
        GoCooling,
        GoLighting,
        GoSettings,
        ClosePopover,
    ]
);

/// Key bindings, registered once at startup.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("ctrl-1", GoMonitoring, None),
        KeyBinding::new("ctrl-2", GoCooling, None),
        KeyBinding::new("ctrl-3", GoLighting, None),
        KeyBinding::new("ctrl-comma", GoSettings, None),
        KeyBinding::new("escape", ClosePopover, None),
    ]
}

/// The only destinations this product has.
///
/// The panel has no destination of its own. It is one device's appearance
/// among the others, so it lives on Lighting next to the channels of the
/// controller, which is also where an operator looking for "what my hardware
/// shows" goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Destination {
    Monitoring,
    Cooling,
    Lighting,
    Settings,
}

impl Destination {
    /// Primary destinations, in rail order.
    pub const PRIMARY: [Destination; 3] = [
        Destination::Monitoring,
        Destination::Cooling,
        Destination::Lighting,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Monitoring => "Monitoring",
            Self::Cooling => "Cooling",
            Self::Lighting => "Lighting",
            Self::Settings => "Settings",
        }
    }

    /// Icon shown in the rail, so an entry is not identified by color alone.
    pub fn icon(self) -> Icon {
        match self {
            Self::Monitoring => Icon::ChartLine,
            Self::Cooling => Icon::Snowflake,
            Self::Lighting => Icon::Bulb,
            Self::Settings => Icon::Settings,
        }
    }

    /// Tab index of this rail entry. Rail entries come before screen controls.
    pub fn tab_index(self) -> isize {
        match self {
            Self::Monitoring => 1,
            Self::Cooling => 2,
            Self::Lighting => 3,
            Self::Settings => 4,
        }
    }
}

/// The root view.
pub struct Shell {
    focus: FocusHandle,
    feed: Feed,
    link: LinkState,
    metrics: MetricBook,
    cooling: CoolingEditor,
    lighting: LightingEditor,
    /// The LCD editor and the last preset that rendered, held together so an
    /// unfinished field keeps the previous picture on screen instead of
    /// blanking it, and so no control can move one without the other.
    lcd: DisplayScreen,
    /// What the pointer is doing, and where this frame painted what it can grab.
    interaction: Interaction,
    /// Which rows each screen has open, and where the keyboard sits on a plot.
    rows: Disclosure,
    destination: Destination,
    /// When each edited target should be written, once it settles.
    due: WriteSchedule,
    /// The program the last cooling write sent, held until its outcome arrives.
    sent: Option<CoolingProgram>,
    /// How the last command ended, on whichever screen issued it.
    outcome: Option<CommandOutcome>,
    /// Set once the operator has armed the profile deletion.
    ///
    /// Deleting a profile is the one destructive action this screen offers, so
    /// it takes two deliberate activations rather than one stray click.
    confirm_delete: bool,
    /// Set while the pointer holds the title bar, before a move begins.
    window_drag: DragLatch,
    /// Set once a daemon status has seeded the panel editor.
    ///
    /// The seeding happens on the first status that arrives and never again: a
    /// later snapshot arriving mid-edit would replace what the operator is
    /// typing with what the panel is still showing. The first status is also the
    /// only moment where nothing can be lost, since every write control is
    /// disabled until the link reports one.
    lcd_seeded: bool,
    /// The committed program the last status reported, as it was reported.
    ///
    /// Held so a change can be told from a repetition. The panel is seeded once
    /// and never again because its editor holds text a snapshot must not
    /// retype; a cooling program has no such field, and following it is what
    /// puts a newly activated profile's curve on the plot without the client
    /// having to guess what activation did.
    committed: Option<CoolingProgram>,
    /// Wall clock of the last refresh, used to age every reading.
    now_unix_ms: u64,
}

impl Shell {
    pub fn new(
        feed: Feed,
        notifications: futures::channel::mpsc::UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);

        // The view repaints when the worker publishes, not on a timer of its
        // own: one repaint per sample, and no lag between the two.
        cx.spawn(async move |shell, cx| {
            let mut notifications = notifications;
            use futures::StreamExt;
            while notifications.next().await.is_some() {
                if shell.update(cx, |shell, cx| shell.refresh(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let mut rows = Disclosure::default();
        // The panel opens first: it is the row with something to look at, and a
        // screen where every row is shut hides the editor the last arrangement
        // put on its own destination.
        rows.lighting.insert(LightingRow::Lcd);

        Self {
            focus,
            feed,
            link: LinkState::connecting(),
            metrics: MetricBook::new(),
            cooling: CoolingEditor::new(),
            lighting: LightingEditor::default(),
            lcd: DisplayScreen::default(),
            interaction: Interaction::default(),
            rows,
            destination: Destination::Monitoring,
            due: WriteSchedule::default(),
            sent: None,
            outcome: None,
            confirm_delete: false,
            window_drag: DragLatch::default(),
            lcd_seeded: false,
            committed: None,
            now_unix_ms: now_unix_ms(),
        }
    }

    /// Take whatever the worker published and repaint.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.now_unix_ms = now_unix_ms();

        if let Some(link) = self.feed.link() {
            if let Some(snapshot) = link.telemetry() {
                self.metrics.observe(snapshot);
            }
            self.link = link;
            // What the panel is running outlives this window: the daemon keeps
            // writing the preset it committed, so opening the client again must
            // show that preset rather than the factory arrangement. The first
            // status is what seeds it, and only the first, so a snapshot cannot
            // overwrite an edit in progress.
            if !self.lcd_seeded
                && let Some(status) = self.link.status()
            {
                self.lcd_seeded = true;
                if let Some(preset) = status.display.committed.clone() {
                    self.lcd.adopt(&preset);
                }
            }
            self.adopt_committed_program();
            // The controller's channel list is the only source of what can be
            // addressed, so the editor follows it rather than assuming three.
            let channels: Vec<u8> = self
                .link
                .lighting_channels()
                .iter()
                .map(|state| state.channel)
                .collect();
            self.lighting.sync(&channels);
            // The preview carries readings, so it follows the sample that just
            // landed. Built here rather than in the repaint that draws it: a
            // repaint happens on every pointer move, and this is the only
            // moment anything the picture is made of can have changed.
            self.refresh_picture(cx);
        }

        self.adopt_outcome();

        // The timers are what make an edit land promptly. This is what makes
        // sure it lands at all: a write held back because another was in
        // flight, or a timer that fired while a drag was still running, is
        // picked up here on the next sample rather than waiting for the
        // operator to touch the screen again.
        self.flush_writes(cx);

        cx.notify();
    }

    /// Take the worker's latest outcome, and resolve what it settles.
    ///
    /// Told apart from the last one by its sequence rather than by comparing
    /// the whole value: two refusals of the same edit for the same reason are
    /// equal, and treating the second as already seen would leave [`Self::sent`]
    /// set on a write that will never be answered again.
    fn adopt_outcome(&mut self) {
        let Some(outcome) = self.feed.outcome() else {
            return;
        };
        if self.outcome.as_ref().map(|seen| seen.sequence) == Some(outcome.sequence) {
            return;
        }

        // Only a cooling write's own outcome resolves the cooling write in
        // flight. A refused lighting command used to clear it, which released
        // the guard that keeps two cooling writes from overlapping and left a
        // confirmed curve unrecorded.
        if outcome.subject == CommandSubject::Cooling {
            match outcome.severity {
                // A confirmed write is what turns a pending curve into the
                // client's record of what the hardware is running. Curve points
                // cannot be read back, so this record is the only evidence
                // there is, and it is only ever set from a confirmation.
                OutcomeSeverity::Confirmed => {
                    if let Some(program) = self.sent.take() {
                        self.cooling.record_applied(program);
                    }
                }
                OutcomeSeverity::Unconfirmed | OutcomeSeverity::Refused => self.sent = None,
            }
        }
        self.outcome = Some(outcome);
    }

    fn kraken(&self) -> Option<&KrakenTelemetry> {
        self.link.telemetry().map(|snapshot| &snapshot.kraken)
    }

    fn go(&mut self, destination: Destination, cx: &mut Context<Self>) {
        self.destination = destination;
        self.interaction.dismiss();
        // An armed deletion does not survive leaving the screen: coming back to
        // a button that already says "Confirm" would delete on one press.
        self.confirm_delete = false;
        cx.notify();
    }

    /// Open one row of the Lighting screen, or close it if it is open.
    ///
    /// Every control the detail renders is built for the channel whose line it
    /// sits under and carries that channel with it, so opening a row reveals
    /// controls rather than selecting anything.
    fn toggle_lighting_row(&mut self, row: LightingRow, cx: &mut Context<Self>) {
        self.rows.lighting.toggle(row);
        // A popover anchored to a control that just moved would be left
        // pointing at nothing.
        self.interaction.dismiss();
        // An animation only runs while it is on screen. A closed row repaints
        // nothing, so a timer firing into it would be ten wake-ups a second
        // spent moving a cursor nobody can see.
        if row == LightingRow::Lcd {
            self.play_film(cx);
        }
        cx.notify();
    }

    fn toggle_popover(&mut self, popover: Popover, cx: &mut Context<Self>) {
        self.interaction.toggle(popover);
        cx.notify();
    }

    fn on_focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        set_focus_visible(true);
        window.focus_next();
        cx.notify();
    }

    fn on_focus_previous(
        &mut self,
        _: &FocusPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        set_focus_visible(true);
        window.focus_prev();
        cx.notify();
    }

    fn on_close_popover(&mut self, _: &ClosePopover, _: &mut Window, cx: &mut Context<Self>) {
        if self.interaction.dismiss() {
            cx.notify();
        }
    }

    /// The navigation rail, as a card inset from the window it sits in.
    ///
    /// Paneflow's shape: the window's caption buttons at the top of the column,
    /// the destinations below them, and the utility entry pinned to a footer.
    /// Nothing names the product here. The window title carries the name, the
    /// Settings screen carries the qualifier, and a heading repeating either one
    /// would only push the first destination down the column.
    ///
    /// The card is a surface above the window's own, so the two separate by
    /// luminance rather than by a drawn divider, and it is inset far enough on
    /// every side that no corner of it has to know how the window is rounding
    /// its own.
    ///
    /// `reserved_top` is the strip the title bar is laid over. The card runs
    /// under it and starts its own content below, which is what puts the caption
    /// buttons inside the card rather than in a bar above it.
    fn rail(&self, reserved_top: Pixels, cx: &mut Context<Self>) -> Div {
        let current = self.destination;
        // A `map` closure would have to hold the mutable context borrow across
        // calls, which the 2024 edition's capture rules reject. A plain loop
        // borrows it once per entry and releases it.
        let mut primary_entries = Vec::with_capacity(Destination::PRIMARY.len());
        for destination in Destination::PRIMARY {
            primary_entries.push(self.rail_entry(destination, current, cx));
        }

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(RAIL_WIDTH)
            .h_full()
            .bg(color::SURFACE.hsla())
            .rounded(CARD_RADIUS)
            // No outline: the card is told apart from the window by its
            // luminance, which is how Paneflow separates its own surfaces.
            .overflow_hidden()
            .pt(reserved_top)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::XS)
                    // The same inset the card keeps from the window, so an entry
                    // and the caption buttons above it sit on one edge.
                    .px(CARD_INSET)
                    .pt(CARD_INSET)
                    .children(primary_entries),
            )
            .child(div().flex_1())
            .child(
                // No divider either: the empty space above it is what sets the
                // utility entry apart from the destinations.
                div()
                    .flex()
                    .flex_col()
                    .px(CARD_INSET)
                    .pb(CARD_INSET)
                    .child(self.rail_entry(Destination::Settings, current, cx)),
            )
    }

    /// Returns a concrete element rather than `impl IntoElement`.
    ///
    /// Under the 2024 edition an opaque return type captures every input
    /// lifetime, so an `impl IntoElement` here would keep borrowing the context
    /// and only one entry could be built at a time.
    fn rail_entry(
        &self,
        destination: Destination,
        current: Destination,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = destination == current;
        // The label color, which the icon takes too: an entry whose glyph and
        // word disagreed would read as a word with a decoration beside it.
        let ink = if selected {
            color::TEXT_ON_ACCENT.hsla()
        } else {
            color::TEXT.hsla()
        };
        // A menu row's fills, with one exception. Where a menu marks its current
        // value with a whisper, because choosing there has not happened yet, the
        // rail marks the destination the operator is standing in and takes the
        // accent for it. Everything else is the menu's: no fill at rest, and a
        // 5% wash under the pointer rather than a grey block.
        let resting = if selected {
            color::ACCENT.hsla()
        } else {
            color::TEXT.alpha(0.0)
        };
        let hovered = if selected {
            color::ACCENT_HOVER.hsla()
        } else {
            color::TEXT.alpha(0.05)
        };

        focus_ring(
            div()
                .id(SharedString::from(destination.label()))
                .tab_index(destination.tab_index())
                .tab_stop(true)
                .flex()
                .items_center()
                .gap(space::SM)
                .w_full()
                // The menu row's padding, but not its height: a menu row is 28
                // tall because it is one of a list the pointer is already
                // inside, and a rail entry is a target the pointer travels to,
                // so it keeps the floor every pointer target in this interface
                // keeps.
                .min_h(TARGET_MIN)
                .p(space::SM)
                .rounded(RADIUS)
                .cursor_pointer()
                .text_xs()
                .text_color(ink),
            true,
        )
        .bg(resting)
        .hover(|this| this.bg(hovered))
        .child(icon(destination.icon(), ICON_SIZE, ink))
        .child(destination.label())
        .on_click(cx.listener(move |this, _, _, cx| this.go(destination, cx)))
    }

    fn banner(&self) -> Option<Div> {
        let message = self.link.banner()?;
        Some(Note::new(NoteLevel::Warning, message).render())
    }

    /// Note an edit and arrange for it to be written once things go quiet.
    ///
    /// Every edit spawns its own timer and every timer drains whatever has come
    /// due, so a later edit on the same target pushes that target's deadline
    /// out and the earlier timers fire into nothing. No task has to be
    /// cancelled and no edit can be lost by cancelling the wrong one.
    fn schedule_write(&mut self, target: WriteTarget, cx: &mut Context<Self>) {
        // The picture is built at the edit rather than at the repaint, and this
        // is where every panel edit passes.
        if target == WriteTarget::Lighting(LightingRow::Lcd) {
            self.refresh_picture(cx);
        }
        let quiet = target.quiet();
        self.due.touch(target, Instant::now() + quiet);
        cx.notify();
        cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(quiet).await;
            let _ = shell.update(cx, |shell, cx| shell.flush_writes(cx));
        })
        .detach();
    }

    /// Write everything that has been still long enough.
    ///
    /// The quiet period is not cosmetic. The daemon refuses a command arriving
    /// inside its cadence floor rather than queueing it, and it keeps no
    /// last-value-wins. A screen that wrote on every pointer move would have
    /// most of its writes refused, and the value the operator actually stopped
    /// on could be one of them.
    ///
    /// A target that is due but held back keeps its place in the book rather
    /// than losing its write: this also runs on every sample, so a write held
    /// back by a gesture or by an outcome that had not landed is picked up on
    /// the next one instead of waiting for the operator to touch the screen
    /// again.
    fn flush_writes(&mut self, cx: &mut Context<Self>) {
        for target in self.due.take_due(Instant::now()) {
            if write_is_held_back(target, self.interaction.drag(), self.sent.is_some()) {
                self.due.touch(target, Instant::now() + target.quiet());
                continue;
            }
            match target {
                WriteTarget::Cooling => self.send_cooling(),
                WriteTarget::Lighting(LightingRow::Channel(channel)) => self.send_lighting(channel),
                WriteTarget::Lighting(LightingRow::Lcd) => self.send_display(),
            }
        }
        cx.notify();
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Each frame republishes the tracks it paints. Clearing here is what
        // keeps a row that has gone away, or a screen that no longer shows one,
        // from leaving a rectangle behind that a press could still grab.
        self.interaction.clear_tracks();

        let content = match self.destination {
            Destination::Monitoring => self.monitoring(),
            Destination::Cooling => self.cooling(cx),
            Destination::Lighting => self.lighting(cx),
            Destination::Settings => self.settings(),
        };

        let title_bar = window_chrome::title_bar(window, &self.window_drag);
        // The bar is laid over the top of the window, so everything under it
        // starts below the strip it occupies. The card and the work surface
        // already begin one gap down, which is that much less to reserve.
        let reserved_top = window_chrome::title_bar_height(window) - CARD_INSET;

        let shell = div()
            .id("shell")
            .track_focus(&self.focus)
            // Any press anywhere in the window means focus is about to move by
            // pointer, so the ring is dropped before the control under the
            // cursor renders with it. Capture phase, so it runs ahead of that
            // control's own handler rather than after it. Enter and Space reach
            // a control through the click listeners without a mouse event, so
            // keyboard activation leaves the ring alone.
            .capture_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                if focus_visible() {
                    set_focus_visible(false);
                    cx.notify();
                }
                // An open popover owns the press: it is being dismissed, and a
                // press that lands over a slider or a curve on the way out must
                // not also move a value the operator was not aiming at. This is
                // also why `begin_drag` never has a list to close.
                if this.interaction.showing_any() {
                    return;
                }
                // A press starts at most one gesture, decided here from where
                // the controls were painted rather than by a listener on each
                // of them: an event that has to reach a control nested under
                // its own interactive state never arrived, and the capture
                // phase on the window is the one place it always does.
                if event.button == MouseButton::Left {
                    this.begin_drag(event.position, cx);
                }
            }))
            // A brightness drag is tracked by the window rather than by the
            // slider: the pointer routinely leaves a 200-pixel control while
            // still holding it, and a drag that stopped at the edge would leave
            // the value wherever the cursor crossed it.
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                this.drag_to(event.position, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    // Nothing is written while the pointer holds a control: a
                    // drag names dozens of values and only the one it stops on
                    // is worth a command. Releasing ends the gesture wherever
                    // the pointer is, including outside the control that
                    // started it.
                    match this.interaction.end_drag() {
                        Some(Drag::Brightness { row, .. }) => {
                            this.schedule_write(WriteTarget::Lighting(row), cx)
                        }
                        Some(Drag::Duty { .. } | Drag::Curve { .. }) => {
                            this.schedule_write(WriteTarget::Cooling, cx)
                        }
                        None => {}
                    }
                }),
            )
            .on_action(cx.listener(Self::on_focus_next))
            .on_action(cx.listener(Self::on_focus_previous))
            .on_action(cx.listener(Self::on_close_popover))
            .on_action(
                cx.listener(|this, _: &GoMonitoring, _, cx| this.go(Destination::Monitoring, cx)),
            )
            .on_action(cx.listener(|this, _: &GoCooling, _, cx| this.go(Destination::Cooling, cx)))
            .on_action(
                cx.listener(|this, _: &GoLighting, _, cx| this.go(Destination::Lighting, cx)),
            )
            .on_action(
                cx.listener(|this, _: &GoSettings, _, cx| this.go(Destination::Settings, cx)),
            )
            .size_full()
            .flex()
            // The gap and the padding are what make the rail read as a card
            // laid on the window rather than as a column cut out of it.
            .gap(CARD_INSET)
            .p(CARD_INSET)
            .text_color(color::TEXT.hsla())
            .text_sm()
            .child(self.rail(reserved_top, cx))
            .child(
                div()
                    .flex_1()
                    // Without this the rail plus an unwrapped sentence can
                    // exceed the window width instead of wrapping.
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    // The caption strip carries nothing on this side, and the
                    // band is reserved anyway: it is the window's own bar, so
                    // it stays a place to drag the window from rather than a
                    // place a heading can reach. Rigid and outside the scroll,
                    // as Paneflow reserves it, so a scrolled screen passes
                    // under nothing.
                    .child(div().flex_none().h(reserved_top))
                    .child(
                        div()
                            .id("work-surface")
                            .flex_1()
                            // A flex child floors at its content height unless
                            // it is told it may shrink, which is what turns the
                            // overflow below into a scroll instead of a spill.
                            .min_h_0()
                            // No fill: the window's own surface is the ground
                            // the panels and the rail card sit on.
                            .overflow_y_scroll()
                            // One gap of its own on top of the row's, so a
                            // screen keeps a little more air than the rail card
                            // takes.
                            .px(space::SM)
                            .pb(space::SM)
                            .flex()
                            .flex_col()
                            .gap(space::LG)
                            .children(self.banner())
                            .child(content),
                    ),
            );

        // The chrome color fills the window, so the transparent title bar, the
        // work surface and all four corners read as one ground. The bar is laid
        // over the shell rather than stacked above it: that is what puts the
        // caption buttons inside the rail card while every pixel across the top
        // of the window stays a place to drag it from.
        window_chrome::window_shell(
            div()
                .relative()
                .size_full()
                .child(shell)
                // While a list is open, the rest of the window is a way out of
                // it. Painted under the popover and over everything else, so a
                // press anywhere else closes the list and stops there: the
                // control underneath is not also operated by the press that
                // dismissed the list, which is how a menu behaves everywhere
                // else on the desktop. Escape does the same from the keyboard.
                .children(self.interaction.showing_any().then(|| {
                    div()
                        .id("popover-dismiss")
                        // Takes the press rather than letting it through, so
                        // the control that opened the list does not reopen it
                        // on the release and no other control is operated by
                        // the press that closed it.
                        .occlude()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.interaction.dismiss();
                                cx.notify();
                            }),
                        )
                }))
                .child(div().absolute().top_0().left_0().w_full().child(title_bar)),
            window,
            color::RAIL.hsla(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{WINDOW_HEIGHT, WINDOW_WIDTH};
    use kori_core::profile::Channel;
    use std::time::Duration;

    /// Lay the shell out and paint it at the size the layout is designed for.
    ///
    /// The view itself, not a placeholder: `draw` paints whatever the closure
    /// returns, so handing it an empty element would assert nothing at all.
    fn draw(shell: &gpui::Entity<Shell>, cx: &mut gpui::VisualTestContext) {
        let view = shell.clone();
        cx.draw(
            gpui::point(gpui::px(0.0), gpui::px(0.0)),
            gpui::size(WINDOW_WIDTH, WINDOW_HEIGHT),
            |_, _| gpui::AnyView::from(view),
        );
    }

    /// A shell with no daemon behind it, which is the state every write control
    /// is disabled in and every readout has to describe without inventing one.
    fn shell(cx: &mut gpui::TestAppContext) -> (gpui::Entity<Shell>, &mut gpui::VisualTestContext) {
        cx.update(|cx| cx.bind_keys(key_bindings()));
        let (feed, notifications) = Feed::spawn(
            std::path::PathBuf::from("/nonexistent/kori/absent.sock"),
            Duration::from_secs(3_600),
        );
        cx.add_window_view(|window, cx| Shell::new(feed, notifications, window, cx))
    }

    /// Every destination builds its whole element tree.
    ///
    /// The unit tests around this one exercise the rules a screen applies; none
    /// of them builds one. That gap is not theoretical: GPUI asserts that a
    /// hover style is set exactly once, and a primitive that layers its own
    /// accent over a shared one only trips that assertion when the screen
    /// holding it is actually rendered. Splitting this module into one file per
    /// screen would otherwise have been checked by the compiler and by nothing
    /// else.
    #[gpui::test]
    fn every_destination_renders_without_a_daemon(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = shell(cx);

        for destination in Destination::PRIMARY
            .into_iter()
            .chain([Destination::Settings])
        {
            shell.update(cx, |shell, cx| shell.go(destination, cx));
            draw(&shell, cx);
            cx.run_until_parked();
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.destination, destination);
            });
        }
    }

    /// The open rows are where the screens carry most of their controls, and a
    /// closed row renders none of them. This walks the two openable screens
    /// with every row revealed.
    #[gpui::test]
    fn every_open_row_renders_its_controls(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = shell(cx);

        shell.update(cx, |shell, cx| {
            shell.rows.toggle_cooling(Channel::Pump);
            shell.rows.toggle_cooling(Channel::Fan);
            for row in [
                LightingRow::Channel(1),
                LightingRow::Channel(2),
                LightingRow::Channel(3),
            ] {
                shell.rows.lighting.insert(row);
            }
            cx.notify();
        });

        for destination in [Destination::Cooling, Destination::Lighting] {
            shell.update(cx, |shell, cx| shell.go(destination, cx));
            draw(&shell, cx);
            cx.run_until_parked();
        }

        shell.read_with(cx, |shell, _| {
            assert!(shell.rows.cooling.contains(Channel::Pump));
            assert!(
                shell.is_open(LightingRow::Lcd),
                "the panel opens by default"
            );
        });
    }

    /// Leaving a screen closes whatever list was open on it: a menu anchored to
    /// a control that is no longer drawn would be left pointing at nothing.
    #[gpui::test]
    fn navigating_away_closes_the_open_list_and_disarms_the_deletion(
        cx: &mut gpui::TestAppContext,
    ) {
        let (shell, cx) = shell(cx);

        shell.update(cx, |shell, cx| {
            shell.toggle_popover(
                Popover::Options {
                    select: screen::SelectId::CoolingMode,
                },
                cx,
            );
            shell.confirm_delete = true;
        });
        shell.read_with(cx, |shell, _| assert!(shell.interaction.showing_any()));

        shell.update(cx, |shell, cx| shell.go(Destination::Lighting, cx));
        shell.read_with(cx, |shell, _| {
            assert!(!shell.interaction.showing_any());
            assert!(
                !shell.confirm_delete,
                "a primed delete must not survive the trip back"
            );
        });
    }

    #[test]
    fn the_shell_exposes_three_primary_destinations_and_one_secondary() {
        // The panel is not one of them. It is a device's appearance, so it is a
        // row on Lighting rather than a destination that would put the two
        // halves of the same question two clicks apart.
        assert_eq!(Destination::PRIMARY.len(), 3);
        assert_eq!(
            Destination::PRIMARY.map(Destination::label),
            ["Monitoring", "Cooling", "Lighting"]
        );
        assert!(!Destination::PRIMARY.contains(&Destination::Settings));
    }

    #[test]
    fn every_destination_has_a_label_and_its_own_icon() {
        let mut icons = Vec::new();
        for destination in Destination::PRIMARY
            .into_iter()
            .chain([Destination::Settings])
        {
            assert!(!destination.label().is_empty());
            icons.push(destination.icon());
        }
        // An entry identified by a shared icon is identified by its label
        // alone, which is what the rail's icons exist to avoid. Compared
        // pairwise rather than sorted: an icon is a name, and giving it an
        // order would invent a ranking the interface never uses.
        for (index, icon) in icons.iter().enumerate() {
            assert!(
                !icons[index + 1..].contains(icon),
                "two rail entries share {icon:?}"
            );
        }
    }
}
