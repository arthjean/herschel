// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! When a pending edit leaves the process.
//!
//! No screen has an Apply button: an edit reaches the hardware once the
//! operator stops moving it. That needs three facts and they are all here, for
//! both screens at once, because the two used to run the same mechanism twice
//! and the copies had drifted.
//!
//! The quiet period is not cosmetic. The daemon refuses a command arriving
//! inside its cadence floor rather than queueing it, and it keeps no
//! last-value-wins. A screen that wrote on every pointer move would have most
//! of its writes refused, and the value the operator actually stopped on could
//! be one of them.

use std::time::{Duration, Instant};

use super::drag::Drag;
use super::keyed::Keyed;
use super::row::LightingRow;

/// How still the Cooling screen has to be before an edit is written.
///
/// Long enough that a drag, a burst of arrow keys or a few stepper presses
/// count as one change, and short enough that letting go feels like the value
/// took. It also has to clear `MIN_PROGRAM_INTERVAL_MS`, the floor the daemon
/// enforces to keep the firmware from discarding writes that arrive too fast.
pub(crate) const COOLING_QUIET: Duration = Duration::from_millis(400);
/// How still one Lighting row has to be before its edit is written.
///
/// Shorter than [`COOLING_QUIET`] on purpose. That constant answers to
/// `MIN_PROGRAM_INTERVAL_MS`, the 200 ms floor of the thermal path; the lighting
/// floor is `MIN_COMMAND_INTERVAL_MS`, four times lower, and a color that takes
/// four tenths of a second to appear reads as a control that did not take.
/// Still several times the floor, so a drag or a held arrow key resolves to one
/// command rather than to a run of refusals.
const LIGHTING_QUIET: Duration = Duration::from_millis(150);
/// Something the screen writes on its own once the operator stops moving it.
///
/// The two screens ran the same mechanism twice: the cooling side kept a bare
/// `Option<Instant>` with its own touch, schedule, flush and due-check, and the
/// lighting side kept a schedule of rows. They differ in exactly two facts,
/// both carried here, so one deadline book and one timer body serve both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteTarget {
    Cooling,
    Lighting(LightingRow),
}
impl WriteTarget {
    /// How still this target has to be before its edit leaves the process.
    ///
    /// A property of what is being written, because the floor the daemon
    /// enforces is: the thermal path and the lighting path have different ones,
    /// and each is measured against its own.
    pub fn quiet(self) -> Duration {
        match self {
            Self::Cooling => COOLING_QUIET,
            Self::Lighting(_) => LIGHTING_QUIET,
        }
    }
}
/// Whether a write whose deadline has passed must still wait.
///
/// Free and pure, so the rule can be exercised without a window. Each reason
/// cost a real defect to find: a gesture the pointer has not released names a
/// value the operator never chose, and a cooling write whose outcome has not
/// come back would have the next outcome land against the wrong program.
///
/// The lighting side needs no in-flight guard: the daemon tracks its cadence
/// per channel and the rows are independent, so one row's reply is not
/// something another row is waiting on.
pub fn write_is_held_back(target: WriteTarget, drag: Option<Drag>, in_flight: bool) -> bool {
    match target {
        WriteTarget::Cooling => drag.is_some_and(Drag::is_cooling) || in_flight,
        WriteTarget::Lighting(row) => {
            matches!(drag, Some(Drag::Brightness { row: held, .. }) if held == row)
        }
    }
}
/// When each pending edit should leave the process.
///
/// One deadline per target rather than one for the window. The daemon tracks
/// its cadence floor per channel, so two rows edited in the same breath are two
/// independent writes and a single deadline would make one of them wait behind
/// the other for no reason.
///
/// A target edited again before its deadline pushes that deadline out, which is
/// how a drag or a held arrow key resolves to one command. Every edit spawns a
/// timer and every timer drains whatever has come due, so a timer that fires
/// into a moved deadline does nothing and no task has to be cancelled.
#[derive(Debug, Default)]
pub struct WriteSchedule {
    due: Keyed<WriteTarget, Instant>,
}
impl WriteSchedule {
    /// Arrange for `target` to be written once it has been still until `at`.
    pub fn touch(&mut self, target: WriteTarget, at: Instant) {
        self.due.set(target, at);
    }

    /// Every target whose deadline has passed, removed from the schedule.
    pub fn take_due(&mut self, now: Instant) -> Vec<WriteTarget> {
        self.due.take(|deadline| *deadline <= now)
    }

