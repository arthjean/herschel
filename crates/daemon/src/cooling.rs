// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The serialized cooling executor.
//!
//! One instance owns every write to the thermal path. It remembers what it last
//! committed per channel, which is what lets a repeated Apply perform zero
//! writes and what supplies the last known-good curve: the curve attributes are
//! write-only on this driver, so the only trustworthy record of them is the one
//! kept by the process that is the sole writer.
//!
//! A transaction that fails partway restores every channel it touched. Whether
//! that restoration could be confirmed is what separates a reported
//! `NotApplied` from a reported `Uncertain`, and the difference matters: an
//! uncertain state stops further writes until a readback succeeds.
//!
//! Two rules bound what reaches the device, and they mirror
//! [`crate::lighting`] because the argument is the same. A request identical to
//! the committed state is deduplicated and writes nothing. A request that would
//! write, arriving sooner than [`MIN_PROGRAM_INTERVAL_MS`] after the last
//! write, is refused: the kernel documents that this firmware can lock up or
//! discard changes that arrive too numerous at once, and deduplication is no
//! backpressure against a client alternating between two programs.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use kori_core::ipc::{ApplyOutcome, ChannelReadback, HardwareState};
use kori_core::profile::{
    CURVE_POINT_COUNT, Channel, CoolingProgram, MIN_PROGRAM_INTERVAL_MS, TemperatureCurve,
};
use kori_core::telemetry::{KrakenTelemetry, PwmMode};
use kori_hardware_linux::SysfsRoot;
use kori_hardware_linux::control::{Applied, ChannelSnapshot, CoolingControl, WriteFailure};
use kori_hardware_linux::hwmon::KrakenHwmon;

/// How long a curve is left alone before its committed shape is checked against
/// what the device reports it is running.
///
/// The duty in `pwmN` is the firmware's own reported value, refreshed from its
/// periodic status report rather than echoed from the last write, so it lags a
/// write by up to one report. Checking sooner would read the previous program
/// and call a correct write a divergence.
const CURVE_SETTLE_MS: u64 = 5_000;

/// How far the reported duty may sit from the committed curve before the curve
/// is treated as not actually running.
///
/// The driver stores duty as a percentage and converts both ways, so a value
/// written as 0-255 comes back up to two steps away. The liquid temperature the
/// daemon reads and the one the firmware interpolated against are also a moment
/// apart, so the neighboring points are accepted too; this covers the rounding
/// on top of that.
const CURVE_DUTY_TOLERANCE: u8 = 3;

/// What one channel was last told to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Fixed(u8),
    Curve(TemperatureCurve),
}

impl Target {
    fn mode(&self) -> PwmMode {
        match self {
            Self::Fixed(_) => PwmMode::Fixed,
            Self::Curve(_) => PwmMode::Curve,
        }
    }
}

/// A committed program, and when it was committed.
///
/// The timestamp is what makes the curve check meaningful: a curve is only
/// contradicted by a reported duty once the device has had a report to publish
/// it in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Commit {
    target: Target,
    at_unix_ms: u64,
}

/// A committed curve the device turned out not to be running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurveDivergence {
    pub channel: Channel,
    pub liquid_temperature_mc: i32,
    pub expected: u8,
    pub reported: u8,
}

/// The sole writer of the thermal path.
pub struct CoolingExecutor {
    sysfs: SysfsRoot,
    control: Option<CoolingControl>,
    /// The last program this process confirmed on each channel.
    committed: BTreeMap<Channel, Commit>,
    /// When an attribute was last written, whatever channel it belonged to.
    ///
    /// One stamp for the device rather than one per channel: a curve
    /// transaction writes both channels back to back, and it is the device that
    /// the cadence floor protects.
    last_write: Option<Instant>,
}

impl std::fmt::Debug for CoolingExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoolingExecutor")
            .field("available", &self.control.is_some())
            .field("committed", &self.committed.len())
            .finish()
    }
}

impl CoolingExecutor {
    pub fn open(sysfs: &SysfsRoot) -> Self {
        Self {
            control: KrakenHwmon::locate(sysfs).map(CoolingControl::new),
            sysfs: sysfs.clone(),
            committed: BTreeMap::new(),
            last_write: None,
        }
    }

