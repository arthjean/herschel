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
//! polls waiting *on the command queue* rather than sleeping through it, so a
//! write leaves the process when the screen queues it instead of at the next
//! boundary. That matters more now that no button holds an edit back: the only
//! delay an operator can feel is the quiet period the screen imposes on itself.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use kori_core::client::{Client, ClientError};
use kori_core::display::DisplayPreset;
use kori_core::ipc::{ApplyOutcome, HardwareState, LightingOutcome, Request, Response};
use kori_core::lighting::LightingCommand;
use kori_core::profile::{CoolingProgram, Profile};

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

impl Command {
    /// Whether running this can change the stored profile list.
    ///
    /// A property of what was asked rather than of how it ended. Reading the
    /// list back after a refused save costs one request nothing is waiting on,
    /// where threading the answer out of every execution path meant a positional
    /// flag on fourteen returns, most of them a constant `false`.
    fn touches_profiles(&self) -> bool {
        matches!(
            self,
            Self::SaveProfile(_) | Self::ActivateProfile(_) | Self::DeleteProfile(_)
        )
    }
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

/// How urgent a reported hardware state is.
///
/// One mapping for the three executors rather than one per executor. The three
/// used to repeat these five arms verbatim and only the sentence differed, which
/// meant a sixth [`HardwareState`] could be given three different severities by
/// being added to two of the three matches.
impl From<&HardwareState> for OutcomeSeverity {
    fn from(hardware: &HardwareState) -> Self {
        match hardware {
            // Onboard is a success: the device is running its own program
            // because that is what was asked for, and nothing was written
            // because nothing needed to be.
            HardwareState::Confirmed | HardwareState::Onboard => Self::Confirmed,
            HardwareState::Uncertain { .. } => Self::Unconfirmed,
            HardwareState::NotApplied { .. } => Self::Refused,
        }
    }
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
    /// An outcome the hardware had no part in: a stored profile, or a request
    /// that never reached a device.
    fn reported(severity: OutcomeSeverity, message: impl Into<String>) -> Self {
        Self {
            at_unix_ms: now_unix_ms(),
            message: message.into(),
            severity,
            hardware: None,
        }
    }

    fn refused(message: impl Into<String>) -> Self {
        Self::reported(OutcomeSeverity::Refused, message)
    }

    fn confirmed(message: impl Into<String>) -> Self {
        Self::reported(OutcomeSeverity::Confirmed, message)
    }

