// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The one connection the window has to the daemon.
//!
//! Every request runs on a worker thread, never on the UI thread. A local
//! socket is fast, but "fast" is not "bounded": a daemon that stalls would
//! otherwise freeze the window for the length of the client timeout, and a
//! frozen window cannot show the operator that anything is wrong.
//!
//! The worker owns a single connection and serializes polling and commands
//! through it, so the ordering the daemon sees is the ordering the operator
//! produced. Each cycle it publishes a complete [`LinkState`] and rings the
//! notifier, which is what wakes the view: the window repaints when new data
//! exists rather than on a timer of its own.
//!
//! A command does not wait for that cycle. The worker spends the gap between
//! polls waiting *on the command queue* rather than sleeping through it, so an
//! Apply leaves the process when it is pressed instead of at the next boundary.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use nzxt_core::client::{Client, ClientError};
use nzxt_core::display::DisplayPreset;
use nzxt_core::ipc::{ApplyOutcome, HardwareState, LightingOutcome, Request, Response};
use nzxt_core::lighting::LightingCommand;
use nzxt_core::profile::{CoolingProgram, Profile};

use crate::link::LinkState;

/// Longest the worker waits before noticing a shutdown request.
const SHUTDOWN_GRANULARITY: Duration = Duration::from_millis(25);

/// Something the operator asked the hardware to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Write a cooling program without saving it as a profile.
    Apply(CoolingProgram),
    /// Write one channel's lighting without saving it as a profile.
    ApplyLighting(LightingCommand),
    /// Show a preset on the panel without saving it as a profile.
    ApplyDisplay(DisplayPreset),
    SaveProfile(Profile),
    ActivateProfile(String),
    DeleteProfile(String),
}

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeSeverity {
    /// The hardware confirmed it.
    Confirmed,
    /// It was accepted but the hardware state is not confirmed.
    Unconfirmed,
    /// It was refused, and nothing was written.
    Refused,
}

/// The result of the last command, shown next to the control that issued it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutcome {
    pub at_unix_ms: u64,
    pub message: String,
    pub severity: OutcomeSeverity,
    /// Present when the command reached the cooling executor.
    pub hardware: Option<HardwareState>,
}

impl CommandOutcome {
    fn refused(message: impl Into<String>) -> Self {
        Self {
            at_unix_ms: now_unix_ms(),
            message: message.into(),
            severity: OutcomeSeverity::Refused,
            hardware: None,
        }
    }

    /// The outcome of one panel command.
    ///
    /// The panel answers no report that reads its picture back, so a confirmed
    /// command says the transfer completed and claims nothing about the glass.
    ///
    /// A deduplicated frame is not the same as an untouched panel: the
    /// brightness is a setting rather than a picture and travels whatever the
    /// pixels do, so the two are reported separately instead of collapsed into
    /// one "nothing was sent" that would be false exactly when the operator had
    /// just changed the brightness.
    fn from_display(outcome: &nzxt_core::ipc::DisplayOutcome) -> Self {
        let (severity, message) = match &outcome.hardware {
            HardwareState::Confirmed if outcome.deduplicated && outcome.brightness_sent => (
                OutcomeSeverity::Confirmed,
                format!(
                    "Panel brightness set to {}%. The picture was already correct, so no frame \
                     was sent.",
                    outcome.preset.brightness.percent()
                ),
            ),
            HardwareState::Confirmed if outcome.deduplicated => (
                OutcomeSeverity::Confirmed,
                "The panel already shows this. Nothing was sent.".to_string(),
            ),
            HardwareState::Confirmed => (
                OutcomeSeverity::Confirmed,
                format!(
                    "Panel set to {}. The transfer completed; the panel reports no picture to \
                     read back.",
                    outcome.preset.mode.label()
                ),
            ),
            HardwareState::Onboard => (
                OutcomeSeverity::Confirmed,
                "The panel keeps its own picture. Nothing was sent.".to_string(),
            ),
            HardwareState::NotApplied { reason } => (OutcomeSeverity::Refused, reason.clone()),
            HardwareState::Uncertain { reason } => (
                OutcomeSeverity::Unconfirmed,
                format!("The panel is uncertain: {reason}"),
            ),
        };
        Self {
            at_unix_ms: now_unix_ms(),
            message,
            severity,
            hardware: Some(outcome.hardware.clone()),
        }
    }

