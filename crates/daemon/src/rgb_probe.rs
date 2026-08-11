// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The capability probe for the RGB controller.
//!
//! Two modes, and the difference between them is the whole point.
//!
//! The read-only mode asks the controller what it is. Both requests carry no
//! color, no mode and no parameter, so running it changes nothing the operator
//! can see. It is what fills the capability record.
//!
//! The write mode is the only place in this workspace that sends a lighting
//! command the product has never proven. It refuses to start without an
//! explicit typed confirmation, takes the same per-device lock the daemon
//! takes so it can never run beside it, records the exact bytes of every report
//! it sends, and finishes by restoring a state the operator chose.
//!
//! The controller exposes no report that reads a channel back. There is
//! therefore no instrument that can capture the prior state or confirm a
//! restoration except the operator's own eyes, so the probe asks and records
//! the answer verbatim. An observation nobody made is left empty rather than
//! filled with a plausible one.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use kori_core::capability::{Evidenced, RgbChannel, RgbTopology};
use kori_core::lighting::{
    Brightness, EffectDirection, EffectSpeed, LightingEffect, LightingProgram, PROBE_BRIGHTNESS,
    Rgb,
};
use kori_hardware_linux::rgb::{self, HidTransport};
use serde::{Deserialize, Serialize};

/// What the operator must type to authorize a write to the controller.
pub const CONFIRMATION_PHRASE: &str = "PROBE";

/// Color the probe applies while testing a channel.
///
/// White exercises all three LED dies at once, at [`PROBE_BRIGHTNESS`], so one
/// step tells the operator whether the channel lit at all and whether the
/// triplet order is right.
pub const PROBE_COLOR: Rgb = Rgb::new(0xff, 0xff, 0xff);

/// Intervals the cadence measurement steps down through, in milliseconds.
///
/// Bounded and ordered from slow to fast: the fastest interval that completed
/// without an error is what the record reports, and nothing faster is tried
/// after the first failure.
const CADENCE_STEPS_MS: [u64; 5] = [200, 100, 50, 25, 10];

/// Commands sent at each cadence step.
const CADENCE_COMMANDS: usize = 5;

/// One report the probe sent, with everything observable about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeStep {
    pub channel: u8,
    /// Human-readable program, not a wire encoding.
    pub program: String,
    /// The exact 64 bytes that left this process.
    pub report_hex: String,
    /// What the controller sent back, when it sent anything at all.
    pub response_hex: Option<String>,
    /// How long the write itself took.
    pub write_micros: u64,
    pub error: Option<String>,
    /// What the operator saw, verbatim. Empty when nobody looked.
    pub observed: String,
}

/// The fastest cadence that completed without an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CadenceObservation {
    /// Intervals tried, slowest first.
    pub tried_ms: Vec<u64>,
    /// Fastest interval at which every command landed.
    pub stable_interval_ms: Option<u64>,
    /// Interval at which a command first failed, when one did.
    pub first_failure_ms: Option<u64>,
    pub detail: String,
}

/// Whether the controller was put back where the operator wanted it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Restoration {
    /// The operator confirmed the controller looks right.
    Confirmed { program: String, observed: String },
    /// The restoration was attempted and the operator did not confirm it.
    Unconfirmed { program: String, reason: String },
    /// Nothing was restored, because nothing was written.
    NotNeeded,
}

/// Everything a write probe run has to record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteProbeReport {
    /// Which command set this run was authorized to send.
    pub scope: String,
    pub device: String,
    pub node: String,
    pub firmware: Evidenced<String>,
    pub channels: Vec<RgbChannel>,
    /// The state before the first write, as described by the operator.
    ///
    /// This controller answers no report that reads a channel back, so the
    /// operator's description is the only capture that can exist. It is
    /// recorded as written, and left empty when nobody described it.
    pub prior_state: String,
    pub steps: Vec<ProbeStep>,
    pub cadence: CadenceObservation,
    pub restoration: Restoration,
}

impl WriteProbeReport {
    /// True when every report the probe sent left the process without error.
    pub fn every_step_landed(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|step| step.error.is_none())
    }
}

/// Where the probe asks its questions and reads the answers.
///
/// A trait so the ordering, the refusal and the recording can be tested without
/// a terminal, and so a run with no answers available records empty
/// observations rather than inventing them.
pub trait Operator {
    /// Ask a question and return the answer, trimmed.
    fn ask(&mut self, question: &str) -> String;

