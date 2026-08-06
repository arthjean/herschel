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

use nzxt_core::ipc::{ApplyOutcome, ChannelReadback, HardwareState};
use nzxt_core::profile::{CURVE_POINT_COUNT, Channel, CoolingProgram, TemperatureCurve};
use nzxt_core::telemetry::PwmMode;
use nzxt_hardware_linux::SysfsRoot;
use nzxt_hardware_linux::control::{ChannelSnapshot, CoolingControl, WriteFailure};
use nzxt_hardware_linux::hwmon::KrakenHwmon;

/// Attributes one fixed-duty write touches: the duty and the mode.
const FIXED_WRITE_COUNT: u32 = 2;
/// Attributes one curve write touches: forty points and the mode.
const CURVE_WRITE_COUNT: u32 = CURVE_POINT_COUNT as u32 + 1;

/// What one channel was last told to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Fixed(u8),
    Curve(TemperatureCurve),
}

impl Target {
    fn write_count(&self) -> u32 {
        match self {
            Self::Fixed(_) => FIXED_WRITE_COUNT,
            Self::Curve(_) => CURVE_WRITE_COUNT,
        }
    }

    fn mode(&self) -> PwmMode {
        match self {
            Self::Fixed(_) => PwmMode::Fixed,
            Self::Curve(_) => PwmMode::Curve,
        }
    }
}