    fn from_lighting(outcome: &LightingOutcome) -> Self {
        let (severity, message) = match &outcome.hardware {
            HardwareState::Confirmed if outcome.deduplicated => (
                OutcomeSeverity::Confirmed,
                format!(
                    "Channel {} already shows this. Nothing was sent.",
                    outcome.channel
                ),
            ),
            HardwareState::Confirmed => (
                OutcomeSeverity::Confirmed,
                format!(
                    "Channel {} set to {}. The controller accepted the command; it reports no \
                     state to read back.",
                    outcome.channel,
                    outcome.program.summary()
                ),
            ),
            HardwareState::Onboard => (
                OutcomeSeverity::Confirmed,
                "The controller keeps its own program. Nothing was sent.".to_string(),
            ),
            HardwareState::NotApplied { reason } => (OutcomeSeverity::Refused, reason.clone()),
            HardwareState::Uncertain { reason } => (
                OutcomeSeverity::Unconfirmed,
                format!("Channel {} is uncertain: {reason}", outcome.channel),
            ),
        };

        Self {
            at_unix_ms: now_unix_ms(),
            message,
            severity,
            hardware: Some(outcome.hardware.clone()),
        }
    }

    fn from_apply(outcome: &ApplyOutcome) -> Self {
        let (severity, message) = match &outcome.hardware {
            HardwareState::Confirmed => (
                OutcomeSeverity::Confirmed,
                if outcome.deduplicated {
                    "Already applied. The hardware already held this program, so nothing was \
                     written."
                        .to_string()
                } else {
                    format!(
                        "Applied and confirmed by readback. {} kernel attributes written.",
                        outcome.writes
                    )
                },
            ),
            HardwareState::Onboard => (
                OutcomeSeverity::Confirmed,
                "The device keeps running its own program. Nothing was written.".to_string(),
            ),
            HardwareState::NotApplied { reason } => (OutcomeSeverity::Refused, reason.clone()),
            HardwareState::Uncertain { reason } => (OutcomeSeverity::Unconfirmed, reason.clone()),
        };

        Self {
            at_unix_ms: now_unix_ms(),
            message,
            severity,
            hardware: Some(outcome.hardware.clone()),
        }
    }
}

/// What the worker publishes each cycle.
#[derive(Debug, Clone, Default)]
struct Shared {
    link: Option<LinkState>,
    outcome: Option<CommandOutcome>,
}

/// A running worker.
pub struct Feed {
    shared: Arc<Mutex<Shared>>,
    commands: Sender<Command>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Feed {
    /// Start the worker and return it with the notifier the view awaits.
    pub fn spawn(socket: PathBuf, interval: Duration) -> (Self, UnboundedReceiver<()>) {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, inbox) = channel();
        let (notifier, notifications) = unbounded();

        let worker = std::thread::spawn({
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            move || run(socket, interval, shared, stop, inbox, notifier)
        });

        (
            Self {
                shared,
                commands,
                stop,
                worker: Some(worker),
            },
            notifications,
        )
    }

    /// The latest published state, or `None` before the first cycle.
    pub fn link(&self) -> Option<LinkState> {
        self.shared.lock().ok()?.link.clone()
    }

    /// The result of the most recent command.
    pub fn outcome(&self) -> Option<CommandOutcome> {
        self.shared.lock().ok()?.outcome.clone()
    }

    /// Queue a command. It runs on the worker, in order.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl std::fmt::Debug for Feed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feed")
            .field("connected", &self.link().is_some())
            .finish()
    }
}

/// Everything the worker carries between cycles.
struct Session {
    client: Client,
    capabilities: Arc<nzxt_core::capability::CapabilityRecord>,
    profiles: Arc<[Profile]>,
}

