// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Cooling screen: the program, the two channels, and the curve.
//!
//! Every control here writes on its own once the edit settles, so an edit is
//! pending only for the moment between the operator letting go and the daemon
//! confirming it. What is left of the deliberate actions is the way back and
//! the two profile buttons.

use gpui::{Div, Pixels, Stateful, div, prelude::*, px};

use kori_core::KRAKEN_BASE;
use kori_core::capability::CapabilityId;
use kori_core::lighting::LightingCommand;
use kori_core::profile::{Channel, CoolingProgram, Profile, SAFE_PROFILE_NAME};

use crate::components::{
    Button, ButtonVariant, ControlState, Note, NoteLevel, SelectOption, panel_surface, row_panel,
};
use crate::cooling::CoolingMode;
use crate::feed::{Command, OutcomeSeverity};
use crate::shell::Shell;
use gpui::Context;
use std::time::Instant;

use super::drag::Drag;
use crate::theme::{RADIUS, color, space};

use super::tab::{
    COOLING_TAB_DELETE, COOLING_TAB_MODE, COOLING_TAB_PROFILE, COOLING_TAB_REVERT, COOLING_TAB_SAVE,
};
use super::write::{COOLING_QUIET, WriteTarget};
use super::{Caption, screen};

/// Width of the two selects that sit in the Cooling header.
///
/// Narrower than the 260 they used to hold, which is what leaves the coolant
/// readout beside them a column of its own at the 920-pixel target rather than
/// pushing it onto a line by itself.
pub const COOLING_SELECT_WIDTH: Pixels = px(228.0);
/// Side of the dot that marks a write still in flight.
const STATUS_DOT: Pixels = px(6.0);
/// The one moment a cooling edit is in flight, on its own mark.
///
/// Visible between the edit settling and the daemon confirming it, and gone
/// after. A reading that stays put means a write that did not land, which is the
/// only thing on this line worth looking at, so it carries a fill and a dot
/// rather than being a fourth gray sentence beside three buttons. The fill is
/// the neutral control color, not the warning one: this is the normal path of
/// every edit, and a screen that flashes amber on success teaches its operator
/// to ignore amber.
fn write_status() -> Div {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .rounded(RADIUS)
        .bg(color::CONTROL.alpha(0.6))
        .child(
            div()
                .flex_none()
                .w(STATUS_DOT)
                .h(STATUS_DOT)
                .rounded(STATUS_DOT / 2.0)
                .bg(color::ACCENT.hsla()),
        )
        .child(
            div()
                .min_w_0()
                .text_sm()
                .text_color(color::TEXT_MUTED.hsla())
                .child("Saving. The hardware holds its previous program until this clears."),
        )
}
/// The committed program a fresh status should put in the editor, if any.
///
/// Free and pure, for the same reason [`write_is_held_back`] is. `seen` is
/// what the last adoption took, so a status repeating itself adopts nothing and
/// a screen the operator is reading does not twitch once a second.
///
/// Nothing is adopted while an edit is in flight: a write waiting out its quiet
/// period, a gesture still running, or a command whose outcome has not come back
/// is the operator's intent, and what the daemon committed is the state that
/// intent is about to replace. A refused edit stays on screen by the same rule,
/// since a refusal changes nothing the daemon has committed.
pub fn program_to_adopt(
    committed: Option<&CoolingProgram>,
    seen: Option<&CoolingProgram>,
    editing: bool,
) -> Option<CoolingProgram> {
    let committed = committed?;
    if editing || seen == Some(committed) {
        return None;
    }
    Some(committed.clone())
}
/// State of the delete control for the currently active profile.
///
/// `None` means no daemon answered. The built-in safe profile is refused here
/// for the same reason the daemon refuses it: it is what everything else falls
/// back to, so it has to survive every other profile.
pub fn delete_state(active: Option<&str>) -> ControlState {
    match active {
        None => ControlState::disabled("The background service is not running."),
        Some(SAFE_PROFILE_NAME) => ControlState::disabled(
            "The built-in safe profile cannot be deleted. It is what the daemon falls back to \
             when anything else is unavailable.",
        ),
        Some(_) => ControlState::Enabled,
    }
}
/// What one activation of the delete control does.
///
/// Returns the armed flag to keep and the command to send, if any. The first
/// activation only arms: deleting the configuration an operator is running is
/// not something a stray click should accomplish.
pub fn next_deletion(active: Option<&str>, armed: bool) -> (bool, Option<Command>) {
    let Some(name) = active else {
        return (false, None);
    };
    if name == SAFE_PROFILE_NAME {
        return (false, None);
    }
    if armed {
        (false, Some(Command::DeleteProfile(name.to_string())))
    } else {
        (true, None)
    }
}