    /// Report progress. Never carries a value the record depends on.
    fn note(&mut self, message: &str);
}

/// The interactive operator, on stderr and stdin.
///
/// Prompts go to stderr so the report itself can be redirected to a file
/// without the questions landing in it.
pub struct Terminal;

impl Operator for Terminal {
    fn ask(&mut self, question: &str) -> String {
        eprint!("{question} ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => String::new(),
            Ok(_) => answer.trim().to_string(),
        }
    }

    fn note(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

/// Which commands a run is allowed to send.
///
/// The base run tests one allowlisted low-brightness fixed color and off, and
/// nothing else. The two animated candidates are a separate, separately
/// authorized step: they reuse the command sequence the base run establishes,
/// and until that sequence has been seen to work there is nothing to extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeScope {
    /// The base set: a dim fixed color, then off.
    FixedAndOff,
    /// The base set, then the two effect candidates, then off again.
    WithEffects,
    /// Every effect parameter the Lighting screen lets an operator select.
    ///
    /// Only values the record validates may be selected on that screen, and
    /// [`Self::WithEffects`] exercises each effect at one speed only. This
    /// scope sends every speed of both effects and both directions of the one
    /// effect that travels, so no selectable value rests on a table borrowed
    /// from another implementation.
    EffectSweep,
}

impl ProbeScope {
    /// Channels this scope addresses, out of the ones the controller reported.
    ///
    /// [`Self::EffectSweep`] stays on the first channel. Speed and direction
    /// are firmware behavior rather than channel behavior, and the channel
    /// bits were already mapped one to one onto physical fans by the base
    /// run, so sweeping every channel would triple an operator's watching time
    /// to re-observe a fact that is already recorded.
    fn channels(self, reported: &[RgbChannel]) -> Vec<RgbChannel> {
        match self {
            Self::EffectSweep => reported.iter().take(1).cloned().collect(),
            _ => reported.to_vec(),
        }
    }

    /// The name this scope carries in the recorded evidence.
    fn key(self) -> &'static str {
        match self {
            Self::FixedAndOff => "fixed_and_off",
            Self::WithEffects => "with_effects",
            Self::EffectSweep => "effect_sweep",
        }
    }

    /// What this run will send, in the words the operator authorizes.
    ///
    /// Read out before the confirmation is asked, so what is authorized is
    /// what actually leaves the process. A prompt that named only the base
    /// set would understate a run carrying the two animated candidates, and an
    /// authorization given against the wrong description is not one.
    fn description(self) -> String {
        match self {
            Self::FixedAndOff => format!(
                "Each channel is tested one at a time with a {PROBE_BRIGHTNESS}% white, then off. \
                 Nothing else is sent."
            ),
            Self::WithEffects => format!(
                "Each channel is tested one at a time with a {PROBE_BRIGHTNESS}% white, then off, \
                 then Breathing and Spectrum Wave at {PROBE_BRIGHTNESS}%, then off again. \
                 Nothing else is sent."
            ),
            Self::EffectSweep => format!(
                "The first channel only, at {PROBE_BRIGHTNESS}%: Breathing at each of the {} \
                 speeds, then Spectrum Wave at each speed forward, then each speed backward, \
                 then off. {} programs, one at a time. Nothing else is sent.",
                EffectSpeed::ALL.len(),
                Self::EffectSweep.programs().len()
            ),
        }
    }

    /// Programs sent to each addressed channel, in order.
    fn programs(self) -> Vec<LightingProgram> {
        if self == Self::EffectSweep {
            return Self::sweep();
        }

        let mut programs = vec![rgb::probe_program(PROBE_COLOR), LightingProgram::Off];
        if self == Self::WithEffects {
            programs.push(breathing());
            programs.push(spectrum_wave());
            programs.push(LightingProgram::Off);
        }
        programs
    }