fn run(
    socket: PathBuf,
    interval: Duration,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    inbox: Receiver<Command>,
    notifier: UnboundedSender<()>,
) {
    let mut session: Option<Session> = None;
    // The command that ended the previous wait, held until the cycle it opens.
    let mut pending: Option<Command> = None;

    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();

        if session.is_none() {
            match connect(&socket) {
                Ok(fresh) => session = Some(fresh),
                Err(error) => publish_unavailable(&shared, &error),
            }
        }

        if let Some(active) = session.as_mut() {
            // Commands first: an Apply the operator just pressed must not wait
            // a whole cycle behind a poll.
            let mut refresh_profiles = false;
            // `try_recv` treats an empty queue and a dropped sender alike here:
            // both mean there is nothing more to run this cycle.
            while let Some(command) = pending.take().or_else(|| inbox.try_recv().ok()) {
                let (outcome, touched_profiles) = execute(active, command);
                refresh_profiles |= touched_profiles;
                if let Ok(mut shared) = shared.lock() {
                    shared.outcome = Some(outcome);
                }
            }

            if refresh_profiles && let Ok(profiles) = active.client.profiles() {
                active.profiles = profiles.1.into();
            }

            match poll(active) {
                Ok(link) => {
                    if let Ok(mut shared) = shared.lock() {
                        shared.link = Some(link);
                    }
                }
                Err(error) => {
                    publish_unavailable(&shared, &error);
                    // A broken connection is dropped rather than retried in
                    // place: the next cycle reconnects and re-reads the
                    // capability record, which may have changed.
                    session = None;
                }
            }
        }

        let _ = notifier.unbounded_send(());

        pending = wait_for_command(&inbox, &stop, interval.saturating_sub(started.elapsed()));
    }
}

/// Wait out the rest of a cycle, returning early with the command that arrives.
///
/// The waiting is what the operator feels. Sleeping through the remainder and
/// looking at the queue only at the next cycle boundary put a full polling
/// interval between pressing Apply and the request leaving the process: up to a
/// second, against the 15 ms the daemon needs to render a frame and the panel
/// needs to take it. Waiting *on the queue* costs nothing when it is empty and
/// returns immediately when it is not.
///
/// Still sliced, because the slice is also how a shutdown is noticed, and a
/// worker that waited a whole second on the queue would hold the window open
/// for that long on quit.
fn wait_for_command(
    inbox: &Receiver<Command>,
    stop: &AtomicBool,
    mut remaining: Duration,
) -> Option<Command> {
    while !remaining.is_zero() && !stop.load(Ordering::SeqCst) {
        let slice = remaining.min(SHUTDOWN_GRANULARITY);
        match inbox.recv_timeout(slice) {
            Ok(command) => return Some(command),
            Err(RecvTimeoutError::Timeout) => {}
            // A dropped sender means the window is going away. The stop flag is
            // what ends this loop, so a disconnected queue waits exactly as an
            // empty one does rather than spinning through the remainder.
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(slice),
        }
        remaining -= slice;
    }
    None
}

fn connect(socket: &std::path::Path) -> Result<Session, ClientError> {
    let mut client = Client::connect(socket)?;
    let capabilities = Arc::new(client.capabilities()?);
    let profiles: Arc<[Profile]> = client.profiles()?.1.into();
    Ok(Session {
        client,
        capabilities,
        profiles,
    })
}

/// One polling cycle: status and telemetry.
///
/// The capability record is not re-read every second. It changes on hotplug,
/// and a hotplug drops the connection, which is what triggers a fresh read.
fn poll(session: &mut Session) -> Result<LinkState, ClientError> {
    let status = session.client.status()?;
    let telemetry = session.client.telemetry()?;
    Ok(LinkState::Connected {
        status: Arc::new(status),
        capabilities: Arc::clone(&session.capabilities),
        profiles: Arc::clone(&session.profiles),
        telemetry: Some(Arc::new(telemetry)),
    })
}

fn publish_unavailable(shared: &Arc<Mutex<Shared>>, error: &ClientError) {
    if let Ok(mut shared) = shared.lock() {
        shared.link = Some(LinkState::Unavailable {
            message: error.operator_message(),
        });
    }
}