/// The sole writer of the thermal path.
pub struct CoolingExecutor {
    sysfs: SysfsRoot,
    control: Option<CoolingControl>,
    /// The last program this process confirmed on each channel.
    committed: Vec<(Channel, Target)>,
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
            committed: Vec::new(),
        }
    }

    /// True when a bound `kraken2023` instance was resolved.
    pub fn is_available(&mut self) -> bool {
        self.resolve().is_some()
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
        self.committed
            .iter()
            .find_map(|(entry, target)| match (entry == &channel, target) {
                (true, Target::Curve(curve)) => Some(curve.clone()),
                _ => None,
            })
    }

    fn committed_target(&self, channel: Channel) -> Option<&Target> {
        self.committed
            .iter()
            .find(|(entry, _)| entry == &channel)
            .map(|(_, target)| target)
    }

    fn commit(&mut self, channel: Channel, target: Target) {
        self.committed.retain(|(entry, _)| entry != &channel);
        self.committed.push((channel, target));
    }

    fn uncommit(&mut self, channel: Channel) {
        self.committed.retain(|(entry, _)| entry != &channel);
    }

    /// Apply one program, writing only what is not already in place.
    pub fn apply(&mut self, program: &CoolingProgram) -> ApplyOutcome {
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
                (Channel::Pump, Target::Curve(pump.clone())),
                (Channel::Fan, Target::Curve(fan.clone())),
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

        let mut readback = Vec::with_capacity(targets.len());
        let mut writes = 0;

        for (channel, target) in &targets {
            if let Some(current) = self.already_applied(*channel, target) {
                readback.push(current);
                continue;
            }

            match self.write(*channel, target) {
                Ok(entry) => {
                    writes += target.write_count();
                    self.commit(*channel, target.clone());
                    readback.push(entry);
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

    fn write(&self, channel: Channel, target: &Target) -> Result<ChannelReadback, WriteFailure> {
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
                            self.commit(snapshot.channel, Target::Curve(curve.clone()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::profile::CurveNodes;
    use nzxt_hardware_linux::testing::{FakeSysfs, running_as_root};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

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
        let (_fake, _hwmon, mut executor) = executor("cooling-fixed", true);

        let outcome = executor.apply(&CoolingProgram::Fixed { pump: 180, fan: 90 });
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
        let (_fake, _hwmon, mut executor) = executor("cooling-dedup", true);
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };

        assert_eq!(executor.apply(&program).writes, 4);
        for _ in 0..5 {
            let repeat = executor.apply(&program);
            assert_eq!(repeat.writes, 0, "a repeat must not touch the device");
            assert!(repeat.deduplicated);
            assert_eq!(repeat.hardware, HardwareState::Confirmed);
            assert_eq!(repeat.readback_for(Channel::Pump).unwrap().duty, Some(180));
        }

        // A different value is not a repeat.
        assert_eq!(
            executor
                .apply(&CoolingProgram::Fixed { pump: 200, fan: 90 })
                .writes,
            2,
            "only the channel that changed is written"
        );
    }

    #[test]
    fn a_channel_moved_behind_our_back_is_rewritten_rather_than_deduplicated() {
        let (_fake, _hwmon, mut executor) = executor("cooling-drift", true);
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };
        executor.apply(&program);

        // Something else puts the pump back on the failsafe.
        let control = executor.control.as_ref().unwrap().clone();
        std::fs::write(control.hwmon().attribute("pwm1_enable"), "0").unwrap();

        let outcome = executor.apply(&program);
        assert_eq!(outcome.writes, 2, "the drifted channel is written again");
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
    }

    #[test]
    fn a_curve_program_writes_forty_one_attributes_per_channel() {
        let (fake, hwmon, mut executor) = executor("cooling-curve", true);
        let curve = CurveNodes::starting_ramp().interpolate();

        let outcome = executor.apply(&CoolingProgram::Curve {
            pump: curve.clone(),
            fan: curve.clone(),
        });
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
        assert_eq!(outcome.writes, 82);
        assert_eq!(fake.written_curve(&hwmon, 1), curve.points);
        assert_eq!(fake.written_curve(&hwmon, 2), curve.points);

        // The same curve again is deduplicated even though the device cannot
        // read its points back: the record is what makes that safe.
        let repeat = executor.apply(&CoolingProgram::Curve {
            pump: curve.clone(),
            fan: curve,
        });
        assert_eq!(repeat.writes, 0);
        assert!(repeat.deduplicated);
    }

    #[test]
    fn the_onboard_program_writes_nothing_at_all() {
        let (fake, hwmon, mut executor) = executor("cooling-onboard", true);
        let outcome = executor.apply(&CoolingProgram::Onboard);
        assert_eq!(outcome.hardware, HardwareState::Onboard);
        assert_eq!(outcome.writes, 0);
        assert!(outcome.readback.is_empty());
        assert_eq!(fake.written_curve(&hwmon, 1), vec![0u8; CURVE_POINT_COUNT]);
    }

    #[test]
    fn a_partial_failure_restores_the_previous_program_and_reports_it() {
        if running_as_root() {
            return; // Root writes through the permission bits this test sets.
        }
        let (_fake, hwmon, mut executor) = executor("cooling-partial", true);
        // The pump is left on a confirmed fixed duty.
        assert_eq!(
            executor
                .apply(&CoolingProgram::Fixed { pump: 180, fan: 90 })
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

        let outcome = executor.apply(&CoolingProgram::Fixed {
            pump: 200,
            fan: 120,
        });
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
        let (fake, hwmon, mut executor) = executor("cooling-uncertain", true);

        // The fan's mode attribute vanishes, which is how a device that
        // disappears mid-transaction presents to the writer. The duty still
        // moves, so the channel genuinely ends up somewhere this process
        // cannot put back.
        fake.remove_attribute(&hwmon, "pwm2_enable");

        let outcome = executor.apply(&CoolingProgram::Fixed {
            pump: 200,
            fan: 120,
        });
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
        let fake = FakeSysfs::new("cooling-absent");
        fake.add_kraken();
        let mut executor = CoolingExecutor::open(&SysfsRoot::new(fake.root_path()));

        assert!(!executor.is_available());
        let outcome = executor.apply(&CoolingProgram::Fixed { pump: 180, fan: 90 });
        assert_eq!(outcome.writes, 0);
        let HardwareState::NotApplied { reason } = &outcome.hardware else {
            panic!("expected NotApplied, got {:?}", outcome.hardware);
        };
        assert!(reason.contains("kraken2023"), "{reason}");
    }

    #[test]
    fn forgetting_the_record_forces_the_next_apply_to_write_again() {
        let (_fake, _hwmon, mut executor) = executor("cooling-forget", true);
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };
        executor.apply(&program);
        assert_eq!(executor.apply(&program).writes, 0);

        executor.forget();
        assert_eq!(
            executor.apply(&program).writes,
            4,
            "a reconnect must not deduplicate against a stale record"
        );
    }
}
