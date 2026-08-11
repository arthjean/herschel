// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The only caller that writes to the RGB controller.
//!
//! Mirrors [`crate::cooling`]: one owner, one queue, one record of what was
//! committed. Everything the controller is asked to do passes through
//! [`LightingExecutor::apply`], so there is exactly one place where a lighting
//! byte leaves this process.
//!
//! Two rules are enforced here rather than at the screen, because the screen is
//! not a trusted input. A request identical to the committed state is
//! deduplicated and writes nothing. A request arriving sooner than
//! [`kori_core::lighting::MIN_COMMAND_INTERVAL_MS`] after the previous one on
//! the same channel is refused, because the controller acknowledges nothing and
//! cadence is the only backpressure there is.

use std::collections::BTreeMap;
use std::time::Instant;

use kori_core::ipc::{ChannelState, HardwareState, LightingOutcome};
use kori_core::lighting::{
    LightingCommand, LightingError, LightingProgram, MIN_COMMAND_INTERVAL_MS,
};
use kori_hardware_linux::rgb::{self, HidTransport};

/// Owns the controller handle and the record of what it was told.
pub struct LightingExecutor {
    transport: Option<Box<dyn HidTransport>>,
    /// Channels the controller reported, with their accessories.
    channels: Vec<ChannelReport>,
    /// The last program committed per one-based channel.
    committed: BTreeMap<u8, LightingProgram>,
    /// When the last report was sent per one-based channel.
    last_sent: BTreeMap<u8, Instant>,
}

/// One channel as the controller described it.
#[derive(Debug, Clone)]
struct ChannelReport {
    index: u8,
    accessories: Vec<String>,
}

impl LightingExecutor {
    /// An executor with no controller behind it.
    ///
    /// Every command is refused with the reason the caller recorded in the
    /// capability record, so an absent controller is a disabled control rather
    /// than a special case scattered through the daemon.
    pub fn absent() -> Self {
        Self {
            transport: None,
            channels: Vec::new(),
            committed: BTreeMap::new(),
            last_sent: BTreeMap::new(),
        }
    }

    /// An executor bound to a controller that answered its topology.
    pub fn connected(
        transport: Box<dyn HidTransport>,
        channels: &[kori_core::capability::RgbChannel],
    ) -> Self {
        Self {
            transport: Some(transport),
            channels: channels
                .iter()
                .map(|channel| ChannelReport {
                    index: channel.index,
                    accessories: channel
                        .accessories
                        .iter()
                        .map(|accessory| accessory.name.clone())
                        .collect(),
                })
                .collect(),
            committed: BTreeMap::new(),
            last_sent: BTreeMap::new(),
        }
    }

    /// Channels available for a command, zero when no controller answered.
    pub fn channel_count(&self) -> u8 {
        self.channels.len() as u8
    }

    /// Per-channel state for [`kori_core::ipc::DaemonStatus`].
    pub fn state(&self) -> Vec<ChannelState> {
        self.channels
            .iter()
            .map(|channel| ChannelState {
                channel: channel.index,
                accessories: channel.accessories.clone(),
                committed: self.committed.get(&channel.index).cloned(),
            })
            .collect()
    }

    /// The program this channel is showing, as far as the daemon knows.
    pub fn committed(&self, channel: u8) -> Option<&LightingProgram> {
        self.committed.get(&channel)
    }