/// Run one command, returning its outcome and whether profiles changed.
fn execute(session: &mut Session, command: Command) -> (CommandOutcome, bool) {
    match command {
        Command::Apply(program) => match session.client.apply(program) {
            Ok(outcome) => (CommandOutcome::from_apply(&outcome), false),
            Err(error) => (CommandOutcome::refused(error.to_string()), false),
        },
        Command::ApplyLighting(command) => {
            let channel = command.channel;
            match session.client.apply_lighting(command) {
                Ok(outcome) => (CommandOutcome::from_lighting(&outcome), false),
                Err(error) => (
                    CommandOutcome::refused(format!("Channel {channel}: {error}")),
                    false,
                ),
            }
        }
        Command::ApplyDisplay(preset) => match session.client.apply_display(preset) {
            Ok(outcome) => (CommandOutcome::from_display(&outcome), false),
            Err(error) => (CommandOutcome::refused(error.to_string()), false),
        },
        Command::SaveProfile(profile) => {
            let name = profile.name.clone();
            match session.client.request(Request::SaveProfile { profile }) {
                Ok(Response::Saved { .. }) => (
                    CommandOutcome {
                        at_unix_ms: now_unix_ms(),
                        message: format!("Profile {name} saved."),
                        severity: OutcomeSeverity::Confirmed,
                        hardware: None,
                    },
                    true,
                ),
                Ok(Response::Error(error)) => (CommandOutcome::refused(error.to_string()), false),
                Ok(_) => (CommandOutcome::refused(unexpected()), false),
                Err(error) => (CommandOutcome::refused(error.to_string()), false),
            }
        }
        Command::ActivateProfile(name) => {
            match session.client.request(Request::ActivateProfile { name }) {
                Ok(Response::Activated(activation)) => {
                    let mut outcome = match &activation.applied {
                        Some(applied) => CommandOutcome::from_apply(applied),
                        None => CommandOutcome {
                            at_unix_ms: now_unix_ms(),
                            message: "Profile activated.".to_string(),
                            severity: OutcomeSeverity::Confirmed,
                            hardware: Some(activation.hardware.clone()),
                        },
                    };
                    outcome.message = format!("{}: {}", activation.name, outcome.message);
                    (outcome, true)
                }
                Ok(Response::Error(error)) => (CommandOutcome::refused(error.to_string()), false),
                Ok(_) => (CommandOutcome::refused(unexpected()), false),
                Err(error) => (CommandOutcome::refused(error.to_string()), false),
            }
        }
        Command::DeleteProfile(name) => {
            match session.client.request(Request::DeleteProfile { name }) {
                Ok(Response::Deleted {
                    name,
                    activated_instead,
                }) => {
                    let message = match activated_instead {
                        Some(safe) => {
                            format!("{safe} activated before {name} was deleted.")
                        }
                        None => format!("Profile {name} deleted."),
                    };
                    (
                        CommandOutcome {
                            at_unix_ms: now_unix_ms(),
                            message,
                            severity: OutcomeSeverity::Confirmed,
                            hardware: None,
                        },
                        true,
                    )
                }
                Ok(Response::Error(error)) => (CommandOutcome::refused(error.to_string()), false),
                Ok(_) => (CommandOutcome::refused(unexpected()), false),
                Err(error) => (CommandOutcome::refused(error.to_string()), false),
            }
        }
    }
}

fn unexpected() -> String {
    "The background service answered something this build does not understand.".to_string()
}