    /// Forget what was committed, after the device went away.
    ///
    /// A record kept across a disconnect would let a later Apply deduplicate
    /// against a state the device no longer holds.
    pub fn forget(&mut self) {
        self.committed.clear();
        self.control = None;
    }

    fn resolve(&mut self) -> Option<&CoolingControl> {
        if self.control.is_none() {
            self.control = KrakenHwmon::locate(&self.sysfs).map(CoolingControl::new);
        }
        self.control.as_ref()
    }

    fn committed_curve(&self, channel: Channel) -> Option<TemperatureCurve> {
        match &self.committed.get(&channel)?.target {
            Target::Curve(curve) => Some(*curve),
            Target::Fixed(_) => None,
        }
    }

    fn committed_target(&self, channel: Channel) -> Option<&Target> {
        self.committed.get(&channel).map(|commit| &commit.target)
    }

    /// The program both channels are committed to, when they hold the same
    /// kind of target.
    ///
    /// Reconstructed from the per-channel commits rather than kept beside them,
    /// so a curve [`CoolingExecutor::verify_curves`] uncommitted stops being
    /// reported here in the same move. Two channels running different kinds of
    /// target are not a program this daemon wrote, so they are reported as
    /// nothing rather than as half of one.
    pub fn committed_program(&self) -> Option<CoolingProgram> {
        match (
            self.committed_target(Channel::Pump)?,
            self.committed_target(Channel::Fan)?,
        ) {
            (Target::Fixed(pump), Target::Fixed(fan)) => Some(CoolingProgram::Fixed {
                pump: *pump,
                fan: *fan,
            }),
            (Target::Curve(pump), Target::Curve(fan)) => Some(CoolingProgram::Curve {
                pump: *pump,
                fan: *fan,
            }),
            _ => None,
        }
    }

    fn commit(&mut self, channel: Channel, target: Target) {
        self.committed.insert(
            channel,
            Commit {
                target,
                at_unix_ms: crate::now_unix_ms(),
            },
        );
    }

    fn uncommit(&mut self, channel: Channel) {
        self.committed.remove(&channel);
    }

