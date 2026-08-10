// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The pending state of the Cooling screen.
//!
//! Moving a slider or a curve node changes this structure and nothing else.
//! The screen writes on its own once the edit settles, so an edit is pending
//! only for the moment between the operator letting go and the daemon
//! confirming it. Keeping that pending edit separate from the confirmed
//! readback is what lets the screen tell the two apart, and it is the whole
//! basis for saying a write did not land.

use herschel_core::profile::{
    CURVE_NODE_COUNT, Channel, CoolingProgram, CurveNodes, MAX_DUTY, MAX_DUTY_PERCENT,
    ValidationError, duty_from_percent, duty_to_percent, validate_program,
};
use herschel_core::telemetry::{KrakenTelemetry, PwmMode};

/// What the Cooling screen is asking the hardware to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoolingMode {
    /// Leave the device on its own program. Writes nothing.
    Onboard,
    Fixed,
    Curve,
}

impl CoolingMode {
    pub const ALL: [CoolingMode; 3] = [Self::Onboard, Self::Fixed, Self::Curve];

    pub fn value(self) -> &'static str {
        match self {
            Self::Onboard => "onboard",
            Self::Fixed => "fixed",
            Self::Curve => "curve",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Onboard => "Onboard (device default)",
            Self::Fixed => "Fixed duty",
            Self::Curve => "Liquid temperature curve",
        }
    }

    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.value() == value)
    }
}

/// The pending program, plus what the last Apply confirmed.
#[derive(Debug, Clone, PartialEq)]
pub struct CoolingEditor {
    pub mode: CoolingMode,
    pub pump_duty: u8,
    pub fan_duty: u8,
    pump_curve: CurveNodes,
    fan_curve: CurveNodes,
    /// Channels whose rows are open, each editable on its own.
    ///
    /// A list rather than a selection that always points somewhere: with every
    /// row closed the screen renders no editing control at all, so there is no
    /// channel an edit could silently land on. Rows open independently, so the
    /// pump curve and the fan curve can be compared side by side.
    expanded: Vec<Channel>,
    /// Selected curve node per channel, the one keyboard edits move.
    ///
    /// One per channel rather than one for the screen: with both rows open, a
    /// shared index would move a node on whichever plot was focused last.
    pump_node: usize,
    fan_node: usize,
    /// The last program this client saw confirmed on the hardware.
    applied: Option<CoolingProgram>,
}

impl Default for CoolingEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl CoolingEditor {
    pub fn new() -> Self {
        Self {
            mode: CoolingMode::Onboard,
            // On the grid, in whole percent. A default that was not would never
            // read back as itself: the device would answer with the nearest
            // percentage, the screen would call that a difference, and it would
            // sit on "Saving" forever without anything being wrong.
            pump_duty: duty_from_percent(70),
            fan_duty: duty_from_percent(50),
            pump_curve: CurveNodes::starting_ramp(),
            fan_curve: CurveNodes::starting_ramp(),
            expanded: Vec::new(),
            pump_node: 0,
            fan_node: 0,
            applied: None,
        }
    }

    /// The program the screen would write.
    pub fn program(&self) -> CoolingProgram {
        match self.mode {
            CoolingMode::Onboard => CoolingProgram::Onboard,
            CoolingMode::Fixed => CoolingProgram::Fixed {
                pump: self.pump_duty,
                fan: self.fan_duty,
            },
            CoolingMode::Curve => CoolingProgram::Curve {
                pump: self.pump_curve.interpolate(),
                fan: self.fan_curve.interpolate(),
            },
        }
    }

    pub fn curve(&self, channel: Channel) -> &CurveNodes {
        match channel {
            Channel::Pump => &self.pump_curve,
            Channel::Fan => &self.fan_curve,
        }
    }

    pub fn duty(&self, channel: Channel) -> u8 {
        match channel {
            Channel::Pump => self.pump_duty,
            Channel::Fan => self.fan_duty,
        }
    }