impl Shell {
    pub(crate) fn cooling(&self, cx: &mut Context<Self>) -> Div {
        let now = self.now_unix_ms;
        let kraken = self.kraken();
        // Each channel is gated on its own capability. The probe resolves
        // `pwm1` and `pwm2` independently, so a udev rule covering one channel
        // leaves the other read-only, and a control offered on that channel
        // would be one the hardware never accepts.
        let pump_write = self
            .link
            .cooling_state(KRAKEN_BASE, Channel::Pump.duty_capability(), now);
        let fan_write = self
            .link
            .cooling_state(KRAKEN_BASE, Channel::Fan.duty_capability(), now);
        let pending = self.cooling.pending(kraken);
        let invalid = self.cooling.validation_error();

        // A program writes every channel it names, so it is refused unless all
        // of them are writable. The capability list is the daemon's own, so a
        // program that passes here is one the daemon would accept. With no
        // Apply button to disable, this is surfaced as a note instead: an
        // autosaving screen that silently saves nothing would be worse than one
        // that refuses out loud.
        let program_state = match &invalid {
            Some(error) => ControlState::error(error.to_string()),
            None => self.link.program_state(
                KRAKEN_BASE,
                &self.cooling.program().required_capabilities(),
                now,
            ),
        };

        let mode_options: Vec<SelectOption> = CoolingMode::ALL
            .into_iter()
            .map(|mode| SelectOption::new(mode.value(), mode.label()))
            .collect();
        let profiles: Vec<SelectOption> = self
            .link
            .profiles()
            .iter()
            .map(|profile| SelectOption::new(profile.name.clone(), profile.name.clone()))
            .collect();
        let active = self
            .link
            .active_profile()
            .unwrap_or(SAFE_PROFILE_NAME)
            .to_string();

        let mut surface = screen(
            "Cooling",
            "Pump, fan and the onboard liquid-temperature curve.",
        )
        .child(
            // The program on its own surface, above the channels it governs.
            // Which program is running and which profile selected it is one
            // question, and it is a different one from what each channel is
            // doing about it, so the two are separate cards rather than one
            // list with a heading.
            panel_surface().child(
                div()
                    .flex()
                    .flex_wrap()
                    // Every caption on one line, whichever of the two
                    // selects is the taller once a message sits under it.
                    .items_start()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .child(div().flex_none().w(COOLING_SELECT_WIDTH).child(self.select(
                        "cooling-mode",
                        "Mode",
                        Caption::Shown,
                        mode_options,
                        self.cooling.mode.value().to_string(),
                        // Choosing a mode is an edit like any other now, so
                        // it carries the same per-capability gate the
                        // profile selector beside it does, and for the same
                        // reason.
                        self.link.write_state(),
                        COOLING_TAB_MODE,
                        cx,
                        |shell, value, cx| {
                            if let Some(mode) = CoolingMode::from_value(value) {
                                shell.cooling.set_mode(mode);
                                shell.schedule_write(WriteTarget::Cooling, cx);
                            }
                        },
                    )))
                    .child(div().flex_none().w(COOLING_SELECT_WIDTH).child(self.select(
                        "profile",
                        "Active profile",
                        Caption::Shown,
                        profiles,
                        active,
                        // Activating a profile is a write. It is disabled
                        // for the same reasons every other write control is.
                        self.link.write_state(),
                        COOLING_TAB_PROFILE,
                        cx,
                        |shell, value, _| {
                            shell.feed.send(Command::ActivateProfile(value.to_string()));
                        },
                    ))),
            ),
        );

        for alert in self.link.alerts() {
            surface = surface.child(Note::new(NoteLevel::Critical, alert.message()).render());
        }

        surface
            .child(
                // No heading: the two rows name themselves, and the freshness
                // the subtitle used to carry is on every reading already,
                // through the Stale and N/A qualifiers `readback` adds.
                row_panel()
                    .child(self.channel_row(Channel::Pump, &pump_write, cx))
                    .child(self.channel_row(Channel::Fan, &fan_write, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(space::SM)
                            .child(
                                // The only way back. An edit is already on the
                                // hardware, so this is not an undo: it puts the
                                // editor back on what the device reports it is
                                // running, which is what an operator needs after
                                // a refusal or after an edit they did not mean
                                // to make.
                                Button::new("cooling-revert", "Revert to hardware")
                                    .tab_index(COOLING_TAB_REVERT)
                                    .render()
                                    .on_click(cx.listener(|shell, _, _, cx| {
                                        let kraken = shell.kraken().cloned();
                                        shell.cooling.cancel(kraken.as_ref());
                                        shell.sent = None;
                                        shell.schedule_write(WriteTarget::Cooling, cx);
                                    })),
                            )
                            .child(
                                Button::new("cooling-save", "Save as profile")
                                    .state(self.link.write_state())
                                    .tab_index(COOLING_TAB_SAVE)
                                    .render()
                                    .on_click(
                                        cx.listener(|shell, _, _, cx| shell.save_profile(cx)),
                                    ),
                            )
                            .child(self.delete_button(COOLING_TAB_DELETE, cx)),
                    )
                    // Opposite the buttons rather than after them: the status of
                    // a write is not a fourth action, and a sentence that shares
                    // a line with three buttons reads as a label for the last
                    // one. It is the only thing on its side of the line, so it
                    // appears and disappears without moving anything.
                    .children(pending.then(write_status)),
            )
            .children(
                program_state
                    .message()
                    .map(|reason| Note::new(NoteLevel::Warning, reason.to_string()).render()),
            )
            .children(self.outcome_note())
    }

    /// Delete the active profile, in two deliberate activations.
    ///
    /// The daemon activates the built-in safe profile before it removes the
    /// file, so the window between the two states is one where the hardware is
    /// on the program that writes nothing rather than on none at all.
    fn delete_button(&self, tab_index: isize, cx: &mut Context<Self>) -> Stateful<Div> {
        Button::new(
            "cooling-delete",
            if self.confirm_delete {
                "Confirm deletion"
            } else {
                "Delete profile"
            },
        )
        .variant(ButtonVariant::Danger)
        .state(delete_state(self.link.active_profile()))
        .tab_index(tab_index)
        .render()
        .on_click(cx.listener(|shell, _, _, cx| shell.delete_profile(cx)))
    }

    /// Arm the deletion, then perform it on the second activation.
    fn delete_profile(&mut self, cx: &mut Context<Self>) {
        let (armed, command) = next_deletion(self.link.active_profile(), self.confirm_delete);
        self.confirm_delete = armed;
        if let Some(command) = command {
            self.feed.send(command);
        }
        cx.notify();
    }

    /// Put the daemon's committed program in the editor when it changes.
    ///
    /// This is what makes the Cooling screen open on the machine as it is
    /// rather than on the factory arrangement, and what shows the curve of a
    /// profile the operator just activated: an activation is the daemon
    /// committing a program like any other, so one rule covers both.
    pub(crate) fn adopt_committed_program(&mut self) {
        let editing = self.drag.is_some_and(Drag::is_cooling)
            || self.due.is_pending(WriteTarget::Cooling)
            || self.sent.is_some();
        let Some(program) = program_to_adopt(
            self.link
                .status()
                .and_then(|status| status.cooling.as_ref()),
            self.committed.as_ref(),
            editing,
        ) else {
            return;
        };
        self.cooling.adopt(&program);
        self.committed = Some(program);
    }

    /// Note that the cooling edit changed, without spawning a timer.
    ///
    /// Used by the moves inside a drag: they arrive by the dozen and only the
    /// one the operator stops on is worth writing, so they push the deadline
    /// out and let the release schedule the write.
    pub(crate) fn touch_cooling(&mut self) {
        self.due
            .touch(WriteTarget::Cooling, Instant::now() + COOLING_QUIET);
    }

    /// Send the pending cooling program, remembering it until its outcome
    /// arrives.
    ///
    /// Silent when there is nothing to write or nothing the daemon would
    /// accept: a refusal is already on screen in both cases.
    pub(crate) fn send_cooling(&mut self) {
        if !self.cooling.pending(self.kraken()) || self.cooling.validation_error().is_some() {
            return;
        }
        let program = self.cooling.program();
        self.sent = Some(program.clone());
        self.feed.send(Command::Apply(program));
    }

    /// Store the pending state of the whole machine under a generated name.
    ///
    /// Cooling, lighting and the panel together, because that is what
    /// reactivating a profile puts back: a profile that carried only the curve
    /// would silently leave the lights and the glass on whatever the previous
    /// selection left there, and the operator would have no way to tell which
    /// half of the machine a name refers to.
    ///
    /// A channel whose color is mid-edit is left out rather than stored
    /// half-typed, exactly as it is left out of the write that settles, and the
    /// panel is stored only when this machine has one to write to.
    fn save_profile(&mut self, cx: &mut Context<Self>) {
        let existing = self.link.profiles().len();
        let name = format!("{} {}", self.cooling.mode.label(), existing);
        let lighting = self
            .lighting
            .channels()
            .iter()
            .filter_map(|editor| {
                editor.program().ok().map(|program| LightingCommand {
                    channel: editor.channel,
                    program,
                })
            })
            .collect();
        let display = self
            .link
            .control_state(KRAKEN_BASE, CapabilityId::LcdFrame)
            .is_enabled()
            .then(|| self.lcd.preset().ok())
            .flatten();
        let profile = Profile {
            name: name.chars().take(48).collect(),
            program: self.cooling.program(),
            device: Some(KRAKEN_BASE),
            lighting,
            display,
        };
        self.feed.send(Command::SaveProfile(profile));
        cx.notify();
    }

    /// The result of the last command, as a note at the foot of the screen.
    ///
    /// Only when something went wrong. A confirmed write already tells the
    /// operator everything it has to: the readings move, the row's reported
    /// program catches up, and the pending state clears. Restating that in a
    /// banner after every edit is noise that trains the eye to skip the banner,
    /// which is the one place a refusal or an unconfirmed write has to be
    /// noticed.
    ///
    /// One note for a screen whose rows all write on their own, so the message
    /// has to name its device. `Channel 2: ...` and `Panel: ...` are what keep
    /// it from being read against whichever row the operator is looking at.
    pub(crate) fn outcome_note(&self) -> Option<Div> {
        let outcome = self.outcome.as_ref()?;
        let level = match outcome.severity {
            OutcomeSeverity::Confirmed => return None,
            OutcomeSeverity::Unconfirmed => NoteLevel::Critical,
            OutcomeSeverity::Refused => NoteLevel::Warning,
        };
        Some(Note::new(level, outcome.message.clone()).render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_program_is_adopted_once_and_never_over_an_edit() {
        let committed = CoolingProgram::Fixed { pump: 180, fan: 90 };
        let other = CoolingProgram::Fixed { pump: 200, fan: 90 };

        assert_eq!(
            program_to_adopt(Some(&committed), None, false),
            Some(committed.clone()),
            "a window opening on a running machine takes what it is running"
        );
        assert_eq!(
            program_to_adopt(Some(&committed), Some(&committed), false),
            None,
            "a status repeating itself must not redraw the screen every second"
        );
        assert_eq!(
            program_to_adopt(Some(&other), Some(&committed), false),
            Some(other.clone()),
            "activating a profile is the daemon committing a program, and the \
             plot has to follow it"
        );
        assert_eq!(
            program_to_adopt(Some(&other), Some(&committed), true),
            None,
            "an edit in flight is the operator's intent, and it outranks the \
             state that intent is about to replace"
        );
        assert_eq!(
            program_to_adopt(None, Some(&committed), false),
            None,
            "a daemon that has committed nothing says nothing about the plot"
        );
    }

    #[test]
    fn deleting_a_profile_takes_two_deliberate_activations() {
        // The first activation only arms the control.
        let (armed, command) = next_deletion(Some("Silent"), false);
        assert!(armed);
        assert_eq!(command, None, "one press must not delete anything");

        // The second sends the command the daemon acts on.
        let (armed, command) = next_deletion(Some("Silent"), true);
        assert!(!armed, "the control disarms once it has fired");
        assert_eq!(command, Some(Command::DeleteProfile("Silent".to_string())));
    }

    #[test]
    fn the_built_in_safe_profile_can_never_be_deleted() {
        for armed in [false, true] {
            assert_eq!(next_deletion(Some(SAFE_PROFILE_NAME), armed), (false, None));
            assert_eq!(next_deletion(None, armed), (false, None));
        }

        let state = delete_state(Some(SAFE_PROFILE_NAME));
        assert!(state.is_disabled());
        assert!(state.message().unwrap().contains("falls back"), "{state:?}");

        assert!(delete_state(None).is_disabled());
        assert!(delete_state(Some("Silent")).is_enabled());
    }
}