    /// One sentence about a state the hardware reported, at that state's own
    /// severity.
    ///
    /// The severity is never passed in: it is a property of the state, and
    /// [`OutcomeSeverity::from`] is the one place it is decided.
    fn from_hardware(hardware: &HardwareState, message: impl Into<String>) -> Self {
        Self {
            at_unix_ms: now_unix_ms(),
            message: message.into(),
            severity: OutcomeSeverity::from(hardware),
            hardware: Some(hardware.clone()),
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
    fn from_display(outcome: &kori_core::ipc::DisplayOutcome) -> Self {
        let message = match &outcome.hardware {
            HardwareState::Confirmed if outcome.deduplicated && outcome.brightness_sent => format!(
                "Panel brightness set to {}%. The picture was already correct, so no frame was \
                 sent.",
                outcome.preset.brightness.percent()
            ),
            HardwareState::Confirmed if outcome.deduplicated => {
                "The panel already shows this. Nothing was sent.".to_string()
            }
            HardwareState::Confirmed => format!(
                "Panel set to {}. The transfer completed; the panel reports no picture to read \
                 back.",
                outcome.preset.mode.label()
            ),
            HardwareState::Onboard => {
                "The panel keeps its own picture. Nothing was sent.".to_string()
            }
            HardwareState::NotApplied { reason } => reason.clone(),
            HardwareState::Uncertain { reason } => format!("The panel is uncertain: {reason}"),
        };
        Self::from_hardware(&outcome.hardware, message)
    }

    fn from_lighting(outcome: &LightingOutcome) -> Self {
        let message = match &outcome.hardware {
            HardwareState::Confirmed if outcome.deduplicated => format!(
                "Channel {} already shows this. Nothing was sent.",
                outcome.channel
            ),
            HardwareState::Confirmed => format!(
                "Channel {} set to {}. The controller accepted the command; it reports no state \
                 to read back.",
                outcome.channel,
                outcome.program.summary()
            ),
            HardwareState::Onboard => {
                "The controller keeps its own program. Nothing was sent.".to_string()
            }
            HardwareState::NotApplied { reason } => reason.clone(),
            HardwareState::Uncertain { reason } => {
                format!("Channel {} is uncertain: {reason}", outcome.channel)
            }
        };
        Self::from_hardware(&outcome.hardware, message)
    }

    fn from_apply(outcome: &ApplyOutcome) -> Self {
        let message = match &outcome.hardware {
            HardwareState::Confirmed if outcome.deduplicated => {
                "Already applied. The hardware already held this program, so nothing was written."
                    .to_string()
            }
            HardwareState::Confirmed => format!(
                "Applied and confirmed by readback. {} kernel attributes written.",
                outcome.writes
            ),
            HardwareState::Onboard => {
                "The device keeps running its own program. Nothing was written.".to_string()
            }
            HardwareState::NotApplied { reason } | HardwareState::Uncertain { reason } => {
                reason.clone()
            }
        };
        Self::from_hardware(&outcome.hardware, message)
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
    capabilities: Arc<kori_core::capability::CapabilityRecord>,
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
            // Commands first: a write the screen just queued must not wait
            // a whole cycle behind a poll.
            let mut refresh_profiles = false;
            // `try_recv` treats an empty queue and a dropped sender alike here:
            // both mean there is nothing more to run this cycle.
            while let Some(command) = pending.take().or_else(|| inbox.try_recv().ok()) {
                refresh_profiles |= command.touches_profiles();
                let outcome = execute(active, command);
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
/// interval between the screen queueing a write and the request leaving it: up to a
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

/// Run one command and report how it ended.
fn execute(session: &mut Session, command: Command) -> CommandOutcome {
    match command {
        Command::Apply(program) => match session.client.apply(program) {
            Ok(outcome) => CommandOutcome::from_apply(&outcome),
            Err(error) => CommandOutcome::refused(error.to_string()),
        },
        Command::ApplyLighting(command) => {
            let channel = command.channel;
            match session.client.apply_lighting(command) {
                Ok(outcome) => CommandOutcome::from_lighting(&outcome),
                Err(error) => CommandOutcome::refused(format!("Channel {channel}: {error}")),
            }
        }
        Command::ApplyDisplay(preset) => match session.client.apply_display(preset) {
            Ok(outcome) => CommandOutcome::from_display(&outcome),
            // Named, like a channel refusal is. The Lighting screen shows one
            // note for four rows that all write on their own, so a sentence
            // that does not say which device it is about would be read against
            // whichever row the operator is looking at.
            Err(error) => CommandOutcome::refused(format!("Panel: {error}")),
        },
        Command::SaveProfile(profile) => {
            let name = profile.name.clone();
            request(session, Request::SaveProfile { profile }, |response| {
                let Response::Saved { .. } = response else {
                    return None;
                };
                Some(CommandOutcome::confirmed(format!("Profile {name} saved.")))
            })
        }
        Command::ActivateProfile(name) => {
            request(session, Request::ActivateProfile { name }, |response| {
                let Response::Activated(activation) = response else {
                    return None;
                };
                let mut outcome = match &activation.applied {
                    Some(applied) => CommandOutcome::from_apply(applied),
                    None => CommandOutcome::from_hardware(
                        &activation.hardware,
                        "Profile activated.".to_string(),
                    ),
                };
                outcome.message = format!("{}: {}", activation.name, outcome.message);
                Some(outcome)
            })
        }
        Command::DeleteProfile(name) => {
            request(session, Request::DeleteProfile { name }, |response| {
                let Response::Deleted {
                    name,
                    activated_instead,
                } = response
                else {
                    return None;
                };
                Some(CommandOutcome::confirmed(match activated_instead {
                    Some(safe) => format!("{safe} activated before {name} was deleted."),
                    None => format!("Profile {name} deleted."),
                }))
            })
        }
    }
}

/// Send one request and read the answer it was sent for.
///
/// `describe` returns `None` for any response that is not the one this request
/// asks for, which is the only thing the three profile commands did differently.
/// They each repeated the refusal, the unexpected answer and the transport
/// failure verbatim, so a new failure mode had three places to be handled two
/// ways.
fn request(
    session: &mut Session,
    request: Request,
    describe: impl FnOnce(Response) -> Option<CommandOutcome>,
) -> CommandOutcome {
    match session.client.request(request) {
        Ok(Response::Error(error)) => CommandOutcome::refused(error.to_string()),
        Ok(response) => describe(response).unwrap_or_else(|| {
            CommandOutcome::refused(
                "The background service answered something this build does not understand.",
            )
        }),
        Err(error) => CommandOutcome::refused(error.to_string()),
    }
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
    use kori_core::ipc::{ChannelReadback, DisplayOutcome};
    use kori_core::lighting::Brightness;
    use kori_core::profile::Channel;

    #[test]
    fn a_command_ends_the_wait_instead_of_serving_out_the_interval() {
        // The defect this pins: the worker used to sleep through the gap
        // between polls and look at the queue only at the next boundary, which
        // put up to a whole interval between queueing a write and the request
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
        let preset = kori_core::display::DisplayPreset {
            brightness: Brightness::new(20).unwrap(),
            ..kori_core::display::DisplayPreset::default_infographic()
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