    /// Whether anything is waiting to be written for `target`.
    pub fn is_pending(&self, target: WriteTarget) -> bool {
        self.due.contains(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, Point, px};
    use kori_core::profile::{Channel, CurveNodes};

    #[test]
    fn a_due_write_still_waits_for_the_gesture_and_for_the_one_in_flight() {
        let track = Bounds {
            origin: Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: gpui::size(px(200.0), px(22.0)),
        };
        let duty = Drag::Duty {
            channel: Channel::Pump,
            track,
        };
        let brightness = Drag::Brightness {
            row: LightingRow::Channel(1),
            track,
        };

        assert!(!write_is_held_back(WriteTarget::Cooling, None, false));
        // Either cooling gesture holds it back, not just the curve. A timer
        // that fired mid-drag wrote the value under the cursor and then wrote
        // again on release: two commands for one gesture, and a duty nobody
        // chose.
        assert!(write_is_held_back(WriteTarget::Cooling, Some(duty), false));
        assert!(write_is_held_back(
            WriteTarget::Cooling,
            Some(Drag::Curve {
                channel: Channel::Fan,
                node: 0,
                base: CurveNodes::starting_ramp(),
                plot: track,
            }),
            false
        ));
        assert!(
            write_is_held_back(WriteTarget::Cooling, None, true),
            "one write in flight at a time, or an outcome lands against the wrong program"
        );

        // A brightness drag is not a cooling gesture, and holds back only the
        // row it is actually on.
        assert!(!write_is_held_back(
            WriteTarget::Cooling,
            Some(brightness),
            false
        ));
        assert!(write_is_held_back(
            WriteTarget::Lighting(LightingRow::Channel(1)),
            Some(brightness),
            false
        ));
        assert!(!write_is_held_back(
            WriteTarget::Lighting(LightingRow::Channel(2)),
            Some(brightness),
            false
        ));
        // The lighting rows are independent, so one reply is not something
        // another row waits on.
        assert!(!write_is_held_back(
            WriteTarget::Lighting(LightingRow::Lcd),
            None,
            true
        ));
    }

    #[test]
    fn each_target_waits_out_the_floor_its_own_daemon_path_enforces() {
        assert_eq!(WriteTarget::Cooling.quiet(), COOLING_QUIET);
        assert_eq!(
            WriteTarget::Lighting(LightingRow::Lcd).quiet(),
            LIGHTING_QUIET
        );
        assert!(
            LIGHTING_QUIET < COOLING_QUIET,
            "the lighting floor is four times lower than the thermal one"
        );
    }

    #[test]
    fn the_quiet_period_clears_the_floor_the_daemon_enforces() {
        // The daemon refuses a command arriving inside the floor rather than
        // queueing it, and keeps no last-value-wins, so a quiet period shorter
        // than the floor would turn a settled edit into a refusal.
        assert!(
            LIGHTING_QUIET > Duration::from_millis(kori_core::lighting::MIN_COMMAND_INTERVAL_MS),
            "an edit must not be written faster than the controller accepts"
        );
        // And short enough that the whole round trip stays inside the 500 ms
        // budgeted for a write reaching a confirmed state.
        assert!(
            LIGHTING_QUIET < Duration::from_millis(250),
            "an edit that takes a quarter of a second to leave reads as a control that did not take"
        );
    }

    #[test]
    fn each_target_waits_out_its_own_quiet_period() {
        let start = Instant::now();
        let mut schedule = WriteSchedule::default();
        schedule.touch(
            WriteTarget::Lighting(LightingRow::Channel(1)),
            start + LIGHTING_QUIET,
        );
        schedule.touch(
            WriteTarget::Lighting(LightingRow::Lcd),
            start + LIGHTING_QUIET * 3,
        );

        // Nothing is due before its own deadline, and one row coming due does
        // not drag the other out with it: the daemon tracks its cadence per
        // channel, so two rows edited together are two independent writes.
        assert!(schedule.take_due(start).is_empty());
        assert_eq!(
            schedule.take_due(start + LIGHTING_QUIET),
            vec![WriteTarget::Lighting(LightingRow::Channel(1))]
        );
        assert!(!schedule.is_pending(WriteTarget::Lighting(LightingRow::Channel(1))));
        assert!(schedule.is_pending(WriteTarget::Lighting(LightingRow::Lcd)));

        // A row edited again before its deadline is written once, at the later
        // deadline. That is what turns a drag into one command.
        schedule.touch(
            WriteTarget::Lighting(LightingRow::Lcd),
            start + LIGHTING_QUIET * 5,
        );
        assert!(schedule.take_due(start + LIGHTING_QUIET * 3).is_empty());
        assert_eq!(
            schedule.take_due(start + LIGHTING_QUIET * 5),
            vec![WriteTarget::Lighting(LightingRow::Lcd)]
        );
        assert!(!schedule.is_pending(WriteTarget::Lighting(LightingRow::Lcd)));
    }
}