    /// Set a fixed duty, clamped into the channel's validated range.
    ///
    /// Clamping in the editor is what keeps an impossible value from ever
    /// becoming an edit worth writing; the daemon still revalidates, because
    /// the client is not a trusted input.
    ///
    /// Deliberately does *not* snap the value onto the percent grid, though
    /// every edit that reaches it does. This is also the setter that loads what
    /// the device reports, and snapping a readback would move it: a reading of
    /// 180 would become 181, the editor would differ from the hardware the
    /// instant it synced, and with autosave that difference would be written
    /// straight back. Snapping belongs where the operator's intent is turned
    /// into a value, in [`CoolingEditor::adjust_duty`] and in `node_at`.
    pub fn set_duty(&mut self, channel: Channel, duty: u8) {
        let clamped = duty.clamp(channel.min_duty(), MAX_DUTY);
        match channel {
            Channel::Pump => self.pump_duty = clamped,
            Channel::Fan => self.fan_duty = clamped,
        }
    }

    /// Move the duty by `steps` percentage points.
    ///
    /// Percentage points, not raw duty, because one point is exactly what the
    /// device distinguishes: the driver stores a percentage. Stepping by raw
    /// duty meant some presses moved the hardware two settings and some moved
    /// it none.
    pub fn adjust_duty(&mut self, channel: Channel, steps: i16) {
        let current = duty_to_percent(self.duty(channel)) as i16;
        let next = (current + steps).clamp(0, MAX_DUTY_PERCENT as i16);
        self.set_duty(channel, duty_from_percent(next as u8));
    }

    /// Move the selection along the curve.
    /// The node keyboard edits move on `channel`.
    pub fn node(&self, channel: Channel) -> usize {
        match channel {
            Channel::Pump => self.pump_node,
            Channel::Fan => self.fan_node,
        }
    }

    fn node_mut(&mut self, channel: Channel) -> &mut usize {
        match channel {
            Channel::Pump => &mut self.pump_node,
            Channel::Fan => &mut self.fan_node,
        }
    }

    pub fn step_node(&mut self, channel: Channel, delta: isize) {
        let next = self.node(channel) as isize + delta;
        *self.node_mut(channel) = next.clamp(0, CURVE_NODE_COUNT as isize - 1) as usize;
    }

    pub fn select_node(&mut self, channel: Channel, index: usize) {
        *self.node_mut(channel) = index.min(CURVE_NODE_COUNT - 1);
    }

    /// Whether `channel`'s row is open.
    pub fn is_expanded(&self, channel: Channel) -> bool {
        self.expanded.contains(&channel)
    }

    /// Every open channel, in the order the screen draws them.
    pub fn expanded(&self) -> &[Channel] {
        &self.expanded
    }

    /// Open a channel's row, or close it when it is already open.
    ///
    /// The rows are independent: each plot publishes its own rectangle and
    /// carries its own selected node, so two open at once edit two curves
    /// rather than one meaning two things.
    pub fn toggle(&mut self, channel: Channel) {
        match self.expanded.iter().position(|open| *open == channel) {
            Some(index) => {
                self.expanded.remove(index);
            }
            None => self.expanded.push(channel),
        }
        *self.node_mut(channel) = 0;
    }

    /// Set one node of a channel's curve.
    ///
    /// The duty floor of the channel applies to every node, so a pump curve
    /// can never be edited into stopping the pump.
    ///
    /// Editing a node also selects the curve mode. Moving a point the program
    /// would then ignore is an edit with no effect, and the alternative, a plot
    /// that refuses to move until a select above it is changed first, is the
    /// same rule stated less clearly.
    pub fn set_node(&mut self, channel: Channel, index: usize, duty: u8) {
        let base = *self.curve(channel);
        self.set_node_from(channel, base, index, duty);
    }

    /// Set one node against `base` rather than against the current curve.
    ///
    /// [`CurveNodes::set`] keeps the set monotonic by pushing the nodes a moved
    /// one crosses, which is what the curve ABI requires and what
    /// `validate_curve` enforces. Applied repeatedly to its own output it is
    /// also lossy: dragging a node down and back up in one gesture would leave
    /// the nodes it pushed on the way down flattened, because the second step
    /// no longer knows where they were.
    ///
    /// A drag therefore replays from the curve as it stood when the pointer
    /// went down. Every intermediate position is computed from the same base,
    /// so the gesture is idempotent and the operator can reach the value they
    /// meant without the plot eroding underneath them.
    pub fn set_node_from(&mut self, channel: Channel, base: CurveNodes, index: usize, duty: u8) {
        let mut nodes = base;
        nodes.set(index, duty.clamp(channel.min_duty(), MAX_DUTY));
        *self.curve_mut(channel) = nodes;
        self.mode = CoolingMode::Curve;
    }

