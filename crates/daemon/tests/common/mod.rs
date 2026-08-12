// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Every test binary includes this module and uses the part of it its own subject
// needs, so an item no single binary reaches is expected rather than dead.
#![allow(dead_code)]

//! The fake machine every end-to-end test drives.
//!
//! One daemon, one real Unix socket, one sysfs tree under the temporary
//! directory. Every case starts from the entry point a GPUI client uses:
//! connect, handshake, send a typed request. Nothing reaches into the daemon's
//! internals to shortcut a path.
//!
//! Shared here rather than repeated per subject because the fixture is the same
//! machine whatever is being asserted about it, and a second copy of it would be
//! a second machine to keep in step with the first.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use kori_core::client::Client;
use kori_core::display::{DisplayMode, DisplayPreset};
use kori_core::ipc::{Request, Response};
use kori_core::lighting::{Brightness, LightingCommand, LightingProgram, Rgb};
use kori_core::profile::{CoolingProgram, Profile};
use kori_core::telemetry::TelemetrySnapshot;
use kori_daemon::server::ShutdownHandle;
use kori_daemon::state::Daemon;
use kori_daemon::{LcdBackend, Paths, RgbBackend, Server};
use kori_hardware_linux::SysfsRoot;
use kori_hardware_linux::testing::{FakeController, FakeKraken, FakeSysfs};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Sampling interval used by every test that is not measuring the real one.
pub const FAST_INTERVAL: Duration = Duration::from_millis(40);

/// How one fixture machine differs from the default one.
///
/// A described machine rather than a constructor per combination: the entry
/// points below used to be a telescope of positional arguments, where
/// `start_with(name, false)` said nothing about what `false` meant and every
/// caller of the paced variant restated the interval it was not changing.
/// Adding an axis multiplied the telescope again.
pub struct Machine {
    /// Whether the installed udev rule granting the PWM attributes is simulated.
    pub grant_write: bool,
    /// How often the collectors sample and the daemon's own clock ticks.
    pub interval: Duration,
    /// Where the daemon's lighting commands go.
    pub rgb: RgbBackend,
    /// Where the daemon's frames go.
    pub lcd: LcdBackend,
}

impl Default for Machine {
    /// A writable Kraken with no controller and no panel answering.
    ///
    /// Both devices default to absent because the fixture's sysfs tree resolves
    /// to the machine's real `/dev/hidraw*` and `/dev/bus/usb` nodes, and a test
    /// must never open either. A case that wants one says so.
    fn default() -> Self {
        Self {
            grant_write: true,
            interval: FAST_INTERVAL,
            rgb: RgbBackend::None,
            lcd: LcdBackend::None,
        }
    }
}

/// A daemon serving a fake machine on a real socket.
pub struct Harness {
    base: PathBuf,
    pub paths: Paths,
    pub fake: FakeSysfs,
    pub hwmon: PathBuf,
    pub proc_root: PathBuf,
    shutdown: ShutdownHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    /// The default machine: writable, with neither the controller nor the panel
    /// answering.
    pub fn start(name: &str) -> Self {
        Self::start_on(name, Machine::default(), |_, _| {})
    }

    /// A machine where the udev rule was never installed.
    ///
    /// This is how a daemon that cannot write anything comes up, which is the
    /// state it has to report rather than force.
    pub fn start_read_only(name: &str) -> Self {
        Self::start_on(
            name,
            Machine {
                grant_write: false,
                ..Machine::default()
            },
            |_, _| {},
        )
    }

    /// The default machine, with the fixture adjusted before the daemon starts.
    ///
    /// `prepare` runs before the first sample, so a test can present a stalled
    /// tachometer or a hot coolant from the very first one.
    pub fn start_prepared(name: &str, prepare: impl FnOnce(&FakeSysfs, &Path)) -> Self {
        Self::start_on(name, Machine::default(), prepare)
    }

    /// A daemon serving a controller that answers the reports the real one
    /// answers.
    pub fn start_lit(name: &str, firmware: &str, channels: usize) -> Self {
        Self::start_on(
            name,
            Machine {
                rgb: RgbBackend::Transport(Box::new(FakeController::new(firmware, channels))),
                ..Machine::default()
            },
            |_, _| {},
        )
    }

    /// A daemon serving a Kraken whose panel answers its display report.
    pub fn start_lcd(name: &str, firmware: &str) -> Self {
        Self::start_on(
            name,
            Machine {
                lcd: LcdBackend::Link(Box::new(FakeKraken::new(firmware).link())),
                ..Machine::default()
            },
            |_, _| {},
        )
    }