/// Milliseconds since the Unix epoch.
pub fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::ipc::{ChannelReadback, DisplayOutcome};
    use nzxt_core::lighting::Brightness;
    use nzxt_core::profile::Channel;

    #[test]
    fn a_command_ends_the_wait_instead_of_serving_out_the_interval() {
        // The defect this pins: the worker used to sleep through the gap
        // between polls and look at the queue only at the next boundary, which
        // put up to a whole interval between pressing Apply and the request
        // leaving the process.
        let (sender, inbox) = channel();
        let stop = AtomicBool::new(false);
        let queued = Command::ActivateProfile("Onboard safe".to_string());

        let started = Instant::now();
        std::thread::spawn({
            let queued = queued.clone();
            move || {
                std::thread::sleep(Duration::from_millis(30));
                let _ = sender.send(queued);
            }
        });

        assert_eq!(
            wait_for_command(&inbox, &stop, Duration::from_secs(5)),
            Some(queued)
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the command waited {:?} for a five second interval",
            started.elapsed()
        );
    }

    #[test]
    fn an_empty_queue_waits_the_interval_out_and_a_stop_cuts_it_short() {
        let (_sender, inbox) = channel::<Command>();

        let started = Instant::now();
        let waiting = AtomicBool::new(false);
        assert_eq!(
            wait_for_command(&inbox, &waiting, Duration::from_millis(60)),
            None
        );
        assert!(started.elapsed() >= Duration::from_millis(50));

        // A shutdown already requested means no waiting at all, which is what
        // keeps quitting the window prompt.
        let stopped = AtomicBool::new(true);
        let started = Instant::now();
        assert_eq!(
            wait_for_command(&inbox, &stopped, Duration::from_secs(5)),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn a_brightness_only_change_is_reported_as_applied_rather_than_as_nothing() {
        let preset = nzxt_core::display::DisplayPreset {
            brightness: Brightness::new(20).unwrap(),
            ..nzxt_core::display::DisplayPreset::default_infographic()
        };
        let dimmed = DisplayOutcome {
            preset,
            hardware: HardwareState::Confirmed,
            frames: 0,
            deduplicated: true,
            brightness_sent: true,
        };

        let reported = CommandOutcome::from_display(&dimmed);
        assert_eq!(reported.severity, OutcomeSeverity::Confirmed);
        assert!(reported.message.contains("20%"), "{}", reported.message);
        assert!(
            !reported.message.contains("Nothing was sent"),
            "a panel that was just dimmed was told nothing happened: {}",
            reported.message
        );

        // A command that changed neither the picture nor the setting still says
        // so, which is the case the message was written for.
        let untouched = DisplayOutcome {
            brightness_sent: false,
            ..dimmed
        };
        assert!(
            CommandOutcome::from_display(&untouched)
                .message
                .contains("Nothing was sent")
        );
    }

    fn outcome(hardware: HardwareState, writes: u32, deduplicated: bool) -> CommandOutcome {
        CommandOutcome::from_apply(&ApplyOutcome {
            hardware,
            writes,
            deduplicated,
            readback: vec![ChannelReadback::new(Channel::Pump)],
        })
    }

    #[test]
    fn a_confirmed_write_reports_how_many_attributes_it_touched() {
        let reported = outcome(HardwareState::Confirmed, 4, false);
        assert_eq!(reported.severity, OutcomeSeverity::Confirmed);
        assert!(reported.message.contains('4'), "{}", reported.message);
    }

    #[test]
    fn a_deduplicated_write_says_nothing_was_written() {
        let reported = outcome(HardwareState::Confirmed, 0, true);
        assert_eq!(reported.severity, OutcomeSeverity::Confirmed);
        assert!(
            reported.message.contains("nothing was written"),
            "{}",
            reported.message
        );
    }

    #[test]
    fn an_uncertain_state_is_neither_a_success_nor_a_plain_refusal() {
        let reported = outcome(
            HardwareState::Uncertain {
                reason: "pwm2 could not be read back".into(),
            },
            2,
            false,
        );
        assert_eq!(reported.severity, OutcomeSeverity::Unconfirmed);
        assert!(reported.message.contains("read back"));
        assert!(matches!(
            reported.hardware,
            Some(HardwareState::Uncertain { .. })
        ));
    }

    #[test]
    fn a_refusal_carries_the_reason_the_daemon_gave() {
        let reported = outcome(
            HardwareState::NotApplied {
                reason: "Pump duty 3 is outside the accepted range 51-255.".into(),
            },
            0,
            false,
        );
        assert_eq!(reported.severity, OutcomeSeverity::Refused);
        assert!(reported.message.contains("51-255"));
    }

    #[test]
    fn the_onboard_program_is_reported_as_untouched_hardware() {
        let reported = outcome(HardwareState::Onboard, 0, false);
        assert_eq!(reported.severity, OutcomeSeverity::Confirmed);
        assert!(reported.message.contains("Nothing was written"));
    }
}