    /// Move the selected node by `steps` percentage points, as
    /// [`CoolingEditor::adjust_duty`] does and for the same reason.
    pub fn adjust_node(&mut self, channel: Channel, steps: i16) {
        let current = duty_to_percent(self.curve(channel).duty[self.node(channel)]) as i16;
        let next = (current + steps).clamp(0, MAX_DUTY_PERCENT as i16);
        let node = self.node(channel);
        self.set_node(channel, node, duty_from_percent(next as u8));
    }

    fn curve_mut(&mut self, channel: Channel) -> &mut CurveNodes {
        match channel {
            Channel::Pump => &mut self.pump_curve,
            Channel::Fan => &mut self.fan_curve,
        }
    }

    pub fn set_mode(&mut self, mode: CoolingMode) {
        self.mode = mode;
    }

    /// Record the program the daemon confirmed.
    pub fn record_applied(&mut self, program: CoolingProgram) {
        self.applied = Some(program);
    }

    /// Open on the program the daemon reports it has committed.
    ///
    /// The editor opens on defaults while the hardware keeps running whatever
    /// the daemon last wrote, so a window opened a second time showed a curve
    /// the machine was not on and the operator had to draw theirs again. The
    /// shape arrives from the daemon's record rather than from telemetry
    /// because the device publishes no attribute that returns a curve.
    ///
    /// Adopting is also recording: the program came from the daemon saying it
    /// committed one, which is the same confirmation
    /// [`CoolingEditor::record_applied`] exists for, so nothing is left
    /// pending against a write that already landed.
    pub fn adopt(&mut self, program: &CoolingProgram) {
        match program {
            CoolingProgram::Onboard => self.mode = CoolingMode::Onboard,
            CoolingProgram::Fixed { pump, fan } => {
                self.set_duty(Channel::Pump, *pump);
                self.set_duty(Channel::Fan, *fan);
                self.mode = CoolingMode::Fixed;
            }
            CoolingProgram::Curve { pump, fan } => {
                // A curve this editor produced round-trips through the node
                // set it was interpolated from. One that does not came from
                // somewhere else, and the plot is left as it stands rather
                // than showing an arbitrary reading of it.
                let (Some(pump), Some(fan)) =
                    (CurveNodes::from_curve(pump), CurveNodes::from_curve(fan))
                else {
                    return;
                };
                self.pump_curve = pump;
                self.fan_curve = fan;
                self.mode = CoolingMode::Curve;
            }
        }
        self.applied = Some(program.clone());
    }

    /// Whether the pending edit differs from what the hardware confirms.
    ///
    /// A fixed program is checked against the readback, which the driver
    /// publishes. A curve cannot be read back at all, so it is checked against
    /// this client's record of the last confirmed write *and* the mode the
    /// device reports: both have to agree before the edit stops being pending.
    pub fn pending(&self, kraken: Option<&KrakenTelemetry>) -> bool {
        let program = self.program();
        match &program {
            CoolingProgram::Onboard => false,
            CoolingProgram::Fixed { pump, fan } => {
                !confirms_fixed(kraken, Channel::Pump, *pump)
                    || !confirms_fixed(kraken, Channel::Fan, *fan)
            }
            CoolingProgram::Curve { .. } => {
                self.applied.as_ref() != Some(&program)
                    || !confirms_mode(kraken, Channel::Pump, PwmMode::Curve)
                    || !confirms_mode(kraken, Channel::Fan, PwmMode::Curve)
            }
        }
    }