    /// One program per value the Lighting screen can select.
    ///
    /// Built from the same `ALL` tables the screen builds its selects from, so
    /// a value that becomes selectable without being swept here fails
    /// `the_sweep_covers_every_value_the_screen_can_select`.
    fn sweep() -> Vec<LightingProgram> {
        let brightness = Brightness::new(PROBE_BRIGHTNESS).unwrap_or(Brightness::OFF);
        let mut programs = Vec::new();

        for effect in LightingEffect::ALL {
            let (min, _) = effect.color_range();
            let colors = vec![PROBE_COLOR; min];
            let directions: &[EffectDirection] = if effect.accepts_direction() {
                &EffectDirection::ALL
            } else {
                &[EffectDirection::Forward]
            };
            for direction in directions {
                for speed in EffectSpeed::ALL {
                    programs.push(LightingProgram::Effect {
                        effect,
                        colors: colors.clone(),
                        brightness,
                        speed,
                        direction: *direction,
                    });
                }
            }
        }

        programs.push(LightingProgram::Off);
        programs
    }
}

/// Why the probe refused to start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeRefusal {
    #[error("the controller did not answer its topology: {reason}")]
    NoTopology { reason: String },
    #[error(
        "the write probe was not authorized. Re-run it and type {CONFIRMATION_PHRASE} when asked."
    )]
    NotAuthorized,
}

/// Run the write probe against a controller the caller already opened.
///
/// The caller owns opening the node and holding the device lock, so this
/// function is the same code whether it drives real hardware or a controller
/// that answers the same reports.
pub fn run<T: HidTransport + ?Sized, O: Operator>(
    transport: &mut T,
    operator: &mut O,
    topology: &RgbTopology,
    scope: ProbeScope,
    restore_to: LightingProgram,
) -> Result<WriteProbeReport, ProbeRefusal> {
    let Some(reported) = topology.channels.value().cloned() else {
        return Err(ProbeRefusal::NoTopology {
            reason: topology
                .channels
                .reason()
                .unwrap_or("no reason recorded")
                .to_string(),
        });
    };

    operator.note(
        "This will send lighting commands to 1e71:2021 that this product has never \
         validated on this firmware.",
    );
    operator.note(&scope.description());
    if operator.ask(&format!(
        "Type {CONFIRMATION_PHRASE} to authorize, anything else to abort:"
    )) != CONFIRMATION_PHRASE
    {
        return Err(ProbeRefusal::NotAuthorized);
    }

    let prior_state = operator.ask(
        "Describe what the lighting shows right now. This controller answers no \
         report that reads a channel back, so this is the only capture there can be:",
    );

    // Restoration covers every channel the controller reported, even when the
    // scope only wrote to one: a channel left mid-run by an aborted earlier
    // pass must still be put back.
    let addressed = scope.channels(&reported);

    let mut steps = Vec::new();
    for channel in &addressed {
        // One channel at a time, and within a channel one program at a time,
        // so an operator watching the strip can attribute what they see.
        for program in scope.programs() {
            let mut step = send(transport, channel.index, &program);
            step.observed = operator.ask(&format!(
                "Channel {} was sent {}. What did it show?",
                channel.index,
                program.summary()
            ));
            steps.push(step);
            std::thread::sleep(rgb::minimum_command_interval());
        }
    }

    let cadence = measure_cadence(transport, addressed.first().map(|c| c.index).unwrap_or(1));
    operator.note(&cadence.detail);

    let restoration = restore(transport, operator, &reported, restore_to);

    Ok(WriteProbeReport {
        scope: scope.key().to_string(),
        device: kori_core::RGB_CONTROLLER.to_string(),
        node: transport.source(),
        firmware: topology.firmware.clone(),
        channels: reported,
        prior_state,
        steps,
        cadence,
        restoration,
    })
}

fn breathing() -> LightingProgram {
    LightingProgram::Effect {
        effect: LightingEffect::Breathing,
        colors: vec![PROBE_COLOR],
        brightness: Brightness::new(PROBE_BRIGHTNESS).unwrap_or(Brightness::OFF),
        speed: EffectSpeed::Normal,
        direction: EffectDirection::Forward,
    }
}

fn spectrum_wave() -> LightingProgram {
    LightingProgram::Effect {
        effect: LightingEffect::SpectrumWave,
        colors: Vec::new(),
        brightness: Brightness::new(PROBE_BRIGHTNESS).unwrap_or(Brightness::OFF),
        speed: EffectSpeed::Normal,
        direction: EffectDirection::Forward,
    }
}