    /// Apply one program, writing only what is not already in place.
    ///
    /// `now` is passed in rather than read here so the cadence floor can be
    /// exercised without a test sleeping through the interval it checks.
    pub fn apply(&mut self, now: Instant, program: &CoolingProgram) -> ApplyOutcome {
        let targets = match program {
            // The safe program leaves the device on whatever it is running.
            CoolingProgram::Onboard => {
                return ApplyOutcome::untouched(HardwareState::Onboard);
            }
            CoolingProgram::Fixed { pump, fan } => vec![
                (Channel::Pump, Target::Fixed(*pump)),
                (Channel::Fan, Target::Fixed(*fan)),
            ],
            CoolingProgram::Curve { pump, fan } => vec![
                (Channel::Pump, Target::Curve(*pump)),
                (Channel::Fan, Target::Curve(*fan)),
            ],
        };

        if self.resolve().is_none() {
            return ApplyOutcome::untouched(HardwareState::NotApplied {
                reason: "No kraken2023 hwmon instance is bound to an allowlisted device, so \
                         nothing was written."
                    .to_string(),
            });
        }

        // Captured before the first write, with this process's own record of
        // the curve folded in, because the device cannot hand one back.
        let snapshots: Vec<ChannelSnapshot> = targets
            .iter()
            .map(|(channel, _)| {
                let committed = self.committed_curve(*channel);
                let control = self.control.as_ref();
                match control {
                    Some(control) => control.snapshot(*channel).with_curve(committed),
                    None => ChannelSnapshot {
                        channel: *channel,
                        mode: None,
                        duty: None,
                        curve: committed,
                    },
                }
            })
            .collect();

        // Which channels already hold their target is decided before anything
        // is written, because the cadence floor below must only see a
        // transaction that would actually reach the device.
        let settled: Vec<Option<ChannelReadback>> = targets
            .iter()
            .map(|(channel, target)| self.already_applied(*channel, target))
            .collect();

        // A request that sends nothing cannot overrun the firmware, so a
        // fully deduplicated Apply is never refused. Same ordering as
        // `LightingExecutor::apply`, for the same reason.
        if settled.iter().any(Option::is_none)
            && let Some(previous) = self.last_write
        {
            let elapsed = now.saturating_duration_since(previous);
            let minimum = Duration::from_millis(MIN_PROGRAM_INTERVAL_MS);
            if elapsed < minimum {
                return ApplyOutcome::untouched(HardwareState::NotApplied {
                    reason: format!(
                        "Refused: {} ms since the last write to the cooler, and the firmware is \
                         given at least {MIN_PROGRAM_INTERVAL_MS} ms between programs. Nothing \
                         was written. Try again in a moment.",
                        elapsed.as_millis()
                    ),
                });
            }
        }

        let mut readback = Vec::with_capacity(targets.len());
        let mut writes = 0;

        for ((channel, target), settled) in targets.iter().zip(settled) {
            if let Some(current) = settled {
                readback.push(current);
                continue;
            }

            // Stamped before the result is known: a write that failed still
            // reached the device, and the restoration in `abort` writes again.
            self.last_write = Some(now);
            match self.write(*channel, target) {
                Ok(applied) => {
                    writes += applied.writes;
                    self.commit(*channel, target.clone());
                    readback.push(applied.readback);
                }
                Err(failure) => {
                    return self.abort(&snapshots, &failure, writes, readback);
                }
            }
        }

        let mismatched: Vec<String> = readback
            .iter()
            .filter_map(|entry| {
                entry
                    .mismatch
                    .as_ref()
                    .map(|detail| format!("{}: {detail}", entry.channel))
            })
            .collect();

        let hardware = if mismatched.is_empty() {
            HardwareState::Confirmed
        } else {
            // The write landed but the device disagrees. Nothing further is
            // written until a readback succeeds.
            for (channel, _) in &targets {
                self.uncommit(*channel);
            }
            HardwareState::Uncertain {
                reason: format!(
                    "The write completed but the hardware did not read back as written. {}",
                    mismatched.join(" ")
                ),
            }
        };

        ApplyOutcome {
            hardware,
            writes,
            deduplicated: writes == 0,
            readback,
        }
    }

    /// Check every committed curve against the duty the device reports.
    ///
    /// This exists because `apply_curve` cannot prove anything on its own. The
    /// curve attributes are write-only on this driver, so a curve write returns
    /// `Confirmed` on the strength of the mode alone, and a curve the firmware
    /// silently discarded is indistinguishable from one it is running. The duty
    /// in `pwmN` is the firmware's own answer, so at a known liquid temperature
    /// it says which curve is actually in force.
    ///
    /// A channel that disagrees is uncommitted, which is what makes the next
    /// Apply write instead of deduplicating against a program the device never
    /// took. Reporting is left to the caller.
    ///
    /// The comparison accepts the points on either side of the measured
    /// temperature: the reading and the firmware's own interpolation are a
    /// moment apart, and a curve that climbs steeply would otherwise look wrong
    /// while both are merely disagreeing about a fraction of a degree.
    pub fn verify_curves(
        &mut self,
        kraken: &KrakenTelemetry,
        now_unix_ms: u64,
    ) -> Vec<CurveDivergence> {
        let Some(liquid_c) = kraken.liquid_temperature_c.copied() else {
            return Vec::new();
        };

        let mut diverged = Vec::new();
        for (channel, commit) in &self.committed {
            let Target::Curve(curve) = &commit.target else {
                continue;
            };
            if now_unix_ms.saturating_sub(commit.at_unix_ms) < CURVE_SETTLE_MS {
                continue;
            }
            let entry = kraken.channel(*channel);
            if entry.mode.copied() != Some(PwmMode::Curve) {
                // Not in curve mode at all. `already_applied` catches that on
                // its own and rewrites, so it is not this check's business.
                continue;
            }
            let (Some(reported), Some(expected)) = (entry.duty.copied(), curve.duty_at(liquid_c))
            else {
                continue;
            };
            if accepts_duty(curve, liquid_c, reported) {
                continue;
            }
            diverged.push(CurveDivergence {
                channel: *channel,
                liquid_temperature_mc: (liquid_c * 1_000.0).round() as i32,
                expected,
                reported,
            });
        }

        for divergence in &diverged {
            self.uncommit(divergence.channel);
        }
        diverged
    }