    /// Build the fixture, let `prepare` adjust it, then serve it.
    ///
    /// The one implementation. Everything above names a machine and hands it
    /// here, so there is a single place where a fixture is assembled and a
    /// single order in which a daemon comes up.
    pub fn start_on(name: &str, machine: Machine, prepare: impl FnOnce(&FakeSysfs, &Path)) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("kori-ipc-{name}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let fake = FakeSysfs::new(&format!("ipc-{name}-{unique}"));
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();
        fake.add_rgb_controller();
        fake.add_unrelated_device();
        fake.add_cpu_hwmon();
        let proc_root = fake.add_proc();
        if machine.grant_write {
            // Simulates an installed udev rule granting the two PWM nodes.
            for attribute in ["pwm1", "pwm2", "pwm1_enable", "pwm2_enable"] {
                fake.grant_write(&hwmon, attribute);
            }
            for channel in 1..=2 {
                for point in 1..=40 {
                    fake.grant_write(&hwmon, &format!("temp{channel}_auto_point{point}_pwm"));
                }
            }
        }
        prepare(&fake, &hwmon);

        let paths = Paths::new(base.join("run"), base.join("config"));
        let daemon = Daemon::start_with(
            paths.clone(),
            &SysfsRoot::new(fake.root_path()),
            &proc_root,
            machine.interval,
            machine.rgb,
            machine.lcd,
        )
        .unwrap();
        let listener = kori_daemon::server::bind_socket(&paths.socket).unwrap();
        // The daemon's own clock carries the reconciliation, not just the
        // panel redraw, so a test that waits on the daemon noticing something
        // waits on this. At the shipped one second per pass every such test
        // would spend a real second doing nothing.
        let server = Server::attach(listener, daemon).with_frame_interval(machine.interval);
        let shutdown = server.shutdown_handle();
        let thread = std::thread::spawn(move || server.run());

        // The socket exists before bind returns, so a connect cannot race it.
        Self {
            base,
            paths,
            fake,
            hwmon,
            proc_root,
            shutdown,
            thread: Some(thread),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.paths.socket
    }

    pub fn client(&self) -> Client {
        Client::connect(self.socket()).expect("client connects and handshakes")
    }

    /// A raw connection that has not completed the handshake.
    pub fn raw(&self) -> UnixStream {
        UnixStream::connect(self.socket()).expect("raw connection")
    }

    pub fn hwmon_path(&self) -> PathBuf {
        std::fs::canonicalize(self.fake.root_path().join("class/hwmon/hwmon4")).unwrap()
    }

    /// Poll telemetry until `condition` holds, or give up.
    pub fn wait_for_telemetry(
        &self,
        client: &mut Client,
        limit: Duration,
        mut condition: impl FnMut(&TelemetrySnapshot) -> bool,
    ) -> TelemetrySnapshot {
        let deadline = std::time::Instant::now() + limit;
        loop {
            let snapshot = client.telemetry().expect("telemetry is served");
            if condition(&snapshot) || std::time::Instant::now() >= deadline {
                return snapshot;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Poll status until `condition` holds, or give up.
    ///
    /// What the daemon settles on its own clock is only visible through status,
    /// and it happens whether or not anything is asking. This is how a test
    /// waits for it without pretending a client request caused it.
    pub fn wait_for_status(
        &self,
        client: &mut Client,
        limit: Duration,
        mut condition: impl FnMut(&kori_core::ipc::DaemonStatus) -> bool,
    ) -> kori_core::ipc::DaemonStatus {
        let deadline = std::time::Instant::now() + limit;
        loop {
            let status = client.status().expect("status is served");
            if condition(&status) || std::time::Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Modification times of every file under `root`, to prove nothing was written.
pub fn snapshot(root: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut entries = Vec::new();
    for child in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let Ok(metadata) = child.metadata() else {
            continue;
        };
        if metadata.is_file() {
            entries.push((
                child.path(),
                metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                metadata.len(),
            ));
        }
    }
    entries.sort();
    entries
}

/// A profile running one fixed duty per channel.
pub fn fixed(name: &str, pump: u8, fan: u8) -> Profile {
    Profile {
        name: name.to_string(),
        program: CoolingProgram::Fixed { pump, fan },
        device: None,
        lighting: Vec::new(),
        display: None,
    }
}

pub fn read_attribute(hwmon: &Path, attribute: &str) -> String {
    std::fs::read_to_string(hwmon.join(attribute))
        .unwrap_or_else(|error| panic!("{attribute}: {error}"))
        .trim()
        .to_string()
}

/// Send an Apply and return the outcome the daemon reported.
pub fn apply(client: &mut Client, program: CoolingProgram) -> kori_core::ipc::ApplyOutcome {
    match client.request(Request::ApplyProgram { program }).unwrap() {
        Response::Applied(outcome) => *outcome,
        other => panic!("expected an apply outcome, got {other:?}"),
    }
}

/// One channel told to hold one color.
pub fn lighting(channel: u8, hex: &str) -> LightingCommand {
    LightingCommand {
        channel,
        program: LightingProgram::Fixed {
            color: Rgb::parse_hex(hex).unwrap(),
            brightness: Brightness::new(60).unwrap(),
        },
    }
}

/// Milliseconds since the Unix epoch, for age assertions.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// A preset the panel tests drive.
pub fn preset(mode: DisplayMode) -> DisplayPreset {
    DisplayPreset {
        mode,
        ..DisplayPreset::default_infographic()
    }
}