/// Send one program and record everything observable about the exchange.
fn send<T: HidTransport + ?Sized>(
    transport: &mut T,
    channel: u8,
    program: &LightingProgram,
) -> ProbeStep {
    // A program that cannot be encoded is recorded as a refusal, never written.
    // The fallback used to be a zeroed buffer, which would have put report
    // identifier `0x00` on the wire: an identifier the controller never
    // declared, sent by the one command that exists to avoid exactly that.
    let Some(report) =
        rgb::packet::channel_bit(channel).and_then(|bit| rgb::packet::color_command(bit, program))
    else {
        return ProbeStep {
            channel,
            program: program.summary(),
            report_hex: String::new(),
            response_hex: None,
            write_micros: 0,
            error: Some(format!(
                "the program could not be encoded for channel {channel}, so nothing was sent"
            )),
            observed: String::new(),
        };
    };

    let started = Instant::now();
    let error = transport.write_report(&report).err().map(|e| e.to_string());
    let write_micros = started.elapsed().as_micros() as u64;

    // The controller answers no color command, so an empty read is the
    // expected result and is recorded as such rather than as a failure.
    let response_hex = transport
        .read_report(Duration::from_millis(50))
        .ok()
        .flatten()
        .map(|answer| hex(&answer));

    ProbeStep {
        channel,
        program: program.summary(),
        report_hex: hex(&report),
        response_hex,
        write_micros,
        error,
        observed: String::new(),
    }
}

/// Step the interval down until a command fails, and report the last one that
/// did not.
fn measure_cadence<T: HidTransport + ?Sized>(transport: &mut T, channel: u8) -> CadenceObservation {
    let mut tried = Vec::new();
    let mut stable = None;
    let mut first_failure = None;

    for interval_ms in CADENCE_STEPS_MS {
        tried.push(interval_ms);
        let mut failed = false;
        for index in 0..CADENCE_COMMANDS {
            // Alternating dim white and off, so the burst is visible without
            // ever exceeding the probe brightness.
            let program = if index % 2 == 0 {
                rgb::probe_program(PROBE_COLOR)
            } else {
                LightingProgram::Off
            };
            let step = send(transport, channel, &program);
            if step.error.is_some() {
                failed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(interval_ms));
        }
        if failed {
            first_failure = Some(interval_ms);
            break;
        }
        stable = Some(interval_ms);
    }

    let detail = match (stable, first_failure) {
        (Some(stable), Some(failure)) => format!(
            "{CADENCE_COMMANDS} commands landed at {stable} ms; the first failure appeared at {failure} ms."
        ),
        (Some(stable), None) => format!(
            "{CADENCE_COMMANDS} commands landed at every interval down to {stable} ms without an error."
        ),
        (None, Some(failure)) => {
            format!("The controller refused a command at the slowest interval tried, {failure} ms.")
        }
        (None, None) => "No cadence step completed.".to_string(),
    };

    CadenceObservation {
        tried_ms: tried,
        stable_interval_ms: stable,
        first_failure_ms: first_failure,
        detail,
    }
}