    /// The current readback when the channel already holds the target, or
    /// `None` when a write is needed.
    ///
    /// Both this process's record and the device's own state have to agree.
    /// Checking only the record would let an external change go unnoticed;
    /// checking only the device cannot see a curve, because curve attributes
    /// are write-only.
    fn already_applied(&mut self, channel: Channel, target: &Target) -> Option<ChannelReadback> {
        if self.committed_target(channel) != Some(target) {
            return None;
        }
        let control = self.control.as_ref()?;
        let mode = control.hwmon().mode(channel).copied()?;
        if mode != target.mode() {
            return None;
        }

        let duty = control.hwmon().duty(channel).copied();
        if let Target::Fixed(expected) = target
            && duty != Some(*expected)
        {
            return None;
        }

        let mut entry = ChannelReadback::new(channel);
        entry.mode = Some(mode);
        entry.duty = duty;
        Some(entry)
    }

    fn write(&self, channel: Channel, target: &Target) -> Result<Applied, WriteFailure> {
        let Some(control) = self.control.as_ref() else {
            return Err(WriteFailure {
                attribute: "hwmon".to_string(),
                detail: "the device disappeared before the write".to_string(),
            });
        };
        match target {
            Target::Fixed(duty) => control.apply_fixed(channel, *duty),
            Target::Curve(curve) => control.apply_curve(channel, curve),
        }
    }

    /// Undo a failed transaction and report what could be proven afterwards.
    fn abort(
        &mut self,
        snapshots: &[ChannelSnapshot],
        failure: &WriteFailure,
        writes: u32,
        readback: Vec<ChannelReadback>,
    ) -> ApplyOutcome {
        let mut restored = true;
        let mut details = Vec::new();

        for snapshot in snapshots {
            let outcome = match self.control.as_ref() {
                Some(control) => control.restore(snapshot),
                None => Ok(false),
            };
            match outcome {
                Ok(true) => {
                    // Put the record back to what the channel is running now.
                    match (snapshot.mode, &snapshot.curve, snapshot.duty) {
                        (Some(PwmMode::Curve), Some(curve), _) => {
                            self.commit(snapshot.channel, Target::Curve(*curve));
                        }
                        (Some(PwmMode::Fixed), _, Some(duty)) => {
                            self.commit(snapshot.channel, Target::Fixed(duty));
                        }
                        _ => self.uncommit(snapshot.channel),
                    }
                }
                Ok(false) => {
                    restored = false;
                    self.uncommit(snapshot.channel);
                    details.push(format!(
                        "{} could not be confirmed back on its previous program.",
                        snapshot.channel
                    ));
                }
                Err(error) => {
                    restored = false;
                    self.uncommit(snapshot.channel);
                    details.push(format!("Restoring {} failed: {error}", snapshot.channel));
                }
            }
        }

        let hardware = if restored {
            HardwareState::NotApplied {
                reason: format!(
                    "The write failed at {failure}. Every channel was restored to the program \
                     it was running, confirmed by readback, and no further write was sent."
                ),
            }
        } else {
            HardwareState::Uncertain {
                reason: format!(
                    "The write failed at {failure}. {} The hardware state cannot be confirmed \
                     and no further write is sent until a readback succeeds.",
                    details.join(" ")
                ),
            }
        };

        ApplyOutcome {
            hardware,
            writes,
            deduplicated: false,
            readback,
        }
    }
}