    /// Discard pending edits and return to what the hardware reports.
    pub fn cancel(&mut self, kraken: Option<&KrakenTelemetry>) {
        if let Some(CoolingProgram::Curve { pump, fan }) = &self.applied
            && let (Some(pump), Some(fan)) =
                (CurveNodes::from_curve(pump), CurveNodes::from_curve(fan))
        {
            self.pump_curve = pump;
            self.fan_curve = fan;
        }

        let Some(kraken) = kraken else {
            self.mode = CoolingMode::Onboard;
            return;
        };

        if let Some(duty) = kraken.pump.duty.copied() {
            self.set_duty(Channel::Pump, duty);
        }
        if let Some(duty) = kraken.fan.duty.copied() {
            self.set_duty(Channel::Fan, duty);
        }
        self.mode = match kraken.pump.mode.copied() {
            Some(PwmMode::Fixed) => CoolingMode::Fixed,
            Some(PwmMode::Curve) => CoolingMode::Curve,
            _ => CoolingMode::Onboard,
        };
    }

    /// The reason this edit would be refused before it is even sent, if any.
    pub fn validation_error(&self) -> Option<ValidationError> {
        validate_program(&self.program()).err()
    }
}

fn confirms_fixed(kraken: Option<&KrakenTelemetry>, channel: Channel, duty: u8) -> bool {
    let Some(kraken) = kraken else { return false };
    let entry = kraken.channel(channel);
    entry.mode.copied() == Some(PwmMode::Fixed) && entry.duty.copied() == Some(duty)
}