    /// Apply one command, refusing everything the hardware must not receive.
    ///
    /// `now` is passed in rather than read here so cadence can be exercised
    /// without a test sleeping through the interval it is checking.
    pub fn apply(
        &mut self,
        now: Instant,
        command: &LightingCommand,
    ) -> Result<LightingOutcome, LightingError> {
        // A repeat of the committed state is answered from the record. This
        // runs before the cadence check on purpose: a request that sends
        // nothing cannot overrun a controller.
        if self.committed.get(&command.channel) == Some(&command.program) {
            return Ok(LightingOutcome {
                channel: command.channel,
                program: command.program.clone(),
                hardware: HardwareState::Confirmed,
                writes: 0,
                deduplicated: true,
            });
        }

        if let Some(previous) = self.last_sent.get(&command.channel) {
            let elapsed = now.saturating_duration_since(*previous);
            if elapsed < rgb::minimum_command_interval() {
                return Err(LightingError::CadenceTooFast {
                    channel: command.channel,
                    minimum_interval_ms: MIN_COMMAND_INTERVAL_MS,
                    elapsed_ms: elapsed.as_millis() as u64,
                });
            }
        }

        let Some(transport) = self.transport.as_mut() else {
            return Ok(LightingOutcome {
                channel: command.channel,
                program: command.program.clone(),
                hardware: HardwareState::NotApplied {
                    reason: "No lighting controller is connected.".to_string(),
                },
                writes: 0,
                deduplicated: false,
            });
        };

        match rgb::apply(transport.as_mut(), command.channel, &command.program) {
            Ok(_) => {
                self.last_sent.insert(command.channel, now);
                self.committed
                    .insert(command.channel, command.program.clone());
                Ok(LightingOutcome {
                    channel: command.channel,
                    program: command.program.clone(),
                    hardware: HardwareState::Confirmed,
                    writes: 1,
                    deduplicated: false,
                })
            }
            Err(error) => {
                // The report may have reached the controller before the failure
                // and there is no way to read any channel back, so the record of
                // what the controller is showing is dropped rather than left
                // claiming programs that may not be running.
                //
                // Every channel, not just the one addressed. A write fails
                // because the node went away, the permission was revoked or the
                // device stopped answering, and none of those are facts about
                // one channel. Dropping only the addressed one left the others
                // deduplicating against a controller that had since come back on
                // its firmware defaults, which is the reconnect defect the other
                // two executors have an explicit `forget` for. This path is the
                // controller's equivalent, and it needs no separate trigger
                // because it is the only way this process learns the link is
                // bad: the controller acknowledges nothing, so a failed write is
                // the whole of the evidence available.
                self.last_sent.insert(command.channel, now);
                self.committed.clear();
                Ok(LightingOutcome {
                    channel: command.channel,
                    program: command.program.clone(),
                    hardware: HardwareState::Uncertain {
                        reason: error.to_string(),
                    },
                    writes: 0,
                    deduplicated: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::capability::{Evidenced, RgbAccessory, RgbChannel};
    use kori_core::lighting::{Brightness, Rgb};
    use kori_hardware_linux::testing::FakeController;
    use std::time::Duration;

    fn channels(count: u8) -> Vec<RgbChannel> {
        (1..=count)
            .map(|index| RgbChannel {
                index,
                accessories: vec![RgbAccessory {
                    id: 0x04,
                    name: "HUE 2 LED Strip 300 mm".to_string(),
                }],
                led_count: Evidenced::unknown("not reported", "report 0x21 0x03"),
            })
            .collect()
    }

    fn fixed(hex: &str) -> LightingProgram {
        LightingProgram::Fixed {
            color: Rgb::parse_hex(hex).unwrap(),
            brightness: Brightness::new(60).unwrap(),
        }
    }

    /// An executor over a fake controller, plus a handle to inspect it.
    ///
    /// The transport is boxed into the executor, so the assertions read the
    /// controller through a shared counter rather than through the box.
    fn executor(count: u8) -> (LightingExecutor, std::sync::Arc<Recorder>) {
        let recorder = std::sync::Arc::new(Recorder::default());
        let transport = RecordingController {
            inner: FakeController::new("1.0.0", count as usize),
            recorder: std::sync::Arc::clone(&recorder),
        };
        (
            LightingExecutor::connected(Box::new(transport), &channels(count)),
            recorder,
        )
    }

    #[derive(Default)]
    struct Recorder {
        commands: std::sync::Mutex<Vec<[u8; kori_hardware_linux::rgb::REPORT_BYTES]>>,
        fail: std::sync::atomic::AtomicBool,
    }

    impl Recorder {
        fn count(&self) -> usize {
            self.commands.lock().map(|c| c.len()).unwrap_or(0)
        }

        fn last(&self) -> Option<[u8; kori_hardware_linux::rgb::REPORT_BYTES]> {
            self.commands.lock().ok().and_then(|c| c.last().copied())
        }
    }

    struct RecordingController {
        inner: FakeController,
        recorder: std::sync::Arc<Recorder>,
    }

    impl HidTransport for RecordingController {
        fn write_report(
            &mut self,
            report: &[u8; kori_hardware_linux::rgb::REPORT_BYTES],
        ) -> Result<(), kori_hardware_linux::rgb::RgbError> {
            if self.recorder.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(kori_hardware_linux::rgb::RgbError::PermissionDenied {
                    path: "/dev/hidraw12".to_string(),
                });
            }
            self.inner.write_report(report)?;
            if [report[0], report[1]] == kori_hardware_linux::rgb::packet::COLOR_COMMAND
                && let Ok(mut commands) = self.recorder.commands.lock()
            {
                commands.push(*report);
            }
            Ok(())
        }

        fn read_report(
            &mut self,
            timeout: Duration,
        ) -> Result<
            Option<[u8; kori_hardware_linux::rgb::REPORT_BYTES]>,
            kori_hardware_linux::rgb::RgbError,
        > {
            self.inner.read_report(timeout)
        }

        fn source(&self) -> String {
            self.inner.source()
        }
    }

    #[test]
    fn a_command_sends_exactly_one_report_and_becomes_the_committed_state() {
        let (mut executor, recorder) = executor(3);
        let now = Instant::now();

        let outcome = executor
            .apply(
                now,
                &LightingCommand {
                    channel: 2,
                    program: fixed("7C5CFF"),
                },
            )
            .unwrap();

        assert_eq!(outcome.writes, 1);
        assert!(!outcome.deduplicated);
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
        assert_eq!(recorder.count(), 1);
        // Channel 2 is bit 1, addressed twice in the header.
        let report = recorder.last().unwrap();
        assert_eq!(&report[0..4], &[0x2a, 0x04, 0x02, 0x02]);
        assert_eq!(executor.committed(2), Some(&fixed("7C5CFF")));
        assert_eq!(executor.committed(1), None, "only channel 2 was addressed");
    }

    #[test]
    fn repeating_the_committed_state_writes_nothing() {
        let (mut executor, recorder) = executor(3);
        let start = Instant::now();
        let command = LightingCommand {
            channel: 1,
            program: fixed("00FF00"),
        };

        executor.apply(start, &command).unwrap();
        assert_eq!(recorder.count(), 1);

        // Immediately, so the deduplication cannot be mistaken for the cadence
        // refusal that would otherwise apply at this instant.
        let repeat = executor.apply(start, &command).unwrap();
        assert!(repeat.deduplicated);
        assert_eq!(repeat.writes, 0);
        assert_eq!(repeat.hardware, HardwareState::Confirmed);
        assert_eq!(recorder.count(), 1, "no additional report was sent");
    }

    #[test]
    fn a_command_faster_than_the_floor_is_refused_before_the_write() {
        let (mut executor, recorder) = executor(3);
        let start = Instant::now();

        executor
            .apply(
                start,
                &LightingCommand {
                    channel: 1,
                    program: fixed("FF0000"),
                },
            )
            .unwrap();

        let error = executor
            .apply(
                start + Duration::from_millis(MIN_COMMAND_INTERVAL_MS - 1),
                &LightingCommand {
                    channel: 1,
                    program: fixed("00FF00"),
                },
            )
            .unwrap_err();
        assert!(matches!(error, LightingError::CadenceTooFast { .. }));
        assert_eq!(recorder.count(), 1, "the refused command reached no device");
        assert_eq!(
            executor.committed(1),
            Some(&fixed("FF0000")),
            "a refused command must not disturb the committed state"
        );

        // At the floor exactly, it lands.
        executor
            .apply(
                start + Duration::from_millis(MIN_COMMAND_INTERVAL_MS),
                &LightingCommand {
                    channel: 1,
                    program: fixed("00FF00"),
                },
            )
            .unwrap();
        assert_eq!(recorder.count(), 2);
    }

    #[test]
    fn cadence_is_tracked_per_channel_rather_than_globally() {
        let (mut executor, recorder) = executor(3);
        let start = Instant::now();

        for channel in 1..=3 {
            executor
                .apply(
                    start,
                    &LightingCommand {
                        channel,
                        program: fixed("101010"),
                    },
                )
                .unwrap();
        }
        assert_eq!(recorder.count(), 3, "three channels, three reports");
    }

    #[test]
    fn a_failed_write_reports_uncertain_and_forgets_the_channel() {
        let (mut executor, recorder) = executor(3);
        let start = Instant::now();
        recorder
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let outcome = executor
            .apply(
                start,
                &LightingCommand {
                    channel: 1,
                    program: fixed("FFFFFF"),
                },
            )
            .unwrap();

        assert_eq!(outcome.writes, 0);
        match &outcome.hardware {
            HardwareState::Uncertain { reason } => assert!(reason.contains("udev"), "{reason}"),
            other => panic!("expected uncertain, got {other:?}"),
        }
        assert_eq!(
            executor.committed(1),
            None,
            "a channel that may or may not have taken the report is not claimed"
        );
    }

    /// A refused write drops the record of the *whole* controller.
    ///
    /// This is the reconnect defect, on the one device that cannot be asked
    /// whether it is still there. A write fails because the node went away or
    /// the permission was revoked, and neither is a fact about the channel that
    /// happened to be addressed. Keeping the untouched channels committed left
    /// them deduplicating against a controller that had come back on its
    /// firmware defaults, and the operator could not get them lit again.
    #[test]
    fn a_refused_write_drops_every_channel_rather_than_only_the_one_addressed() {
        let (mut executor, recorder) = executor(3);
        let start = Instant::now();

        for channel in 1..=3 {
            executor
                .apply(
                    start,
                    &LightingCommand {
                        channel,
                        program: fixed("FF0000"),
                    },
                )
                .unwrap();
        }
        assert!((1..=3).all(|channel| executor.committed(channel).is_some()));

        // The controller goes away. The next write is the only way this process
        // can find out, because nothing here acknowledges anything.
        recorder
            .fail
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let elapsed = start + Duration::from_millis(MIN_COMMAND_INTERVAL_MS);
        executor
            .apply(
                elapsed,
                &LightingCommand {
                    channel: 1,
                    program: fixed("00FF00"),
                },
            )
            .unwrap();

        for channel in 1..=3 {
            assert_eq!(
                executor.committed(channel),
                None,
                "channel {channel} must not stay claimed after the link refused"
            );
        }

        // And once it answers again, every channel writes rather than
        // deduplicating against what it was showing before it left.
        recorder
            .fail
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let sent = recorder.count();
        let back = elapsed + Duration::from_millis(MIN_COMMAND_INTERVAL_MS);
        for channel in 1..=3 {
            let outcome = executor
                .apply(
                    back,
                    &LightingCommand {
                        channel,
                        program: fixed("FF0000"),
                    },
                )
                .unwrap();
            assert!(!outcome.deduplicated, "channel {channel} was deduplicated");
        }
        assert_eq!(recorder.count(), sent + 3);
    }

    #[test]
    fn an_absent_controller_refuses_without_pretending_to_write() {
        let mut executor = LightingExecutor::absent();
        assert_eq!(executor.channel_count(), 0);
        assert!(executor.state().is_empty());

        let outcome = executor
            .apply(
                Instant::now(),
                &LightingCommand {
                    channel: 1,
                    program: LightingProgram::Off,
                },
            )
            .unwrap();
        assert_eq!(outcome.writes, 0);
        assert!(matches!(outcome.hardware, HardwareState::NotApplied { .. }));
        assert_eq!(executor.committed(1), None);
    }

    #[test]
    fn the_reported_state_carries_the_accessories_the_controller_named() {
        let (mut executor, _recorder) = executor(2);
        executor
            .apply(
                Instant::now(),
                &LightingCommand {
                    channel: 1,
                    program: fixed("ABCDEF"),
                },
            )
            .unwrap();

        let state = executor.state();
        assert_eq!(state.len(), 2);
        assert_eq!(state[0].channel, 1);
        assert_eq!(state[0].accessories, vec!["HUE 2 LED Strip 300 mm"]);
        assert_eq!(state[0].committed, Some(fixed("ABCDEF")));
        assert_eq!(state[1].committed, None);
    }
}