/// Whether `reported` is consistent with `curve` around `liquid_c`.
///
/// The window spans the point the temperature names and its two neighbors,
/// widened by the driver's percentage rounding in both directions.
fn accepts_duty(curve: &TemperatureCurve, liquid_c: f32, reported: u8) -> bool {
    // A temperature that names no point cannot contradict the curve, so nothing
    // is claimed about the device from it. This used to be reached only because
    // `duty_at` had already turned the same temperature away two lines earlier
    // at the single call site, which left the slice below relying on a check
    // made somewhere else.
    let Some(center) = TemperatureCurve::point_index_for(liquid_c) else {
        return true;
    };
    let first = center.saturating_sub(1);
    let last = (center + 1).min(CURVE_POINT_COUNT - 1);
    let window = &curve.points()[first..=last];
    let (Some(low), Some(high)) = (window.iter().min(), window.iter().max()) else {
        return true;
    };
    reported >= low.saturating_sub(CURVE_DUTY_TOLERANCE)
        && reported <= high.saturating_add(CURVE_DUTY_TOLERANCE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::profile::CurveNodes;
    use kori_hardware_linux::testing::{FakeSysfs, running_as_root};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A clock that jumps a full second per read.
    ///
    /// Every test that is not about cadence uses it, so the floor never
    /// interferes with what the test is actually asserting, and no test sleeps
    /// through a real interval to get past it.
    struct Clock(Instant);

    impl Clock {
        fn new() -> Self {
            Self(Instant::now())
        }

        fn tick(&mut self) -> Instant {
            self.0 += Duration::from_secs(1);
            self.0
        }
    }

    fn executor(name: &str, grant: bool) -> (FakeSysfs, PathBuf, CoolingExecutor) {
        let fake = FakeSysfs::new(name);
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();
        if grant {
            for channel in 1..=2 {
                fake.grant_write(&hwmon, &format!("pwm{channel}"));
                fake.grant_write(&hwmon, &format!("pwm{channel}_enable"));
                for point in 1..=40 {
                    fake.grant_write(&hwmon, &format!("temp{channel}_auto_point{point}_pwm"));
                }
            }
        }
        let executor = CoolingExecutor::open(&SysfsRoot::new(fake.root_path()));
        (fake, hwmon, executor)
    }

    #[test]
    fn a_fixed_program_writes_both_channels_and_confirms_them() {
        let mut clock = Clock::new();
        let (_fake, _hwmon, mut executor) = executor("cooling-fixed", true);

        let outcome = executor.apply(clock.tick(), &CoolingProgram::Fixed { pump: 180, fan: 90 });
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
        assert_eq!(outcome.writes, 4);
        assert!(!outcome.deduplicated);

        let pump = outcome.readback_for(Channel::Pump).unwrap();
        assert_eq!(pump.mode, Some(PwmMode::Fixed));
        assert_eq!(pump.duty, Some(180));
        assert_eq!(outcome.readback_for(Channel::Fan).unwrap().duty, Some(90));
    }

    #[test]
    fn repeating_the_current_program_performs_zero_writes() {
        let mut clock = Clock::new();
        let (_fake, _hwmon, mut executor) = executor("cooling-dedup", true);
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };

        assert_eq!(executor.apply(clock.tick(), &program).writes, 4);
        for _ in 0..5 {
            let repeat = executor.apply(clock.tick(), &program);
            assert_eq!(repeat.writes, 0, "a repeat must not touch the device");
            assert!(repeat.deduplicated);
            assert_eq!(repeat.hardware, HardwareState::Confirmed);
            assert_eq!(repeat.readback_for(Channel::Pump).unwrap().duty, Some(180));
        }

        // A different value is not a repeat.
        assert_eq!(
            executor
                .apply(clock.tick(), &CoolingProgram::Fixed { pump: 200, fan: 90 })
                .writes,
            2,
            "only the channel that changed is written"
        );
    }

    #[test]
    fn a_channel_moved_behind_our_back_is_rewritten_rather_than_deduplicated() {
        let mut clock = Clock::new();
        let (_fake, _hwmon, mut executor) = executor("cooling-drift", true);
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };
        executor.apply(clock.tick(), &program);

        // Something else puts the pump back on the failsafe.
        let control = executor.control.as_ref().unwrap().clone();
        std::fs::write(control.hwmon().attribute("pwm1_enable"), "0").unwrap();

        let outcome = executor.apply(clock.tick(), &program);
        assert_eq!(outcome.writes, 2, "the drifted channel is written again");
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
    }

    #[test]
    fn a_curve_program_writes_forty_one_attributes_per_channel() {
        let mut clock = Clock::new();
        let (fake, hwmon, mut executor) = executor("cooling-curve", true);
        let curve = CurveNodes::starting_ramp().interpolate();

        let outcome = executor.apply(
            clock.tick(),
            &CoolingProgram::Curve {
                pump: curve,
                fan: curve,
            },
        );
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
        assert_eq!(outcome.writes, 82);
        assert_eq!(fake.written_curve(&hwmon, 1), *curve.points());
        assert_eq!(fake.written_curve(&hwmon, 2), *curve.points());

        // The same curve again is deduplicated even though the device cannot
        // read its points back: the record is what makes that safe.
        let repeat = executor.apply(
            clock.tick(),
            &CoolingProgram::Curve {
                pump: curve,
                fan: curve,
            },
        );
        assert_eq!(repeat.writes, 0);
        assert!(repeat.deduplicated);
    }

    fn kraken_running(liquid_c: f32, pump_duty: u8, fan_duty: u8) -> KrakenTelemetry {
        use kori_core::telemetry::{ChannelTelemetry, Reading};
        let channel = |channel, duty| ChannelTelemetry {
            channel,
            rpm: Reading::valid(1_800),
            duty: Reading::valid(duty),
            mode: Reading::valid(PwmMode::Curve),
        };
        KrakenTelemetry {
            at_unix_ms: 1_000,
            present: true,
            liquid_temperature_c: Reading::valid(liquid_c),
            pump: channel(Channel::Pump, pump_duty),
            fan: channel(Channel::Fan, fan_duty),
        }
    }

    /// The failure this check exists for, as it presented on real hardware: a
    /// curve committed and reported `Confirmed`, while the device kept running
    /// the previous one. At 30 C the starting ramp commands 140, so a device
    /// reporting 140 under a committed flat-255 curve is running neither the
    /// curve it was told to nor anything close to it.
    #[test]
    fn a_curve_the_device_never_took_is_caught_and_uncommitted() {
        let mut clock = Clock::new();
        let (_fake, _hwmon, mut executor) = executor("cooling-verify", true);
        let curve = CurveNodes::flat(255).interpolate();
        let program = CoolingProgram::Curve {
            pump: curve,
            fan: curve,
        };
        assert_eq!(
            executor.apply(clock.tick(), &program).hardware,
            HardwareState::Confirmed
        );

        let running = kraken_running(30.0, 140, 140);
        let settled = crate::now_unix_ms() + CURVE_SETTLE_MS + 1_000;

        // Nothing is concluded before the device has had a report to answer in.
        assert!(
            executor
                .verify_curves(&running, crate::now_unix_ms())
                .is_empty(),
            "a curve is not contradicted before it has settled"
        );

        let diverged = executor.verify_curves(&running, settled);
        assert_eq!(diverged.len(), 2, "both channels disagree");
        let pump = diverged
            .iter()
            .find(|entry| entry.channel == Channel::Pump)
            .expect("the pump diverged");
        assert_eq!(pump.expected, 255);
        assert_eq!(pump.reported, 140);
        assert_eq!(pump.liquid_temperature_mc, 30_000);

        // Uncommitted, so the next Apply writes instead of deduplicating
        // against a program the device never took. That is the whole point:
        // without it the operator can never get the curve applied at all.
        assert!(executor.committed_target(Channel::Pump).is_none());
        assert!(executor.apply(clock.tick(), &program).writes > 0);
    }

    #[test]
    fn a_curve_the_device_is_running_is_left_committed() {
        let mut clock = Clock::new();
        let (_fake, _hwmon, mut executor) = executor("cooling-verify-ok", true);
        let curve = CurveNodes::starting_ramp().interpolate();
        executor.apply(
            clock.tick(),
            &CoolingProgram::Curve {
                pump: curve,
                fan: curve,
            },
        );

        let settled = crate::now_unix_ms() + CURVE_SETTLE_MS + 1_000;
        // 140 is exactly what this curve commands at 30 C.
        assert_eq!(curve.duty_at(30.0), Some(140));
        assert!(
            executor
                .verify_curves(&kraken_running(30.0, 140, 140), settled)
                .is_empty()
        );

        // The driver stores duty as a percentage, so a value comes back up to
        // two steps away. That is rounding, not a divergence.
        assert!(
            executor
                .verify_curves(&kraken_running(30.0, 138, 142), settled)
                .is_empty()
        );
        assert!(executor.committed_target(Channel::Pump).is_some());
    }

    #[test]
    fn a_program_arriving_too_soon_after_a_write_is_refused_without_touching_the_device() {
        let (_fake, _hwmon, mut executor) = executor("cooling-cadence", true);
        let start = Instant::now();
        assert_eq!(
            executor
                .apply(start, &CoolingProgram::Fixed { pump: 180, fan: 90 })
                .writes,
            4
        );

        // A different program, one millisecond under the floor. The firmware is
        // documented to discard changes that arrive too numerous at once, so
        // this is refused rather than sent.
        let too_soon = start + Duration::from_millis(MIN_PROGRAM_INTERVAL_MS - 1);
        let outcome = executor.apply(too_soon, &CoolingProgram::Fixed { pump: 200, fan: 90 });
        assert_eq!(outcome.writes, 0);
        let HardwareState::NotApplied { reason } = &outcome.hardware else {
            panic!("expected a refusal, got {:?}", outcome.hardware);
        };
        assert!(reason.contains("Refused"), "{reason}");
        assert!(reason.contains("Nothing was written"), "{reason}");

        // Nothing moved, and the earlier program is still committed, so the
        // refusal left no trace on the hardware or on the record.
        let control = executor.control.as_ref().unwrap();
        assert_eq!(control.hwmon().duty(Channel::Pump).copied(), Some(180));
        assert_eq!(
            executor.committed_target(Channel::Pump),
            Some(&Target::Fixed(180))
        );

        // On the floor exactly, it goes through.
        let allowed = start + Duration::from_millis(MIN_PROGRAM_INTERVAL_MS);
        assert_eq!(
            executor
                .apply(allowed, &CoolingProgram::Fixed { pump: 200, fan: 90 })
                .writes,
            2
        );
    }

    #[test]
    fn a_repeat_is_never_refused_by_cadence_because_it_sends_nothing() {
        let (_fake, _hwmon, mut executor) = executor("cooling-cadence-repeat", true);
        let start = Instant::now();
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };
        executor.apply(start, &program);

        // Hammered far below the floor, and every one of them is answered from
        // the record. A request that writes nothing cannot overrun anything.
        for step in 1..20 {
            let repeat = executor.apply(start + Duration::from_millis(step), &program);
            assert_eq!(repeat.writes, 0);
            assert!(repeat.deduplicated);
            assert_eq!(repeat.hardware, HardwareState::Confirmed);
        }
    }

    #[test]
    fn the_safe_program_is_never_refused_by_cadence() {
        let (_fake, _hwmon, mut executor) = executor("cooling-cadence-onboard", true);
        let start = Instant::now();
        executor.apply(start, &CoolingProgram::Fixed { pump: 180, fan: 90 });

        // Onboard writes nothing by construction and is the program the daemon
        // falls back to, so it must never be refusable.
        let outcome = executor.apply(start + Duration::from_millis(1), &CoolingProgram::Onboard);
        assert_eq!(outcome.hardware, HardwareState::Onboard);
        assert_eq!(outcome.writes, 0);
    }

    #[test]
    fn the_onboard_program_writes_nothing_at_all() {
        let mut clock = Clock::new();
        let (fake, hwmon, mut executor) = executor("cooling-onboard", true);
        let outcome = executor.apply(clock.tick(), &CoolingProgram::Onboard);
        assert_eq!(outcome.hardware, HardwareState::Onboard);
        assert_eq!(outcome.writes, 0);
        assert!(outcome.readback.is_empty());
        assert_eq!(fake.written_curve(&hwmon, 1), vec![0u8; CURVE_POINT_COUNT]);
    }

    #[test]
    fn a_partial_failure_restores_the_previous_program_and_reports_it() {
        let mut clock = Clock::new();
        if running_as_root() {
            return; // Root writes through the permission bits this test sets.
        }
        let (_fake, hwmon, mut executor) = executor("cooling-partial", true);
        // The pump is left on a confirmed fixed duty.
        assert_eq!(
            executor
                .apply(clock.tick(), &CoolingProgram::Fixed { pump: 180, fan: 90 })
                .hardware,
            HardwareState::Confirmed
        );

        // The fan's mode attribute becomes unwritable mid-session, which is
        // how a revoked permission or a disappearing device presents.
        std::fs::set_permissions(
            hwmon.join("pwm2_enable"),
            std::fs::Permissions::from_mode(0o444),
        )
        .unwrap();

        let outcome = executor.apply(
            clock.tick(),
            &CoolingProgram::Fixed {
                pump: 200,
                fan: 120,
            },
        );
        let HardwareState::NotApplied { reason } = &outcome.hardware else {
            panic!(
                "expected a confirmed restoration, got {:?}",
                outcome.hardware
            );
        };
        assert!(reason.contains("pwm2_enable"), "{reason}");
        assert!(reason.contains("restored"), "{reason}");

        // The pump is back where it was, not left on the new value.
        let control = executor.control.as_ref().unwrap();
        assert_eq!(control.hwmon().duty(Channel::Pump).copied(), Some(180));
        assert_eq!(
            control.hwmon().mode(Channel::Pump).copied(),
            Some(PwmMode::Fixed)
        );
    }

    #[test]
    fn a_failure_the_restoration_cannot_undo_reports_an_uncertain_state() {
        let mut clock = Clock::new();
        let (fake, hwmon, mut executor) = executor("cooling-uncertain", true);

        // The fan's mode attribute vanishes, which is how a device that
        // disappears mid-transaction presents to the writer. The duty still
        // moves, so the channel genuinely ends up somewhere this process
        // cannot put back.
        fake.remove_attribute(&hwmon, "pwm2_enable");

        let outcome = executor.apply(
            clock.tick(),
            &CoolingProgram::Fixed {
                pump: 200,
                fan: 120,
            },
        );
        let HardwareState::Uncertain { reason } = &outcome.hardware else {
            panic!("expected uncertain, got {:?}", outcome.hardware);
        };
        assert!(reason.contains("pwm2_enable"), "{reason}");
        assert!(reason.contains("Fan"), "{reason}");
        assert!(reason.contains("cannot be confirmed"), "{reason}");
        assert!(reason.contains("no further write"), "{reason}");

        // Nothing stays committed for a channel whose state is unknown, so the
        // next Apply writes rather than deduplicating against a guess.
        assert!(executor.committed_target(Channel::Fan).is_none());
    }

    #[test]
    fn a_machine_without_a_bound_driver_reports_that_nothing_was_written() {
        let mut clock = Clock::new();
        let fake = FakeSysfs::new("cooling-absent");
        fake.add_kraken();
        let mut executor = CoolingExecutor::open(&SysfsRoot::new(fake.root_path()));

        let outcome = executor.apply(clock.tick(), &CoolingProgram::Fixed { pump: 180, fan: 90 });
        assert_eq!(outcome.writes, 0);
        let HardwareState::NotApplied { reason } = &outcome.hardware else {
            panic!("expected NotApplied, got {:?}", outcome.hardware);
        };
        assert!(reason.contains("kraken2023"), "{reason}");
    }

    #[test]
    fn forgetting_the_record_forces_the_next_apply_to_write_again() {
        let mut clock = Clock::new();
        let (_fake, _hwmon, mut executor) = executor("cooling-forget", true);
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };
        executor.apply(clock.tick(), &program);
        assert_eq!(executor.apply(clock.tick(), &program).writes, 0);

        executor.forget();
        assert_eq!(
            executor.apply(clock.tick(), &program).writes,
            4,
            "a reconnect must not deduplicate against a stale record"
        );
    }
}