fn confirms_mode(kraken: Option<&KrakenTelemetry>, channel: Channel, mode: PwmMode) -> bool {
    kraken.is_some_and(|kraken| kraken.channel(channel).mode.copied() == Some(mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use herschel_core::profile::MIN_PUMP_DUTY;
    use herschel_core::telemetry::{ChannelTelemetry, Reading};

    fn kraken(mode: PwmMode, pump_duty: u8, fan_duty: u8) -> KrakenTelemetry {
        KrakenTelemetry {
            at_unix_ms: 1_000,
            present: true,
            liquid_temperature_c: Reading::valid(29.8),
            pump: ChannelTelemetry {
                channel: Channel::Pump,
                rpm: Reading::valid(2_970),
                duty: Reading::valid(pump_duty),
                mode: Reading::valid(mode),
            },
            fan: ChannelTelemetry {
                channel: Channel::Fan,
                rpm: Reading::valid(1_764),
                duty: Reading::valid(fan_duty),
                mode: Reading::valid(mode),
            },
        }
    }

    #[test]
    fn editing_never_leaves_the_pump_below_its_validated_floor() {
        let mut editor = CoolingEditor::new();
        editor.set_duty(Channel::Pump, 0);
        assert_eq!(editor.pump_duty, MIN_PUMP_DUTY);

        for _ in 0..100 {
            editor.adjust_duty(Channel::Pump, -1);
        }
        assert_eq!(editor.pump_duty, MIN_PUMP_DUTY);

        // The fan is allowed to stop.
        editor.set_duty(Channel::Fan, 0);
        assert_eq!(editor.fan_duty, 0);
    }

    /// One press is one percentage point, on both the stepper and the plot.
    /// The old five-step-out-of-255 press was a shade under 2%, which meant
    /// some presses moved the hardware two settings and some moved it one.
    #[test]
    fn one_press_moves_the_value_by_exactly_one_percent() {
        let mut editor = CoolingEditor::new();
        editor.set_duty(Channel::Fan, duty_from_percent(50));

        for expected in 51..=55 {
            editor.adjust_duty(Channel::Fan, 1);
            assert_eq!(duty_to_percent(editor.duty(Channel::Fan)), expected);
        }
        for expected in (50..=54).rev() {
            editor.adjust_duty(Channel::Fan, -1);
            assert_eq!(duty_to_percent(editor.duty(Channel::Fan)), expected);
        }

        editor.select_node(Channel::Fan, 4);
        editor.set_node(Channel::Fan, 4, duty_from_percent(60));
        editor.adjust_node(Channel::Fan, 1);
        assert_eq!(duty_to_percent(editor.curve(Channel::Fan).duty[4]), 61);
    }

    /// Every value the editor can hold without being told one has to be a value
    /// the device can report back, or the screen claims a difference that no
    /// edit can ever close.
    #[test]
    fn the_editor_opens_on_values_the_device_can_report() {
        let editor = CoolingEditor::new();
        for duty in [editor.duty(Channel::Pump), editor.duty(Channel::Fan)] {
            assert_eq!(duty_from_percent(duty_to_percent(duty)), duty, "{duty}");
        }

        // And the round trip really closes: a device echoing what it was told
        // leaves nothing pending.
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Fixed);
        let echoed = kraken(
            PwmMode::Fixed,
            editor.duty(Channel::Pump),
            editor.duty(Channel::Fan),
        );
        assert!(!editor.pending(Some(&echoed)));
    }

    /// A drag can only produce values the device can hold, so letting go twice
    /// on the same pixel row produces the same byte and the daemon really does
    /// deduplicate it.
    #[test]
    fn a_dragged_node_lands_on_a_whole_percentage() {
        let bounds = gpui::Bounds {
            origin: gpui::Point {
                x: gpui::px(0.0),
                y: gpui::px(0.0),
            },
            size: gpui::Size {
                width: gpui::px(400.0),
                height: gpui::px(200.0),
            },
        };

        for step in 0..=200 {
            let position = gpui::Point {
                x: gpui::px(200.0),
                y: gpui::px(step as f32),
            };
            let (_, duty) = crate::components::node_at(bounds, position);
            assert_eq!(
                duty_from_percent(duty_to_percent(duty)),
                duty,
                "row {step} produced duty {duty}, which is not a whole percentage"
            );
        }
    }

    #[test]
    fn duty_never_climbs_past_full_scale() {
        let mut editor = CoolingEditor::new();
        for _ in 0..200 {
            editor.adjust_duty(Channel::Fan, 1);
        }
        assert_eq!(editor.fan_duty, MAX_DUTY);
    }

    #[test]
    fn every_edit_keeps_the_program_valid() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Curve);

        for (index, duty) in [(0, 0), (9, 30), (4, 255), (7, 60)] {
            editor.select_node(Channel::Pump, index);
            editor.set_node(Channel::Pump, index, duty);
            assert_eq!(
                editor.validation_error(),
                None,
                "after set({index}, {duty})"
            );
        }

        editor.set_mode(CoolingMode::Fixed);
        editor.set_duty(Channel::Pump, 0);
        assert_eq!(editor.validation_error(), None);
    }

    #[test]
    fn a_pump_curve_node_cannot_be_edited_below_the_pump_floor() {
        let mut editor = CoolingEditor::new();
        editor.select_node(Channel::Pump, 3);
        editor.set_node(Channel::Pump, 3, 0);
        assert!(
            editor
                .curve(Channel::Pump)
                .duty
                .iter()
                .all(|duty| *duty >= MIN_PUMP_DUTY)
        );
    }

    #[test]
    fn node_selection_stays_inside_the_curve() {
        let mut editor = CoolingEditor::new();
        editor.step_node(Channel::Pump, -5);
        assert_eq!(editor.node(Channel::Pump), 0);
        editor.step_node(Channel::Pump, 100);
        assert_eq!(editor.node(Channel::Pump), CURVE_NODE_COUNT - 1);
        editor.select_node(Channel::Pump, 999);
        assert_eq!(editor.node(Channel::Pump), CURVE_NODE_COUNT - 1);

        // Each channel keeps its own selection: with both rows open, walking
        // the nodes of one plot must not move the point selected on the other.
        assert_eq!(editor.node(Channel::Fan), 0, "the fan kept its own node");
    }

    #[test]
    fn a_fixed_edit_stays_pending_until_the_readback_agrees() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Fixed);
        editor.set_duty(Channel::Pump, 180);
        editor.set_duty(Channel::Fan, 128);

        assert!(editor.pending(None), "no telemetry means nothing confirmed");
        assert!(editor.pending(Some(&kraken(PwmMode::FullSpeed, 255, 255))));
        assert!(
            editor.pending(Some(&kraken(PwmMode::Fixed, 200, 128))),
            "a different duty is still pending"
        );
        assert!(!editor.pending(Some(&kraken(PwmMode::Fixed, 180, 128))));
    }

    #[test]
    fn a_curve_edit_needs_both_the_record_and_the_reported_mode() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Curve);
        let program = editor.program();

        assert!(editor.pending(Some(&kraken(PwmMode::Curve, 200, 200))));

        editor.record_applied(program);
        assert!(
            editor.pending(Some(&kraken(PwmMode::Fixed, 200, 200))),
            "the device is not in curve mode"
        );
        assert!(!editor.pending(Some(&kraken(PwmMode::Curve, 200, 200))));

        // Touching a node makes it pending again.
        editor.select_node(Channel::Pump, 2);
        editor.adjust_node(Channel::Pump, 1);
        assert!(editor.pending(Some(&kraken(PwmMode::Curve, 200, 200))));
    }

    #[test]
    fn a_drag_replayed_from_its_base_is_reversible_within_the_gesture() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Curve);
        let base = *editor.curve(Channel::Fan);

        // A gesture that pulls node 7 to the floor and back up to where it
        // started has to leave the curve exactly as it found it. Applied to its
        // own output, `CurveNodes::set` would have flattened nodes 0..7 on the
        // way down and left them there.
        for duty in [200, 120, 40, 0, 90, base.duty[7]] {
            editor.set_node_from(Channel::Fan, base, 7, duty);
        }
        assert_eq!(*editor.curve(Channel::Fan), base);

        // The same drag done against the live curve is the lossy one, which is
        // what the base exists to avoid.
        let mut lossy = CoolingEditor::new();
        for duty in [0, base.duty[7]] {
            lossy.set_node(Channel::Fan, 7, duty);
        }
        assert_ne!(*lossy.curve(Channel::Fan), base);
    }

    #[test]
    fn a_replayed_drag_still_respects_the_pump_floor_and_stays_monotonic() {
        let mut editor = CoolingEditor::new();
        let base = *editor.curve(Channel::Pump);
        editor.set_node_from(Channel::Pump, base, 4, 0);

        let nodes = editor.curve(Channel::Pump);
        assert!(nodes.duty.iter().all(|duty| *duty >= MIN_PUMP_DUTY));
        assert!(
            nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]),
            "the curve the daemon validates must stay non-decreasing"
        );
        assert_eq!(editor.validation_error(), None);
    }

    #[test]
    fn the_onboard_program_is_never_pending() {
        let editor = CoolingEditor::new();
        assert_eq!(editor.mode, CoolingMode::Onboard);
        assert!(!editor.pending(None));
        assert_eq!(editor.program(), CoolingProgram::Onboard);
    }

    /// Reopening the window must show the machine, not the factory arrangement.
    /// The curve is the case that matters: nothing reads one back, so an editor
    /// that ignored the daemon's record left the operator drawing theirs again.
    #[test]
    fn adopting_the_committed_program_opens_the_editor_on_the_machine() {
        let mut drawn = CoolingEditor::new();
        drawn.select_node(Channel::Fan, 6);
        drawn.adjust_node(Channel::Fan, 12);
        let committed = drawn.program();

        let mut fresh = CoolingEditor::new();
        assert_eq!(fresh.mode, CoolingMode::Onboard);
        fresh.adopt(&committed);

        assert_eq!(fresh.mode, CoolingMode::Curve);
        assert_eq!(fresh.curve(Channel::Fan), drawn.curve(Channel::Fan));
        assert_eq!(fresh.program(), committed);
        assert!(
            !fresh.pending(Some(&kraken(PwmMode::Curve, 200, 200))),
            "a program the daemon says it committed is not an unsent edit"
        );

        // A fixed program takes both duties and the mode that runs them.
        let mut fresh = CoolingEditor::new();
        fresh.adopt(&CoolingProgram::Fixed { pump: 180, fan: 90 });
        assert_eq!(fresh.mode, CoolingMode::Fixed);
        assert_eq!(fresh.duty(Channel::Pump), 180);
        assert_eq!(fresh.duty(Channel::Fan), 90);
        assert!(!fresh.pending(Some(&kraken(PwmMode::Fixed, 180, 90))));
    }

    /// A curve this editor could not have produced is not read into the plot.
    /// Showing an arbitrary reading of it would claim the operator drew
    /// something they never did.
    #[test]
    fn a_curve_that_does_not_round_trip_leaves_the_plot_alone() {
        let mut editor = CoolingEditor::new();
        let before = *editor.curve(Channel::Pump);
        let short = herschel_core::profile::TemperatureCurve { points: Vec::new() };
        editor.adopt(&CoolingProgram::Curve {
            pump: short.clone(),
            fan: short,
        });

        assert_eq!(*editor.curve(Channel::Pump), before);
        assert_eq!(editor.mode, CoolingMode::Onboard);
    }

    #[test]
    fn cancel_returns_the_editor_to_the_reported_hardware_state() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Fixed);
        editor.set_duty(Channel::Pump, 255);
        editor.set_duty(Channel::Fan, 255);

        editor.cancel(Some(&kraken(PwmMode::Fixed, 180, 90)));
        assert_eq!(editor.mode, CoolingMode::Fixed);
        assert_eq!(editor.pump_duty, 180);
        assert_eq!(editor.fan_duty, 90);
        assert!(!editor.pending(Some(&kraken(PwmMode::Fixed, 180, 90))));
    }

    #[test]
    fn cancel_restores_the_last_applied_curve() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Curve);
        let applied = editor.program();
        editor.record_applied(applied.clone());

        editor.select_node(Channel::Fan, 5);
        editor.adjust_node(Channel::Fan, 6);
        assert_ne!(editor.program(), applied);

        editor.cancel(Some(&kraken(PwmMode::Curve, 200, 200)));
        assert_eq!(editor.program(), applied);
    }

    #[test]
    fn cancel_without_telemetry_falls_back_to_the_program_that_writes_nothing() {
        let mut editor = CoolingEditor::new();
        editor.set_mode(CoolingMode::Fixed);
        editor.cancel(None);
        assert_eq!(editor.mode, CoolingMode::Onboard);
    }

    #[test]
    fn rows_open_independently_and_edits_stay_on_their_own_channel() {
        let mut editor = CoolingEditor::new();
        assert!(editor.expanded().is_empty(), "every row starts closed");

        editor.toggle(Channel::Pump);
        assert!(editor.is_expanded(Channel::Pump));

        editor.toggle(Channel::Fan);
        assert!(
            editor.is_expanded(Channel::Pump) && editor.is_expanded(Channel::Fan),
            "opening one row does not close the other"
        );

        editor.toggle(Channel::Fan);
        assert!(
            !editor.is_expanded(Channel::Fan),
            "a second press closes it"
        );
        assert!(
            editor.is_expanded(Channel::Pump),
            "and leaves the other row alone"
        );

        // The channel an edit lands on is the one it names, not the open row.
        let pump_before = *editor.curve(Channel::Pump);
        editor.select_node(Channel::Fan, 4);
        editor.adjust_node(Channel::Fan, 3);
        assert_eq!(*editor.curve(Channel::Pump), pump_before);
        assert_ne!(editor.curve(Channel::Fan).duty[4], pump_before.duty[4]);
    }

    #[test]
    fn editing_a_node_selects_the_mode_that_runs_the_curve() {
        let mut editor = CoolingEditor::new();
        assert_eq!(editor.mode, CoolingMode::Onboard);

        editor.select_node(Channel::Fan, 6);
        editor.adjust_node(Channel::Fan, 2);
        assert_eq!(editor.mode, CoolingMode::Curve);
        assert!(matches!(editor.program(), CoolingProgram::Curve { .. }));

        // Selecting a node is not editing one, so it commits to nothing.
        let mut editor = CoolingEditor::new();
        editor.step_node(Channel::Pump, 3);
        assert_eq!(editor.mode, CoolingMode::Onboard);
    }

    #[test]
    fn modes_round_trip_through_their_stored_value() {
        for mode in CoolingMode::ALL {
            assert_eq!(CoolingMode::from_value(mode.value()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(CoolingMode::from_value("raw"), None);
    }
}