/// Put every tested channel on the program the operator chose, then ask.
fn restore<T: HidTransport + ?Sized, O: Operator>(
    transport: &mut T,
    operator: &mut O,
    channels: &[RgbChannel],
    program: LightingProgram,
) -> Restoration {
    let mut failures = Vec::new();
    for channel in channels {
        let step = send(transport, channel.index, &program);
        if let Some(error) = step.error {
            failures.push(format!("channel {}: {error}", channel.index));
        }
        std::thread::sleep(rgb::minimum_command_interval());
    }

    if !failures.is_empty() {
        return Restoration::Unconfirmed {
            program: program.summary(),
            reason: failures.join("; "),
        };
    }

    let observed = operator.ask(&format!(
        "Every channel was set to {}. Does the lighting look right? Describe what you see:",
        program.summary()
    ));
    if observed.is_empty() {
        return Restoration::Unconfirmed {
            program: program.summary(),
            reason: "the operator recorded no observation, so restoration is not confirmed"
                .to_string(),
        };
    }
    Restoration::Confirmed {
        program: program.summary(),
        observed,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_hardware_linux::testing::FakeController;

    /// An operator with a scripted set of answers.
    struct Script {
        answers: std::collections::VecDeque<String>,
        asked: Vec<String>,
        /// Everything the operator was told, in order.
        noted: Vec<String>,
    }

    impl Script {
        fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|a| a.to_string()).collect(),
                asked: Vec::new(),
                noted: Vec::new(),
            }
        }

        /// What the operator had been told by the time they authorized the run.
        fn briefing(&self) -> String {
            self.noted.join(" ")
        }
    }

    impl Operator for Script {
        fn ask(&mut self, question: &str) -> String {
            self.asked.push(question.to_string());
            self.answers.pop_front().unwrap_or_default()
        }

        fn note(&mut self, message: &str) {
            self.noted.push(message.to_string());
        }
    }

    fn topology(channels: u8) -> RgbTopology {
        RgbTopology {
            node: Evidenced::known("/dev/hidraw12".into(), "sysfs"),
            firmware: Evidenced::known("1.2.3".into(), "report 0x11 0x01"),
            channels: Evidenced::known(
                (1..=channels)
                    .map(|index| RgbChannel {
                        index,
                        accessories: Vec::new(),
                        led_count: Evidenced::unknown("not reported", "report 0x21 0x03"),
                    })
                    .collect(),
                "report 0x21 0x03",
            ),
        }
    }

    #[test]
    fn nothing_is_sent_without_the_typed_confirmation() {
        let mut controller = FakeController::new("1.2.3", 1);
        let mut operator = Script::new(&["yes", "sure", "go ahead"]);

        let refusal = run(
            &mut controller,
            &mut operator,
            &topology(1),
            ProbeScope::WithEffects,
            LightingProgram::Off,
        )
        .unwrap_err();

        assert_eq!(refusal, ProbeRefusal::NotAuthorized);
        assert_eq!(
            controller.command_count(),
            0,
            "an unauthorized probe must reach no hardware"
        );
    }

    #[test]
    fn the_operator_authorizes_the_scope_the_run_actually_sends() {
        // The confirmation is worth exactly as much as its description. A run
        // carrying the animated candidates must say so before it is
        // authorized, or the operator agreed to a different run.
        for (scope, effects_named) in [
            (ProbeScope::FixedAndOff, false),
            (ProbeScope::WithEffects, true),
        ] {
            let mut controller = FakeController::new("1.2.3", 1);
            let mut operator = Script::new(&["no"]);
            let refusal = run(
                &mut controller,
                &mut operator,
                &topology(1),
                scope,
                LightingProgram::Off,
            )
            .unwrap_err();
            assert_eq!(refusal, ProbeRefusal::NotAuthorized);

            let briefing = operator.briefing();
            assert!(briefing.contains("white"), "{briefing}");
            assert_eq!(
                briefing.contains("Breathing") && briefing.contains("Spectrum Wave"),
                effects_named,
                "{scope:?} was described as: {briefing}"
            );
        }
    }

    #[test]
    fn an_unknown_topology_refuses_before_the_confirmation_is_even_asked() {
        let mut controller = FakeController::new("1.2.3", 1);
        let mut operator = Script::new(&[CONFIRMATION_PHRASE]);

        let refusal = run(
            &mut controller,
            &mut operator,
            &RgbTopology::unavailable("permission denied", "/dev/hidraw12"),
            ProbeScope::FixedAndOff,
            LightingProgram::Off,
        )
        .unwrap_err();

        assert!(matches!(refusal, ProbeRefusal::NoTopology { .. }));
        assert!(
            operator.asked.is_empty(),
            "a probe that cannot run must not ask the operator to authorize it"
        );
        assert_eq!(controller.command_count(), 0);
    }

    #[test]
    fn the_us_013_scope_sends_a_dim_fixed_color_and_off_and_nothing_else() {
        let mut controller = FakeController::new("1.2.3", 2);
        let mut operator = Script::new(&[
            CONFIRMATION_PHRASE,
            "a static purple",
            "white",
            "dark",
            "white",
            "dark",
            "purple again",
        ]);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(2),
            ProbeScope::FixedAndOff,
            LightingProgram::Off,
        )
        .unwrap();

        assert_eq!(report.scope, "fixed_and_off");
        assert_eq!(
            report.steps.len(),
            4,
            "two programs on each of two channels"
        );
        // The mode byte of every report is zero: no animation was sent.
        for step in &report.steps {
            let mode = u8::from_str_radix(&step.report_hex[8..10], 16).unwrap();
            assert_eq!(mode, 0x00, "{}", step.program);
        }
        // One channel at a time, in order.
        assert_eq!(
            report.steps.iter().map(|s| s.channel).collect::<Vec<_>>(),
            vec![1, 1, 2, 2]
        );
    }

    #[test]
    fn an_authorized_probe_records_every_report_it_sent() {
        let mut controller = FakeController::new("1.2.3", 1);
        let mut operator = Script::new(&[
            CONFIRMATION_PHRASE,
            "a static purple",
            "white",
            "dark",
            "pulsing white",
            "a moving rainbow",
            "dark",
            "purple again",
        ]);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(1),
            ProbeScope::WithEffects,
            LightingProgram::Off,
        )
        .unwrap();

        assert_eq!(report.prior_state, "a static purple");
        assert_eq!(report.steps.len(), 5, "five programs on the single channel");
        assert!(report.every_step_landed());
        assert_eq!(report.steps[0].observed, "white");
        assert_eq!(report.steps[3].observed, "a moving rainbow");

        // Every recorded report is a full 64-byte frame carrying the color
        // command identifier, which is what makes the record reproducible.
        for step in &report.steps {
            assert_eq!(
                step.report_hex.len(),
                rgb::REPORT_BYTES * 2,
                "{}",
                step.program
            );
            assert!(step.report_hex.starts_with("2a04"), "{}", step.report_hex);
        }
        assert_eq!(report.firmware.value().map(String::as_str), Some("1.2.3"));
    }

    #[test]
    fn the_sweep_covers_every_value_the_screen_can_select() {
        // The Lighting screen lets an operator select a speed and a direction.
        // Each one has to have been sent to this controller once, or it is
        // exposed on the strength of a table borrowed from another
        // implementation. The
        // expectation is built from the same `ALL` arrays the screen builds
        // its selects from, so a value added there without being swept here
        // fails this test rather than reaching an operator unproven.
        let programs = ProbeScope::EffectSweep.programs();

        for effect in LightingEffect::ALL {
            for speed in EffectSpeed::ALL {
                for direction in EffectDirection::ALL {
                    if direction == EffectDirection::Backward && !effect.accepts_direction() {
                        continue;
                    }
                    assert!(
                        programs.iter().any(|program| matches!(
                            program,
                            LightingProgram::Effect { effect: e, speed: s, direction: d, .. }
                                if *e == effect && *s == speed && *d == direction
                        )),
                        "{effect:?} at {speed:?} {direction:?} is selectable but never swept"
                    );
                }
            }
        }

        // Every swept program is one the daemon would accept, so the probe
        // cannot record evidence for a command the product then refuses.
        for program in &programs {
            kori_core::lighting::validate_program(program)
                .unwrap_or_else(|error| panic!("{program:?} is not a valid program: {error}"));
        }
        assert_eq!(programs.last(), Some(&LightingProgram::Off), "it ends dark");
    }

    #[test]
    fn the_sweep_writes_to_one_channel_and_still_restores_every_one() {
        let mut controller = FakeController::new("1.2.3", 3);
        let mut answers = vec![CONFIRMATION_PHRASE.to_string(), "purple".to_string()];
        answers.extend(std::iter::repeat_n(
            String::new(),
            ProbeScope::EffectSweep.programs().len(),
        ));
        answers.push("purple again".to_string());
        let borrowed: Vec<&str> = answers.iter().map(String::as_str).collect();
        let mut operator = Script::new(&borrowed);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(3),
            ProbeScope::EffectSweep,
            LightingProgram::Off,
        )
        .unwrap();

        assert_eq!(report.scope, "effect_sweep");
        assert!(
            report.steps.iter().all(|step| step.channel == 1),
            "the sweep is a firmware question, so it stays on one channel"
        );
        assert!(report.every_step_landed());
        // The record still describes the whole controller, and every channel
        // was put back whether or not the sweep addressed it.
        assert_eq!(report.channels.len(), 3);
        assert!(matches!(report.restoration, Restoration::Confirmed { .. }));

        let briefing = operator.briefing();
        assert!(briefing.contains("first channel only"), "{briefing}");
    }

    #[test]
    fn a_channel_the_controller_cannot_address_is_refused_rather_than_zeroed() {
        // The topology is what names the channels, so this is not a state the
        // probe reaches on its own. It is asserted anyway because the fallback
        // for an unencodable program is the one place a report the controller
        // never declared could leave this process.
        let mut controller = FakeController::new("1.2.3", 1);
        let step = send(&mut controller, 9, &LightingProgram::Off);

        assert!(step.error.is_some(), "{step:?}");
        assert!(step.report_hex.is_empty(), "{}", step.report_hex);
        assert_eq!(
            controller.command_count(),
            0,
            "an unaddressable channel must reach no hardware"
        );
    }

    #[test]
    fn the_cadence_measurement_reports_the_fastest_interval_that_held() {
        let mut controller = FakeController::new("1.2.3", 1);
        let mut operator = Script::new(&[CONFIRMATION_PHRASE, "purple", "", "", "", "", "", "ok"]);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(1),
            ProbeScope::WithEffects,
            LightingProgram::Off,
        )
        .unwrap();

        assert_eq!(report.cadence.tried_ms, CADENCE_STEPS_MS.to_vec());
        assert_eq!(report.cadence.stable_interval_ms, Some(10));
        assert_eq!(report.cadence.first_failure_ms, None);
        assert!(
            report.cadence.detail.contains("10 ms"),
            "{}",
            report.cadence.detail
        );
    }

    #[test]
    fn an_unobserved_restoration_is_never_reported_as_confirmed() {
        let mut controller = FakeController::new("1.2.3", 1);
        // The trailing answer is empty: the operator walked away.
        let mut operator = Script::new(&[CONFIRMATION_PHRASE, "purple"]);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(1),
            ProbeScope::WithEffects,
            LightingProgram::Off,
        )
        .unwrap();

        match &report.restoration {
            Restoration::Unconfirmed { reason, .. } => {
                assert!(reason.contains("no observation"), "{reason}");
            }
            other => panic!("expected an unconfirmed restoration, got {other:?}"),
        }
        // Every step's observation is empty rather than invented.
        assert!(report.steps.iter().all(|step| step.observed.is_empty()));
    }

    #[test]
    fn a_confirmed_restoration_carries_what_the_operator_saw() {
        let mut controller = FakeController::new("1.2.3", 2);
        let mut answers = vec![CONFIRMATION_PHRASE.to_string(), "purple".to_string()];
        answers.extend(std::iter::repeat_n(String::new(), 10));
        answers.push("purple again, as before".to_string());
        let borrowed: Vec<&str> = answers.iter().map(String::as_str).collect();
        let mut operator = Script::new(&borrowed);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(2),
            ProbeScope::WithEffects,
            LightingProgram::Fixed {
                color: Rgb::new(0x7c, 0x5c, 0xff),
                brightness: Brightness::new(60).unwrap(),
            },
        )
        .unwrap();

        assert_eq!(
            report.steps.len(),
            10,
            "five programs on each of two channels"
        );
        match &report.restoration {
            Restoration::Confirmed { program, observed } => {
                assert!(program.contains("7C5CFF"), "{program}");
                assert_eq!(observed, "purple again, as before");
            }
            other => panic!("expected a confirmed restoration, got {other:?}"),
        }
    }

    #[test]
    fn a_controller_that_refuses_every_write_is_reported_rather_than_retried() {
        let mut controller = FakeController::new("1.2.3", 1);
        controller.write_failure = Some(kori_hardware_linux::rgb::RgbError::PermissionDenied {
            path: "/dev/hidraw12".to_string(),
        });
        let mut operator = Script::new(&[CONFIRMATION_PHRASE, "purple", "", "", "", "", "", ""]);

        let report = run(
            &mut controller,
            &mut operator,
            &topology(1),
            ProbeScope::WithEffects,
            LightingProgram::Off,
        )
        .unwrap();

        assert!(!report.every_step_landed());
        assert!(report.steps.iter().all(|step| step.error.is_some()));
        assert_eq!(report.cadence.stable_interval_ms, None);
        assert_eq!(report.cadence.first_failure_ms, Some(CADENCE_STEPS_MS[0]));
        match &report.restoration {
            Restoration::Unconfirmed { reason, .. } => assert!(reason.contains("udev"), "{reason}"),
            other => panic!("expected an unconfirmed restoration, got {other:?}"),
        }
    }
}
