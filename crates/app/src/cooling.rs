// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The pending state of the Cooling screen.
//!
//! Everything here is an edit that has not happened yet. Moving a slider or a
//! curve node changes this structure and nothing else; the hardware is only
//! touched when Apply is activated and the daemon accepts the resulting
//! program. Keeping the pending edit separate from the confirmed readback is
//! what lets the screen show the two side by side.

use nzxt_core::profile::{
    CURVE_NODE_COUNT, Channel, CoolingProgram, CurveNodes, MAX_DUTY, ValidationError,
    validate_program,
};
use nzxt_core::telemetry::{KrakenTelemetry, PwmMode};

/// Duty a single press moves, out of 255.
///
/// Five steps is a shade under 2%, small enough to tune a curve and large
/// enough that crossing the useful range does not take a hundred presses.
pub const DUTY_STEP: u8 = 5;

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
    /// Channel the curve editor is currently editing.
    pub channel: Channel,
    /// Selected curve node, the one keyboard edits move.
    pub node: usize,
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
            pump_duty: 180,
            fan_duty: 128,
            pump_curve: CurveNodes::starting_ramp(),
            fan_curve: CurveNodes::starting_ramp(),
            channel: Channel::Pump,
            node: 0,
            applied: None,
        }
    }

    /// The program Apply would send.
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
    /// Clamping in the editor is what makes the control refuse an impossible
    /// value rather than let Apply be pressed on one; the daemon still
    /// revalidates, because the client is not a trusted input.
    pub fn set_duty(&mut self, channel: Channel, duty: u8) {
        let clamped = duty.clamp(channel.min_duty(), MAX_DUTY);
        match channel {
            Channel::Pump => self.pump_duty = clamped,
            Channel::Fan => self.fan_duty = clamped,
        }
    }

    pub fn adjust_duty(&mut self, channel: Channel, steps: i16) {
        let current = self.duty(channel) as i16;
        let next = (current + steps * DUTY_STEP as i16).clamp(0, MAX_DUTY as i16);
        self.set_duty(channel, next as u8);
    }

    /// Move the selection along the curve.
    pub fn step_node(&mut self, delta: isize) {
        let next = self.node as isize + delta;
        self.node = next.clamp(0, CURVE_NODE_COUNT as isize - 1) as usize;
    }

    pub fn select_node(&mut self, index: usize) {
        self.node = index.min(CURVE_NODE_COUNT - 1);
    }

    /// Set one node of the current channel's curve.
    ///
    /// The duty floor of the channel applies to every node, so a pump curve
    /// can never be edited into stopping the pump.
    pub fn set_node(&mut self, index: usize, duty: u8) {
        let channel = self.channel;
        let clamped = duty.clamp(channel.min_duty(), MAX_DUTY);
        self.curve_mut(channel).set(index, clamped);
    }

    pub fn adjust_node(&mut self, steps: i16) {
        let channel = self.channel;
        let current = self.curve(channel).duty[self.node] as i16;
        let next = (current + steps * DUTY_STEP as i16).clamp(0, MAX_DUTY as i16);
        let node = self.node;
        self.set_node(node, next as u8);
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

    /// Whether the pending edit differs from what the hardware confirms.
    ///
    /// A fixed program is checked against the readback, which the driver
    /// publishes. A curve cannot be read back at all, so it is checked against
    /// this client's record of the last confirmed Apply *and* the mode the
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

    /// The reason Apply is refused before it is even sent, if there is one.
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
    use nzxt_core::profile::MIN_PUMP_DUTY;
    use nzxt_core::telemetry::{ChannelTelemetry, Reading};

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

        editor.channel = Channel::Pump;
        for (index, duty) in [(0, 0), (9, 30), (4, 255), (7, 60)] {
            editor.select_node(index);
            editor.set_node(index, duty);
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
        editor.channel = Channel::Pump;
        editor.select_node(3);
        editor.set_node(3, 0);
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
        editor.step_node(-5);
        assert_eq!(editor.node, 0);
        editor.step_node(100);
        assert_eq!(editor.node, CURVE_NODE_COUNT - 1);
        editor.select_node(999);
        assert_eq!(editor.node, CURVE_NODE_COUNT - 1);
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
        editor.select_node(2);
        editor.adjust_node(1);
        assert!(editor.pending(Some(&kraken(PwmMode::Curve, 200, 200))));
    }

    #[test]
    fn the_onboard_program_is_never_pending() {
        let editor = CoolingEditor::new();
        assert_eq!(editor.mode, CoolingMode::Onboard);
        assert!(!editor.pending(None));
        assert_eq!(editor.program(), CoolingProgram::Onboard);
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

        editor.select_node(5);
        editor.adjust_node(6);
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
    fn modes_round_trip_through_their_stored_value() {
        for mode in CoolingMode::ALL {
            assert_eq!(CoolingMode::from_value(mode.value()), Some(mode));
            assert!(!mode.label().is_empty());
        }
        assert_eq!(CoolingMode::from_value("raw"), None);
    }
}
