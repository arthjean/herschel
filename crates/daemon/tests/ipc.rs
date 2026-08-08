// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end tests over a real Unix socket.
//!
//! Every case starts from the entry point a GPUI client uses: connect to the
//! socket, handshake, send a typed request. Nothing reaches into the daemon's
//! internals to shortcut a path.

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use nzxt_core::capability::CapabilityId;
use nzxt_core::client::Client;
use nzxt_core::display::{DisplayMode, DisplayPreset};
use nzxt_core::ipc::{
    AccessMode, ConfigState, HardwareState, IpcError, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request,
    Response, read_frame, write_frame,
};
use nzxt_core::lighting::{Brightness, LightingCommand, LightingProgram, Rgb};
use nzxt_core::profile::{
    CURVE_POINT_COUNT, Channel, CoolingProgram, CurveNodes, MIN_PUMP_DUTY, Profile,
    SAFE_PROFILE_NAME, TemperatureCurve,
};
use nzxt_core::telemetry::{Collector, PwmMode, SafetyAlert, TelemetrySnapshot};
use nzxt_core::{KRAKEN_BASE, RGB_CONTROLLER};
use nzxt_daemon::server::ShutdownHandle;
use nzxt_daemon::state::{Daemon, LcdBackend, RgbBackend};
use nzxt_daemon::{Paths, Server};
use nzxt_hardware_linux::SysfsRoot;
use nzxt_hardware_linux::testing::{FakeController, FakeKraken, FakeSysfs};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Sampling interval used by every test that is not measuring the real one.
const FAST_INTERVAL: Duration = Duration::from_millis(40);

/// A daemon serving a fake machine on a real socket.
struct Harness {
    base: PathBuf,
    paths: Paths,
    fake: FakeSysfs,
    hwmon: PathBuf,
    proc_root: PathBuf,
    shutdown: ShutdownHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    fn start(name: &str) -> Self {
        Self::start_with(name, true)
    }

    fn start_with(name: &str, grant_write: bool) -> Self {
        Self::start_paced(name, grant_write, FAST_INTERVAL, |_, _| {})
    }

    /// A daemon serving a controller that answers the reports the real one
    /// answers.
    ///
    /// Every other case runs with `RgbBackend::None`: the fixture's sysfs tree
    /// resolves to the machine's real `/dev/hidraw*` node, and a test must
    /// never open it.
    fn start_lit(name: &str, firmware: &str, channels: usize) -> Self {
        Self::start_full(
            name,
            true,
            FAST_INTERVAL,
            |_, _| {},
            RgbBackend::Transport(Box::new(FakeController::new(firmware, channels))),
            LcdBackend::None,
        )
    }

    /// A daemon serving a Kraken whose panel answers its display report.
    ///
    /// Every other case runs with `LcdBackend::None`, for the same reason the
    /// lighting cases do: the fixture's sysfs tree resolves to the machine's
    /// real `/dev/hidraw*` and `/dev/bus/usb` nodes, and a test must never
    /// open either.
    fn start_lcd(name: &str, firmware: &str) -> Self {
        Self::start_full(
            name,
            true,
            FAST_INTERVAL,
            |_, _| {},
            RgbBackend::None,
            LcdBackend::Link(Box::new(FakeKraken::new(firmware).link())),
        )
    }

    /// Build the fixture, let `prepare` adjust it, then serve it.
    ///
    /// `prepare` runs before the daemon starts, so a test can present a
    /// stalled tachometer or a hot coolant from the very first sample.
    fn start_paced(
        name: &str,
        grant_write: bool,
        interval: Duration,
        prepare: impl FnOnce(&FakeSysfs, &Path),
    ) -> Self {
        Self::start_full(
            name,
            grant_write,
            interval,
            prepare,
            RgbBackend::None,
            LcdBackend::None,
        )
    }

    fn start_full(
        name: &str,
        grant_write: bool,
        interval: Duration,
        prepare: impl FnOnce(&FakeSysfs, &Path),
        rgb: RgbBackend,
        lcd: LcdBackend,
    ) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("nzxt-ipc-{name}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let fake = FakeSysfs::new(&format!("ipc-{name}-{unique}"));
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();
        fake.add_rgb_controller();
        fake.add_unrelated_device();
        fake.add_cpu_hwmon();
        let proc_root = fake.add_proc();
        if grant_write {
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
            interval,
            rgb,
            lcd,
        )
        .unwrap();
        let listener = nzxt_daemon::server::bind_socket(&paths.socket).unwrap();
        let server = Server::attach(listener, daemon);
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

    fn socket(&self) -> &Path {
        &self.paths.socket
    }

    fn client(&self) -> Client {
        Client::connect(self.socket()).expect("client connects and handshakes")
    }

    /// A raw connection that has not completed the handshake.
    fn raw(&self) -> UnixStream {
        UnixStream::connect(self.socket()).expect("raw connection")
    }

    fn hwmon_path(&self) -> PathBuf {
        std::fs::canonicalize(self.fake.root_path().join("class/hwmon/hwmon4")).unwrap()
    }

    /// Poll telemetry until `condition` holds, or give up.
    fn wait_for_telemetry(
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
fn snapshot(root: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(children) = std::fs::read_dir(&current) else {
            continue;
        };
        for child in children.flatten() {
            let Ok(file_type) = child.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(child.path());
            } else if file_type.is_file() {
                let metadata = child.metadata().unwrap();
                entries.push((
                    child.path(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    metadata.len(),
                ));
            }
        }
    }
    entries.sort();
    entries
}

fn fixed(name: &str, pump: u8, fan: u8) -> Profile {
    Profile {
        name: name.to_string(),
        program: CoolingProgram::Fixed { pump, fan },
        device: None,
        lighting: Vec::new(),
        display: None,
    }
}

/// Read one hwmon attribute the driver publishes readable.
fn read_attribute(hwmon: &Path, attribute: &str) -> String {
    std::fs::read_to_string(hwmon.join(attribute))
        .unwrap_or_else(|error| panic!("{attribute}: {error}"))
        .trim()
        .to_string()
}

/// Send an Apply and return the outcome the daemon reported.
fn apply(client: &mut Client, program: CoolingProgram) -> nzxt_core::ipc::ApplyOutcome {
    match client.request(Request::ApplyProgram { program }).unwrap() {
        Response::Applied(outcome) => *outcome,
        other => panic!("expected an apply outcome, got {other:?}"),
    }
}

#[test]
fn a_client_completes_the_handshake_and_reads_status() {
    let harness = Harness::start("handshake");
    let mut client = harness.client();

    assert_eq!(client.daemon_version(), nzxt_daemon::DAEMON_VERSION);

    let status = client.status().unwrap();
    assert_eq!(status.protocol_version, PROTOCOL_VERSION);
    assert_eq!(status.devices.len(), 2);
    assert!(status.devices.iter().all(|device| device.owned));
    assert_eq!(status.active_profile, SAFE_PROFILE_NAME);
    assert_eq!(status.socket_path, harness.socket().display().to_string());
    assert_eq!(status.config, ConfigState::Defaults);
}

#[test]
fn the_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let harness = Harness::start("socket-mode");
    let mode = std::fs::metadata(harness.socket())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn one_lock_is_held_per_supported_device() {
    let harness = Harness::start("locks");
    for device in [KRAKEN_BASE, RGB_CONTROLLER] {
        let lock = harness.paths.device_lock(device);
        assert!(lock.exists(), "{lock:?} must exist");
    }

    // A second daemon over the same runtime directory cannot take the devices.
    let second = Daemon::start_with(
        harness.paths.clone(),
        &SysfsRoot::new(harness.fake.root_path()),
        &harness.proc_root,
        FAST_INTERVAL,
        RgbBackend::None,
        LcdBackend::None,
    )
    .unwrap();
    assert!(second.locked_devices().is_empty());

    let AccessMode::ReadOnly { conflicts } = second.access_mode() else {
        panic!("a second daemon must be read-only");
    };
    assert!(
        conflicts
            .iter()
            .any(|conflict| conflict.detail.contains("device lock"))
    );
}

#[test]
fn capabilities_reach_the_client_with_their_evidence() {
    let harness = Harness::start("capabilities");
    let mut client = harness.client();

    let record = client.capabilities().unwrap();
    let kraken = record.device(KRAKEN_BASE).unwrap();
    assert_eq!(
        kraken.usb.product.value().map(String::as_str),
        Some("NZXT Kraken Base")
    );
    assert!(kraken.can_write(CapabilityId::PumpDuty));
    assert!(!kraken.can_write(CapabilityId::LcdFrame));

    let rgb = record.device(RGB_CONTROLLER).unwrap();
    assert!(!rgb.can_write(CapabilityId::RgbFixedColor));
    assert_eq!(record.rejected.len(), 1);
}

#[test]
fn a_request_before_the_handshake_is_refused() {
    let harness = Harness::start("handshake-required");
    let stream = harness.raw();
    let mut writer = BufWriter::new(stream.try_clone().unwrap());
    let mut reader = BufReader::new(stream);

    write_frame(&mut writer, &Request::Status).unwrap();
    let response: Response = read_frame(&mut reader).unwrap();
    assert_eq!(response, Response::Error(IpcError::HandshakeRequired));
}

#[test]
fn a_mismatched_protocol_version_is_refused() {
    let harness = Harness::start("protocol");
    let stream = harness.raw();
    let mut writer = BufWriter::new(stream.try_clone().unwrap());
    let mut reader = BufReader::new(stream);

    write_frame(
        &mut writer,
        &Request::Hello {
            protocol_version: PROTOCOL_VERSION + 99,
        },
    )
    .unwrap();
    let response: Response = read_frame(&mut reader).unwrap();
    match response {
        Response::Error(IpcError::UnsupportedProtocol {
            requested,
            supported,
        }) => {
            assert_eq!(requested, PROTOCOL_VERSION + 99);
            assert_eq!(supported, PROTOCOL_VERSION);
        }
        other => panic!("expected UnsupportedProtocol, got {other:?}"),
    }
}

#[test]
fn malformed_and_oversized_frames_are_refused_without_touching_hardware() {
    let harness = Harness::start("malformed");
    let before = snapshot(&harness.hwmon_path());

    for payload in [
        b"not json at all\n".to_vec(),
        b"{\"request\":\"format_disk\"}\n".to_vec(),
        b"{\"request\":\"activate_profile\"}\n".to_vec(),
        b"[]\n".to_vec(),
        b"null\n".to_vec(),
    ] {
        let stream = harness.raw();
        let mut writer = BufWriter::new(stream.try_clone().unwrap());
        let mut reader = BufReader::new(stream);
        writer.write_all(&payload).unwrap();
        writer.flush().unwrap();

        let response: Response = read_frame(&mut reader).unwrap();
        match response {
            Response::Error(IpcError::Malformed { .. }) => {}
            other => panic!("expected Malformed for {payload:?}, got {other:?}"),
        }
    }

    // A frame past the ceiling is rejected as such.
    let stream = harness.raw();
    let mut writer = BufWriter::new(stream.try_clone().unwrap());
    let mut reader = BufReader::new(stream);
    writer
        .write_all(&vec![b'x'; MAX_FRAME_BYTES + 1024])
        .unwrap();
    writer.flush().unwrap();
    let response: Response = read_frame(&mut reader).unwrap();
    match response {
        Response::Error(IpcError::FrameTooLarge { max_bytes }) => {
            assert_eq!(max_bytes, MAX_FRAME_BYTES)
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }

    assert_eq!(before, snapshot(&harness.hwmon_path()), "hwmon was written");
}

#[test]
fn out_of_range_values_are_rejected_with_their_accepted_range() {
    let harness = Harness::start("out-of-range");
    let before = snapshot(&harness.hwmon_path());
    let mut client = harness.client();

    let response = client
        .request(Request::SaveProfile {
            profile: fixed("Stalled pump", 3, 90),
        })
        .unwrap();
    match response {
        Response::Error(IpcError::Validation(error)) => {
            let message = error.to_string();
            assert!(message.contains("51-255"), "{message}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }

    let mut curve = TemperatureCurve::flat(200);
    curve.points[30] = 100;
    let response = client
        .request(Request::SaveProfile {
            profile: Profile {
                name: "Falling".into(),
                program: CoolingProgram::Curve {
                    pump: curve.clone(),
                    fan: curve,
                },
                device: None,
                lighting: Vec::new(),
                display: None,
            },
        })
        .unwrap();
    assert!(matches!(response, Response::Error(IpcError::Validation(_))));

    let Response::Profiles { profiles, .. } = client.request(Request::Profiles).unwrap() else {
        panic!("expected profiles");
    };
    assert_eq!(profiles.len(), 1, "only the safe profile should exist");
    assert_eq!(before, snapshot(&harness.hwmon_path()), "hwmon was written");
}

#[test]
fn profiles_are_saved_activated_and_deleted_through_the_socket() {
    let harness = Harness::start("profiles");
    let mut client = harness.client();

    assert_eq!(
        client
            .request(Request::SaveProfile {
                profile: fixed("Silent", 120, 80)
            })
            .unwrap(),
        Response::Saved {
            name: "Silent".into()
        }
    );

    let Response::Profiles { active, profiles } = client.request(Request::Profiles).unwrap() else {
        panic!("expected profiles");
    };
    assert_eq!(active, SAFE_PROFILE_NAME);
    assert_eq!(profiles.len(), 2);

    let Response::Activated(outcome) = client
        .request(Request::ActivateProfile {
            name: "Silent".into(),
        })
        .unwrap()
    else {
        panic!("expected activation");
    };
    assert_eq!(outcome.name, "Silent");
    assert_eq!(outcome.hardware, HardwareState::Confirmed);

    // Deleting the active profile activates the safe one first.
    let Response::Deleted {
        activated_instead, ..
    } = client
        .request(Request::DeleteProfile {
            name: "Silent".into(),
        })
        .unwrap()
    else {
        panic!("expected deletion");
    };
    assert_eq!(activated_instead.as_deref(), Some(SAFE_PROFILE_NAME));

    assert_eq!(
        client
            .request(Request::ActivateProfile {
                name: "Silent".into()
            })
            .unwrap(),
        Response::Error(IpcError::ProfileNotFound {
            name: "Silent".into()
        })
    );
}

#[test]
fn the_safe_profile_activates_without_writing_anything() {
    let harness = Harness::start("safe-profile");
    let before = snapshot(&harness.hwmon_path());
    let mut client = harness.client();

    let Response::Activated(outcome) = client
        .request(Request::ActivateProfile {
            name: SAFE_PROFILE_NAME.into(),
        })
        .unwrap()
    else {
        panic!("expected activation");
    };
    assert_eq!(outcome.hardware, HardwareState::Onboard);
    assert_eq!(before, snapshot(&harness.hwmon_path()));
}

#[test]
fn the_active_profile_survives_a_daemon_restart() {
    let harness = Harness::start("restart");
    {
        let mut client = harness.client();
        client
            .request(Request::SaveProfile {
                profile: fixed("Silent", 120, 80),
            })
            .unwrap();
        client
            .request(Request::ActivateProfile {
                name: "Silent".into(),
            })
            .unwrap();
    }

    // A fresh daemon over the same configuration directory, as after a reboot.
    let restarted = Daemon::start_with(
        Paths::new(
            harness.paths.runtime_dir.join("second"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
        &harness.proc_root,
        FAST_INTERVAL,
        RgbBackend::None,
        LcdBackend::None,
    )
    .unwrap();
    assert_eq!(restarted.status().active_profile, "Silent");
    assert_eq!(restarted.status().config, ConfigState::Loaded);

    // The profile is put back on the hardware, not merely reselected: the
    // restart is what US-011 requires to restore the program itself.
    let hwmon = harness.hwmon_path();
    assert_eq!(read_attribute(&hwmon, "pwm1"), "120");
    assert_eq!(read_attribute(&hwmon, "pwm2"), "80");
    assert_eq!(
        read_attribute(&hwmon, "pwm1_enable"),
        PwmMode::Fixed.to_kernel().to_string()
    );
}

#[test]
fn a_corrupt_configuration_recovers_to_the_safe_profile() {
    let harness = Harness::start("corrupt");
    let config_file = harness.paths.config_file();
    std::fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    std::fs::write(&config_file, "schema_version = 1\nactive_pro").unwrap();

    let daemon = Daemon::start_with(
        Paths::new(
            harness.paths.runtime_dir.join("recovery"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
        &harness.proc_root,
        FAST_INTERVAL,
        RgbBackend::None,
        LcdBackend::None,
    )
    .unwrap();

    let status = daemon.status();
    assert_eq!(status.active_profile, SAFE_PROFILE_NAME);
    let ConfigState::Recovered { preserved_path, .. } = &status.config else {
        panic!("expected recovery, got {:?}", status.config);
    };
    assert!(Path::new(preserved_path).exists());
    assert!(
        status
            .config
            .recovery_message()
            .unwrap()
            .contains("Safe defaults are active")
    );
}

#[test]
fn a_profile_needing_an_unwritable_capability_is_refused() {
    // No udev rule: every control attribute stays read-only.
    let harness = Harness::start_with("read-only", false);
    let before = snapshot(&harness.hwmon_path());
    let mut client = harness.client();

    let status = client.status().unwrap();
    assert!(status.access.is_read_only());

    client
        .request(Request::SaveProfile {
            profile: fixed("Silent", 120, 80),
        })
        .unwrap();

    let response = client
        .request(Request::ActivateProfile {
            name: "Silent".into(),
        })
        .unwrap();
    match response {
        Response::Error(IpcError::Incompatible { details }) => {
            assert!(
                details
                    .iter()
                    .any(|detail| detail.capability == CapabilityId::PumpDuty)
            );
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }

    // The refusal did not change the active profile or the hardware.
    assert_eq!(client.status().unwrap().active_profile, SAFE_PROFILE_NAME);
    assert_eq!(before, snapshot(&harness.hwmon_path()));
}

#[test]
fn a_profile_bound_to_an_absent_device_is_refused() {
    let harness = Harness::start("wrong-device");
    let mut client = harness.client();

    let profile = Profile {
        name: "Other machine".into(),
        program: CoolingProgram::Fixed { pump: 120, fan: 80 },
        device: Some(nzxt_core::DeviceId::new(0x1e71, 0x2007)),
        lighting: Vec::new(),
        display: None,
    };
    client
        .request(Request::SaveProfile {
            profile: profile.clone(),
        })
        .unwrap();

    assert_eq!(
        client
            .request(Request::ActivateProfile {
                name: profile.name.clone()
            })
            .unwrap(),
        Response::Error(IpcError::NoDevice)
    );
}

#[test]
fn diagnostics_are_exported_without_serial_numbers() {
    let harness = Harness::start("diagnostics");
    let mut client = harness.client();
    client
        .request(Request::SaveProfile {
            profile: fixed("Silent", 120, 80),
        })
        .unwrap();

    let Response::Diagnostics(export) = client.request(Request::Diagnostics).unwrap() else {
        panic!("expected diagnostics");
    };

    let json = serde_json::to_string(&export).unwrap();
    assert!(
        !json.contains(nzxt_hardware_linux::testing::KRAKEN_FIXTURE_SERIAL),
        "kraken serial leaked"
    );
    assert!(
        !json.contains(nzxt_hardware_linux::testing::RGB_FIXTURE_SERIAL),
        "rgb serial leaked"
    );
    assert!(json.contains("device_discovered"), "{json}");
    assert!(json.contains("ownership_acquired"), "{json}");
    assert!(json.contains("profile_saved"), "{json}");
    assert!(export.events.iter().all(|event| event.at_unix_ms > 0));
}

#[test]
fn several_clients_are_served_without_interleaving() {
    let harness = Harness::start("concurrent");
    let socket = harness.socket().to_path_buf();

    let workers: Vec<_> = (0..4)
        .map(|worker| {
            let socket = socket.clone();
            std::thread::spawn(move || {
                let mut client = Client::connect(&socket).unwrap();
                for round in 0..10 {
                    let name = format!("worker-{worker}-{round}");
                    let response = client
                        .request(Request::SaveProfile {
                            profile: fixed(&name, 120, 80),
                        })
                        .unwrap();
                    assert_eq!(response, Response::Saved { name });
                }
            })
        })
        .collect();

    for worker in workers {
        worker.join().unwrap();
    }

    let mut client = harness.client();
    let Response::Profiles { profiles, .. } = client.request(Request::Profiles).unwrap() else {
        panic!("expected profiles");
    };
    // 40 saved profiles plus the built-in safe one, none lost to a race.
    assert_eq!(profiles.len(), 41);
}

#[test]
fn the_daemon_survives_a_client_that_disappears_mid_request() {
    let harness = Harness::start("client-crash");

    {
        let stream = harness.raw();
        let mut writer = BufWriter::new(stream.try_clone().unwrap());
        write_frame(
            &mut writer,
            &Request::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        // Half a frame, then the socket drops.
        writer.write_all(b"{\"request\":\"sta").unwrap();
        writer.flush().unwrap();
        drop(writer);
        drop(stream);
    }

    std::thread::sleep(Duration::from_millis(50));

    // The daemon is still serving, and still owns both devices.
    let mut client = harness.client();
    let status = client.status().unwrap();
    assert_eq!(status.devices.len(), 2);
    assert!(status.devices.iter().all(|device| device.owned));
}

#[test]
fn read_only_mode_names_the_conflict() {
    let harness = Harness::start_with("conflict-detail", false);
    let mut client = harness.client();

    let AccessMode::ReadOnly { conflicts } = client.status().unwrap().access else {
        panic!("expected read-only");
    };
    assert!(!conflicts.is_empty());
    assert!(
        conflicts.iter().any(|c| c.detail.contains("udev")),
        "{conflicts:?}"
    );
}

#[test]
fn every_blocked_capability_carries_an_operator_reason() {
    let harness = Harness::start("blocked-reasons");
    let mut client = harness.client();
    let status = client.status().unwrap();

    let rgb = status
        .devices
        .iter()
        .find(|device| device.id == RGB_CONTROLLER)
        .unwrap();
    assert!(rgb.writable.is_empty());
    assert!(
        rgb.blocked
            .iter()
            .any(|blocked| blocked.reason.contains("US-013"))
    );

    let kraken = status
        .devices
        .iter()
        .find(|device| device.id == KRAKEN_BASE)
        .unwrap();
    assert!(
        kraken
            .blocked
            .iter()
            .any(|blocked| blocked.reason.contains("US-016"))
    );
    assert!(
        kraken
            .blocked
            .iter()
            .all(|blocked| !blocked.reason.is_empty())
    );
}

// --- EP-002: Monitoring and Thermal Control ------------------------------

#[test]
fn telemetry_reaches_the_client_with_every_section_sampled() {
    let harness = Harness::start("telemetry");
    let mut client = harness.client();

    let snapshot = harness.wait_for_telemetry(&mut client, Duration::from_secs(3), |snapshot| {
        snapshot.kraken.liquid_temperature_c.is_valid() && snapshot.system.memory.is_valid()
    });

    // The Kraken section, straight off the bound kraken2023 instance.
    assert!(snapshot.kraken.present);
    assert_eq!(snapshot.kraken.liquid_temperature_c.copied(), Some(27.9));
    assert_eq!(snapshot.kraken.pump.rpm.copied(), Some(2_970));
    assert_eq!(snapshot.kraken.fan.rpm.copied(), Some(1_764));
    assert_eq!(snapshot.kraken.pump.duty.copied(), Some(255));
    assert_eq!(snapshot.kraken.fan.duty.copied(), Some(255));
    assert_eq!(
        snapshot.kraken.pump.mode.copied(),
        Some(PwmMode::FullSpeed),
        "the fixture starts on the firmware failsafe"
    );

    // The system section, from /proc and the CPU hwmon instance.
    let memory = snapshot.system.memory.copied().unwrap();
    assert_eq!(memory.total_bytes, 31_979_068 * 1024);
    assert!(memory.percent() > 0.0 && memory.percent() < 100.0);
    assert_eq!(snapshot.system.cpu_temperature_c.copied(), Some(46.75));

    assert_eq!(snapshot.interval_ms, FAST_INTERVAL.as_millis() as u64);
    assert!(snapshot.sequence > 0);
}

#[test]
fn an_unreadable_channel_is_unavailable_with_its_cause_rather_than_zero() {
    let harness = Harness::start_paced("telemetry-missing", true, FAST_INTERVAL, |fake, hwmon| {
        // The fan tachometer disappears, as it would on a firmware that does
        // not publish it.
        fake.remove_attribute(hwmon, "fan2_input");
    });
    let mut client = harness.client();

    let snapshot = harness.wait_for_telemetry(&mut client, Duration::from_secs(3), |snapshot| {
        snapshot.kraken.pump.rpm.is_valid()
    });

    assert!(
        snapshot.kraken.pump.rpm.is_valid(),
        "the pump still reports"
    );
    assert!(!snapshot.kraken.fan.rpm.is_valid());
    let cause = snapshot.kraken.fan.rpm.cause().unwrap();
    assert!(cause.detail().contains("fan2_input"), "{cause}");
    assert_eq!(
        snapshot.kraken.fan.rpm.copied(),
        None,
        "an unreadable channel must never present as zero"
    );
}

#[test]
fn sampling_performs_zero_writes_to_hwmon() {
    let harness = Harness::start("telemetry-read-only");
    let hwmon = harness.hwmon_path();
    let before = snapshot(&hwmon);
    let mut client = harness.client();

    // Several complete passes at the fixture's cadence.
    harness.wait_for_telemetry(&mut client, Duration::from_secs(3), |snapshot| {
        snapshot.sequence > 12
    });

    assert_eq!(before, snapshot(&hwmon), "telemetry wrote to hwmon");
}

#[test]
fn a_sample_reaches_the_client_inside_the_freshness_budget() {
    // The production cadence, because this is the criterion being measured.
    let harness = Harness::start_paced(
        "telemetry-age",
        true,
        Duration::from_millis(nzxt_core::telemetry::SAMPLE_INTERVAL_MS),
        |_, _| {},
    );
    let mut client = harness.client();

    let mut ages = Vec::new();
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(
            nzxt_core::telemetry::SAMPLE_INTERVAL_MS,
        ));
        let snapshot = client.telemetry().unwrap();
        if !snapshot.kraken.liquid_temperature_c.is_valid() {
            continue; // The first pass may not have completed yet.
        }
        // Age of the reading itself, not of the response: this is the figure
        // the freshness budget is written against.
        let age = now_unix_ms().saturating_sub(snapshot.kraken.at_unix_ms);
        assert!(
            age <= 1_500,
            "sample reached the client {age} ms old, past the 1500 ms budget"
        );
        ages.push(age);
    }

    assert!(
        ages.len() >= 4,
        "expected several samples, got {}",
        ages.len()
    );
    let worst = ages.iter().max().copied().unwrap_or_default();
    assert!(worst <= 1_500, "worst observed age was {worst} ms");
}

#[test]
fn an_unavailable_gpu_leaves_every_other_metric_updating() {
    let harness = Harness::start("telemetry-gpu");
    let mut client = harness.client();

    let snapshot = harness.wait_for_telemetry(&mut client, Duration::from_secs(3), |snapshot| {
        snapshot.system.memory.is_valid()
    });

    // Whether this machine has NVML or not, the GPU section is independent:
    // either it carries values, or it carries a typed cause, and the CPU and
    // memory readings are unaffected either way.
    if snapshot.gpu.load_percent.is_valid() {
        let load = snapshot.gpu.load_percent.copied().unwrap();
        assert!((0.0..=100.0).contains(&load), "load {load}");
    } else {
        assert!(
            !snapshot
                .gpu
                .load_percent
                .cause()
                .unwrap()
                .detail()
                .is_empty()
        );
    }
    assert!(snapshot.system.cpu_temperature_c.is_valid());
    assert!(snapshot.system.memory.is_valid());
    assert!(
        snapshot.failure(Collector::Cpu).is_none(),
        "the CPU collector must not be dragged down by the GPU"
    );
}

#[test]
fn a_fixed_duty_is_written_once_and_reported_with_its_readback() {
    let harness = Harness::start("apply-fixed");
    let hwmon = harness.hwmon_path();
    let mut client = harness.client();

    let outcome = apply(&mut client, CoolingProgram::Fixed { pump: 180, fan: 90 });
    assert_eq!(outcome.hardware, HardwareState::Confirmed);
    assert_eq!(outcome.writes, 4);
    assert!(!outcome.deduplicated);

    let pump = outcome.readback_for(Channel::Pump).unwrap();
    assert_eq!(pump.mode, Some(PwmMode::Fixed));
    assert_eq!(pump.duty, Some(180));
    assert!(pump.is_confirmed());

    assert_eq!(read_attribute(&hwmon, "pwm1"), "180");
    assert_eq!(read_attribute(&hwmon, "pwm2"), "90");
    assert_eq!(read_attribute(&hwmon, "pwm1_enable"), "1");
}

#[test]
fn repeating_a_fixed_duty_performs_no_further_write() {
    let harness = Harness::start("apply-dedup");
    let hwmon = harness.hwmon_path();
    let mut client = harness.client();

    apply(&mut client, CoolingProgram::Fixed { pump: 180, fan: 90 });
    let after_first = snapshot(&hwmon);

    for _ in 0..5 {
        let repeat = apply(&mut client, CoolingProgram::Fixed { pump: 180, fan: 90 });
        assert_eq!(repeat.writes, 0);
        assert!(repeat.deduplicated);
        assert_eq!(repeat.hardware, HardwareState::Confirmed);
    }

    assert_eq!(
        after_first,
        snapshot(&hwmon),
        "a repeated Apply touched the device"
    );
}

#[test]
fn an_out_of_range_duty_is_refused_with_its_range_and_writes_nothing() {
    let harness = Harness::start("apply-range");
    let hwmon = harness.hwmon_path();
    let before = snapshot(&hwmon);
    let mut client = harness.client();

    let response = client
        .request(Request::ApplyProgram {
            program: CoolingProgram::Fixed {
                pump: MIN_PUMP_DUTY - 1,
                fan: 90,
            },
        })
        .unwrap();
    match response {
        Response::Error(IpcError::Validation(error)) => {
            let message = error.to_string();
            assert!(message.contains("51-255"), "{message}");
            assert_eq!(error.channel(), Some(Channel::Pump));
        }
        other => panic!("expected a validation error, got {other:?}"),
    }

    assert_eq!(before, snapshot(&hwmon), "a refused duty reached hwmon");
}

#[test]
fn a_curve_apply_writes_forty_values_per_channel_in_one_transaction() {
    let harness = Harness::start("apply-curve");
    let mut client = harness.client();
    let curve = CurveNodes::starting_ramp().interpolate();

    let outcome = apply(
        &mut client,
        CoolingProgram::Curve {
            pump: curve.clone(),
            fan: curve.clone(),
        },
    );
    assert_eq!(outcome.hardware, HardwareState::Confirmed);
    assert_eq!(outcome.writes, 2 * (CURVE_POINT_COUNT as u32 + 1));

    // Every point landed, in the order the ABI expects.
    assert_eq!(harness.fake.written_curve(&harness.hwmon, 1), curve.points);
    assert_eq!(harness.fake.written_curve(&harness.hwmon, 2), curve.points);
    let hwmon = harness.hwmon_path();
    assert_eq!(read_attribute(&hwmon, "pwm1_enable"), "2");
    assert_eq!(read_attribute(&hwmon, "pwm2_enable"), "2");

    // The forty points are write-only on this driver, so they are reported as
    // unconfirmed rather than claimed as verified.
    assert_eq!(
        outcome
            .readback_for(Channel::Pump)
            .unwrap()
            .curve_points_confirmed,
        None
    );
    assert_eq!(
        outcome.readback_for(Channel::Pump).unwrap().mode,
        Some(PwmMode::Curve)
    );
}

#[test]
fn a_non_monotonic_curve_is_refused_before_the_first_point_is_written() {
    let harness = Harness::start("apply-curve-invalid");
    let before = harness.fake.written_curve(&harness.hwmon, 1);
    let mut client = harness.client();

    let mut curve = CurveNodes::starting_ramp().interpolate();
    curve.points[30] = curve.points[29] - 1;

    let response = client
        .request(Request::ApplyProgram {
            program: CoolingProgram::Curve {
                pump: curve.clone(),
                fan: curve,
            },
        })
        .unwrap();
    match response {
        Response::Error(IpcError::Validation(error)) => {
            let message = error.to_string();
            assert!(message.contains("never decrease"), "{message}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }

    assert_eq!(
        before,
        harness.fake.written_curve(&harness.hwmon, 1),
        "a refused curve reached the device"
    );
    let hwmon = harness.hwmon_path();
    assert_eq!(read_attribute(&hwmon, "pwm1_enable"), "0");
}

#[test]
fn applying_without_write_permission_is_refused_and_touches_nothing() {
    // No udev rule: every control attribute stays read-only.
    let harness = Harness::start_with("apply-read-only", false);
    let hwmon = harness.hwmon_path();
    let before = snapshot(&hwmon);
    let mut client = harness.client();

    let response = client
        .request(Request::ApplyProgram {
            program: CoolingProgram::Fixed { pump: 180, fan: 90 },
        })
        .unwrap();
    match response {
        Response::Error(IpcError::Incompatible { details }) => {
            assert!(
                details
                    .iter()
                    .any(|detail| detail.capability == CapabilityId::PumpDuty)
            );
            assert!(details.iter().all(|detail| !detail.reason.is_empty()));
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }

    assert_eq!(before, snapshot(&hwmon));
}

#[test]
fn the_onboard_program_reaches_the_hardware_as_no_write_at_all() {
    let harness = Harness::start("apply-onboard");
    let hwmon = harness.hwmon_path();
    let before = snapshot(&hwmon);
    let mut client = harness.client();

    let outcome = apply(&mut client, CoolingProgram::Onboard);
    assert_eq!(outcome.hardware, HardwareState::Onboard);
    assert_eq!(outcome.writes, 0);
    assert_eq!(before, snapshot(&hwmon));
}

#[test]
fn activating_a_profile_writes_it_and_reports_the_readback() {
    let harness = Harness::start("activate-writes");
    let hwmon = harness.hwmon_path();
    let mut client = harness.client();

    client
        .request(Request::SaveProfile {
            profile: fixed("Silent", 120, 80),
        })
        .unwrap();

    let Response::Activated(outcome) = client
        .request(Request::ActivateProfile {
            name: "Silent".into(),
        })
        .unwrap()
    else {
        panic!("expected an activation");
    };
    assert_eq!(outcome.hardware, HardwareState::Confirmed);
    let applied = outcome
        .applied
        .expect("an activation that writes reports it");
    assert_eq!(applied.writes, 4);
    assert_eq!(applied.readback_for(Channel::Fan).unwrap().duty, Some(80));
    assert_eq!(read_attribute(&hwmon, "pwm1"), "120");
}

#[test]
fn a_stalled_channel_raises_an_alert_after_three_samples() {
    let harness = Harness::start_paced("alert-stall", true, FAST_INTERVAL, |fake, hwmon| {
        // The fan reports zero while the firmware failsafe commands 100%.
        fake.set_reading(hwmon, "fan2_input", "0");
    });
    let mut client = harness.client();

    let snapshot = harness.wait_for_telemetry(&mut client, Duration::from_secs(3), |snapshot| {
        !snapshot.alerts.is_empty()
    });

    let stalled = snapshot
        .alerts
        .iter()
        .find_map(|alert| match alert {
            SafetyAlert::ChannelStalled {
                channel,
                commanded_duty,
                samples,
                rpm,
            } => Some((*channel, *commanded_duty, *samples, *rpm)),
            _ => None,
        })
        .expect("a commanded channel at zero RPM must raise an alert");

    assert_eq!(stalled.0, Channel::Fan);
    assert_eq!(stalled.1, 255, "mode 0 commands full speed");
    assert!(stalled.2 >= 3);
    assert_eq!(stalled.3, 0);
    assert!(
        snapshot
            .alerts
            .iter()
            .all(|alert| !alert.message().is_empty()),
        "every alert names its channel and readback"
    );
}

#[test]
fn a_coolant_at_the_failsafe_threshold_raises_a_critical_alert() {
    let harness = Harness::start_paced("alert-liquid", true, FAST_INTERVAL, |fake, hwmon| {
        fake.set_reading(hwmon, "temp1_input", "61400");
    });
    let mut client = harness.client();

    let snapshot = harness.wait_for_telemetry(&mut client, Duration::from_secs(3), |snapshot| {
        !snapshot.alerts.is_empty()
    });

    let critical = snapshot
        .alerts
        .iter()
        .find(|alert| matches!(alert, SafetyAlert::LiquidCritical { .. }))
        .expect("a coolant above 60 C must raise an alert");
    let message = critical.message();
    assert!(message.contains("61.4"), "{message}");
    assert!(
        message.contains("overrides neither"),
        "the application must not claim to alter the failsafe: {message}"
    );
    assert!(
        !message.contains("both channels"),
        "the fan failsafe is undocumented and must not be asserted: {message}"
    );
}

#[test]
fn a_curve_stops_at_the_last_point_the_kernel_abi_defines() {
    let harness = Harness::start("curve-abi-bound");
    let mut client = harness.client();
    let curve = CurveNodes::flat(200).interpolate();

    apply(
        &mut client,
        CoolingProgram::Curve {
            pump: curve.clone(),
            fan: curve,
        },
    );

    // Exactly forty points exist and exactly forty were written. Nothing this
    // application writes can reach past 59 C, which is where the firmware
    // failsafe takes over.
    assert_eq!(harness.fake.written_curve(&harness.hwmon, 1).len(), 40);
    let hwmon = harness.hwmon_path();
    assert!(!hwmon.join("temp1_auto_point41_pwm").exists());
}

#[test]
fn a_diagnostics_export_records_what_reached_the_hardware() {
    let harness = Harness::start("diagnostics-applied");
    let mut client = harness.client();
    apply(&mut client, CoolingProgram::Fixed { pump: 180, fan: 90 });

    let Response::Diagnostics(export) = client.request(Request::Diagnostics).unwrap() else {
        panic!("expected diagnostics");
    };
    let json = serde_json::to_string(&export).unwrap();
    assert!(json.contains("program_applied"), "{json}");
    assert!(json.contains("\"writes\":4"), "{json}");
    assert!(!json.contains(nzxt_hardware_linux::testing::KRAKEN_FIXTURE_SERIAL));
}

// ---------------------------------------------------------------------------
// Lighting, EP-003
// ---------------------------------------------------------------------------

fn lighting(channel: u8, hex: &str) -> LightingCommand {
    LightingCommand {
        channel,
        program: LightingProgram::Fixed {
            color: Rgb::parse_hex(hex).unwrap(),
            brightness: Brightness::new(60).unwrap(),
        },
    }
}

#[test]
fn a_controller_that_answered_is_reported_with_its_channels_and_accessories() {
    let harness = Harness::start_lit("lighting-topology", "9.9.9", 3);
    let mut client = harness.client();

    let status = client.status().unwrap();
    assert_eq!(status.lighting.len(), 3);
    assert_eq!(status.lighting[0].channel, 1);
    assert_eq!(
        status.lighting[0].accessories,
        vec!["HUE 2 LED Strip 300 mm"]
    );
    // Nothing has been commanded, so nothing is claimed to be showing.
    assert!(status.lighting.iter().all(|c| c.committed.is_none()));

    // The topology reached the capability record with its evidence attached.
    let record = client.capabilities().unwrap();
    let rgb = record.device(RGB_CONTROLLER).unwrap();
    let topology = rgb.rgb.as_ref().expect("the controller answered");
    assert_eq!(topology.channel_count(), Some(3));
    assert_eq!(topology.firmware.value().map(String::as_str), Some("9.9.9"));
    // The controller reports accessory identifiers, never LED counts.
    assert!(
        topology
            .channels
            .value()
            .unwrap()
            .iter()
            .all(|channel| !channel.led_count.is_known())
    );
}

#[test]
fn an_unvalidated_firmware_refuses_every_write_and_names_the_missing_evidence() {
    let harness = Harness::start_lit("lighting-unvalidated", "9.9.9", 3);
    let mut client = harness.client();

    let error = client.apply_lighting(lighting(1, "7C5CFF")).unwrap_err();
    let message = error.to_string();
    assert!(
        matches!(
            &error,
            nzxt_core::client::ClientError::Refused(IpcError::Incompatible { .. })
        ),
        "{error:?}"
    );

    // The refusal points at the story that would produce the evidence.
    let record = client.capabilities().unwrap();
    let reason = record
        .device(RGB_CONTROLLER)
        .unwrap()
        .capability(CapabilityId::RgbFixedColor)
        .unwrap()
        .state
        .blocked_reason()
        .unwrap();
    assert!(reason.contains("9.9.9"), "{reason}");
    assert!(reason.contains("US-013"), "{reason}");
    let _ = message;

    // And the daemon still knows nothing is showing, because nothing was sent.
    assert!(
        client
            .status()
            .unwrap()
            .lighting
            .iter()
            .all(|channel| channel.committed.is_none())
    );
}

#[test]
fn a_channel_outside_the_reported_topology_is_refused_before_any_write() {
    let harness = Harness::start_lit("lighting-channel", "9.9.9", 3);
    let mut client = harness.client();

    match client.apply_lighting(lighting(4, "FFFFFF")) {
        Err(nzxt_core::client::ClientError::Refused(IpcError::Lighting(error))) => {
            let message = error.to_string();
            assert!(message.contains("exposes 3"), "{message}");
        }
        other => panic!("expected a typed channel rejection, got {other:?}"),
    }
}

#[test]
fn an_out_of_range_brightness_cannot_even_be_decoded() {
    let harness = Harness::start_lit("lighting-brightness", "9.9.9", 3);

    // Brightness is a validated newtype, so a frame carrying 200 is refused by
    // the decoder: the value never becomes a program the daemon has to reject.
    // The same holds for a color channel outside a byte and an unknown effect.
    for payload in [
        r#"{"request":"apply_lighting","command":{"channel":1,"program":{"mode":"fixed","color":{"r":255,"g":0,"b":0},"brightness":200}}}"#,
        r#"{"request":"apply_lighting","command":{"channel":1,"program":{"mode":"fixed","color":{"r":300,"g":0,"b":0},"brightness":50}}}"#,
        r#"{"request":"apply_lighting","command":{"channel":1,"program":{"mode":"effect","effect":"rainbow_flow","colors":[],"brightness":50,"speed":"normal","direction":"forward"}}}"#,
    ] {
        let stream = harness.raw();
        let mut writer = BufWriter::new(stream.try_clone().unwrap());
        let mut reader = BufReader::new(stream);
        write_frame(
            &mut writer,
            &Request::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        let _: Response = read_frame(&mut reader).unwrap();

        writer.write_all(payload.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
        let response: Response = read_frame(&mut reader).unwrap();
        assert!(
            matches!(response, Response::Error(IpcError::Malformed { .. })),
            "{payload} produced {response:?}"
        );
    }
}

#[test]
fn an_absent_controller_reports_no_channels_and_accepts_no_command() {
    let harness = Harness::start("lighting-absent");
    let mut client = harness.client();

    let status = client.status().unwrap();
    assert!(status.lighting.is_empty());

    let error = client.apply_lighting(lighting(1, "FFFFFF")).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("does not exist"), "{message}");
}

#[test]
fn the_write_path_opens_only_for_a_firmware_the_probe_validated() {
    match nzxt_hardware_linux::rgb::VALIDATED_FIRMWARE.first() {
        Some(firmware) => {
            let harness = Harness::start_lit("lighting-validated", firmware, 3);
            let mut client = harness.client();

            let outcome = client.apply_lighting(lighting(2, "7C5CFF")).unwrap();
            assert_eq!(outcome.channel, 2);
            assert_eq!(outcome.writes, 1);
            assert!(!outcome.deduplicated);
            assert_eq!(outcome.hardware, HardwareState::Confirmed);

            // The committed state is what the daemon reports, because the
            // controller exposes no way to read a channel back.
            let status = client.status().unwrap();
            let channel = status
                .lighting
                .iter()
                .find(|channel| channel.channel == 2)
                .unwrap();
            assert_eq!(channel.committed, Some(lighting(2, "7C5CFF").program));

            // The same request again sends nothing.
            let repeat = client.apply_lighting(lighting(2, "7C5CFF")).unwrap();
            assert!(repeat.deduplicated);
            assert_eq!(repeat.writes, 0);

            // A different color inside the cadence floor is refused outright.
            match client.apply_lighting(lighting(2, "00FF00")) {
                Err(nzxt_core::client::ClientError::Refused(IpcError::Lighting(error))) => {
                    assert!(error.to_string().contains("one every"), "{error}");
                }
                other => panic!("expected a cadence rejection, got {other:?}"),
            }

            // Off is a different program, so it is a real write once the floor
            // has passed.
            std::thread::sleep(Duration::from_millis(
                nzxt_core::lighting::MIN_COMMAND_INTERVAL_MS + 10,
            ));
            let off = client
                .apply_lighting(LightingCommand {
                    channel: 2,
                    program: LightingProgram::Off,
                })
                .unwrap();
            assert_eq!(off.writes, 1);

            // The whole exchange is in the diagnostics, as a summary rather
            // than as packet bytes.
            let Response::Diagnostics(export) = client.request(Request::Diagnostics).unwrap()
            else {
                panic!("expected diagnostics");
            };
            let json = serde_json::to_string(&export).unwrap();
            assert!(json.contains("lighting_applied"), "{json}");
            assert!(json.contains("fixed #7C5CFF at 60%"), "{json}");
            assert!(!json.contains("0x2a"), "{json}");
        }
        None => {
            // No firmware has been validated on real hardware yet, so the gate
            // must refuse every controller, whatever it reports about itself.
            let harness = Harness::start_lit("lighting-closed", "1.0.0", 3);
            let mut client = harness.client();
            assert!(
                client.apply_lighting(lighting(1, "FFFFFF")).is_err(),
                "the write path must stay closed until US-013 records a firmware"
            );
        }
    }
}

#[test]
fn a_saved_effect_round_trips_without_protocol_bytes_reaching_the_file() {
    let harness = Harness::start_lit("lighting-profile", "9.9.9", 3);
    let effect = LightingProgram::Effect {
        effect: nzxt_core::lighting::LightingEffect::SpectrumWave,
        colors: Vec::new(),
        brightness: Brightness::new(80).unwrap(),
        speed: nzxt_core::lighting::EffectSpeed::Faster,
        direction: nzxt_core::lighting::EffectDirection::Backward,
    };
    let profile = Profile {
        name: "Wave".into(),
        program: CoolingProgram::Onboard,
        device: None,
        lighting: vec![
            LightingCommand {
                channel: 1,
                program: effect.clone(),
            },
            LightingCommand {
                channel: 2,
                program: LightingProgram::Off,
            },
        ],
        display: None,
    };

    {
        let mut client = harness.client();
        client
            .request(Request::SaveProfile {
                profile: profile.clone(),
            })
            .unwrap();
    }

    // The stored file carries names, not a wire encoding.
    let stored = std::fs::read_to_string(harness.paths.config_file()).unwrap();
    assert!(stored.contains("spectrum_wave"), "{stored}");
    assert!(stored.contains("backward"), "{stored}");
    for protocol in ["2a", "0x2a", "report", "packet", "hidraw"] {
        assert!(
            !stored.contains(protocol),
            "{protocol:?} leaked into the configuration file:\n{stored}"
        );
    }

    // A fresh daemon over the same directory reads the same parameters back.
    let restarted = Daemon::start_with(
        Paths::new(
            harness.paths.runtime_dir.join("second"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
        &harness.proc_root,
        FAST_INTERVAL,
        RgbBackend::Transport(Box::new(FakeController::new("9.9.9", 3))),
        LcdBackend::None,
    )
    .unwrap();

    let reloaded = restarted.status().active_profile.clone();
    assert_eq!(reloaded, SAFE_PROFILE_NAME, "saving does not activate");

    let mut client = harness.client();
    let (_, profiles) = client.profiles().unwrap();
    let stored_profile = profiles
        .iter()
        .find(|candidate| candidate.name == "Wave")
        .expect("the saved profile came back");
    assert_eq!(stored_profile, &profile);
    assert_eq!(stored_profile.lighting[0].program, effect);
}

/// Milliseconds since the Unix epoch, for age assertions.
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// A preset the panel tests drive.
fn preset(mode: DisplayMode) -> DisplayPreset {
    DisplayPreset {
        mode,
        ..DisplayPreset::default_infographic()
    }
}

/// The panel sits on the same device as the thermal path, so a Kraken that
/// goes away takes the panel with it. The record of what the panel is showing
/// has to go too: a panel that comes back has swapped nothing, so a preserved
/// record would let the next Apply deduplicate against a picture the glass no
/// longer holds, and would leave the link believing it is still primed when the
/// device it was primed against is gone.
#[test]
fn a_kraken_that_goes_away_takes_the_panel_record_with_it() {
    let harness = Harness::start_lcd("lcd-disconnect", "2.0.0");
    let mut client = harness.client();

    client
        .apply_display(preset(DisplayMode::DualInfographic))
        .expect("a validated firmware accepts a frame");
    assert!(
        client.status().unwrap().display.committed.is_some(),
        "the panel is showing something before the device leaves"
    );

    // How a device that unplugs mid-session presents: every reading stops
    // answering at once, and the instance stops resolving, so re-locating it on
    // the next tick finds nothing.
    for attribute in ["temp1_input", "fan1_input", "fan2_input", "name"] {
        harness
            .fake
            .remove_attribute(&harness.hwmon_path(), attribute);
    }

    let snapshot = harness.wait_for_telemetry(&mut client, Duration::from_secs(5), |snapshot| {
        !snapshot.kraken.present
    });
    assert!(!snapshot.kraken.present, "the device is gone");

    let status = client.status().unwrap();
    assert!(
        status.display.committed.is_none(),
        "the panel record must not outlive the device it describes"
    );
    // The preset the operator asked for is deliberately kept, which is what
    // lets the panel resume on its own once the device answers again. Only the
    // claim about what the glass currently holds is dropped. The fixture's link
    // is in memory and never notices the device leave, so `streaming` still
    // reads true here; on real hardware the transport would be gone.
}

#[test]
fn a_machine_with_no_reachable_panel_reports_no_geometry_and_refuses_frames() {
    let harness = Harness::start("lcd-absent");
    let mut client = harness.client();

    let status = client.status().unwrap();
    assert!(
        status.display.panel.is_none(),
        "a panel nothing answered for must not be given a resolution"
    );
    assert!(status.display.committed.is_none());
    assert!(!status.display.streaming);

    let error = client
        .apply_display(preset(DisplayMode::DualInfographic))
        .unwrap_err();
    assert!(
        matches!(
            &error,
            nzxt_core::client::ClientError::Refused(IpcError::Incompatible { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn a_panel_that_answered_is_recorded_with_its_geometry_and_its_settings() {
    let harness = Harness::start_lcd("lcd-topology", "2.0.4");
    let mut client = harness.client();

    let record = client.capabilities().unwrap();
    let lcd = record.device(KRAKEN_BASE).unwrap().lcd.as_ref().unwrap();
    assert_eq!(lcd.firmware.value().map(String::as_str), Some("2.0.4"));
    assert!(lcd.answered(), "the display report was answered");

    let panel = lcd.panel.value().unwrap();
    assert_eq!((panel.width, panel.height), (240, 240));
    assert_eq!(panel.frame_bytes, 240 * 240 * 2);
    assert_eq!(panel.bulk_endpoint, 0x02);
    assert_eq!(panel.bulk_interface, 0);

    // The geometry is a candidate this product carries, and the record says so
    // rather than implying the device reported it.
    match &lcd.panel {
        nzxt_core::capability::Evidenced::Known { source, .. } => {
            assert!(source.contains("candidate"), "{source}");
            assert!(source.contains("reports no"), "{source}");
        }
        other => panic!("expected a recorded candidate, got {other:?}"),
    }

    // And the daemon serves that same geometry in its status, so the client
    // can size a preview without asking for the whole capability record.
    let status = client.status().unwrap();
    assert_eq!(status.display.panel.map(|panel| panel.width), Some(240));
}

#[test]
fn an_unvalidated_firmware_refuses_every_frame_and_names_the_missing_evidence() {
    let harness = Harness::start_lcd("lcd-unvalidated", "2.9.9");
    let mut client = harness.client();

    let error = client
        .apply_display(preset(DisplayMode::DualInfographic))
        .unwrap_err();
    assert!(
        matches!(
            &error,
            nzxt_core::client::ClientError::Refused(IpcError::Incompatible { .. })
        ),
        "{error:?}"
    );

    let record = client.capabilities().unwrap();
    let reason = record
        .device(KRAKEN_BASE)
        .unwrap()
        .capability(CapabilityId::LcdFrame)
        .unwrap()
        .state
        .blocked_reason()
        .unwrap();
    assert!(reason.contains("2.9.9"), "{reason}");
    assert!(reason.contains("US-016"), "{reason}");

    // Nothing was sent, so the daemon claims nothing about the panel.
    let status = client.status().unwrap();
    assert!(status.display.committed.is_none());
    assert!(!status.display.streaming);
}

#[test]
fn an_invalid_preset_is_refused_before_the_capability_gate_is_even_reached() {
    // Image mode with no file is wrong whatever the panel reports, so it is
    // refused as a validation error rather than as an incompatibility.
    let harness = Harness::start_lcd("lcd-invalid", "2.0.4");
    let mut client = harness.client();

    let error = client
        .apply_display(preset(DisplayMode::Image))
        .unwrap_err();
    match &error {
        nzxt_core::client::ClientError::Refused(IpcError::Display(display)) => {
            assert_eq!(display.field(), Some("image"));
        }
        other => panic!("expected a typed display refusal, got {other:?}"),
    }
}

#[test]
fn every_validated_firmware_completes_the_whole_frame_path_from_the_client() {
    // Vacuous until a `--lcd-write-probe` an operator watched fills the list,
    // which is the correct failure direction: nothing is claimed to work on a
    // firmware nobody has driven. Once filled, this proves the round trip from
    // the client's own entry point.
    for firmware in nzxt_hardware_linux::lcd::VALIDATED_FIRMWARE {
        let harness = Harness::start_lcd("lcd-validated", firmware);
        let mut client = harness.client();

        let wanted = preset(DisplayMode::DualInfographic);
        let outcome = client.apply_display(wanted.clone()).unwrap();
        assert_eq!(outcome.frames, 1, "{firmware} sent no frame");
        assert!(!outcome.deduplicated);
        assert_eq!(outcome.hardware, HardwareState::Confirmed);

        // The daemon now knows what the panel holds, and says it is streaming
        // because the preset reads telemetry.
        let status = client.status().unwrap();
        assert_eq!(status.display.committed.as_ref(), Some(&wanted));
        assert!(status.display.streaming);

        // A solid field is not streamed: nothing in it changes with a sample.
        let solid = preset(DisplayMode::Solid);
        assert_eq!(client.apply_display(solid).unwrap().frames, 1);
        assert!(!client.status().unwrap().display.streaming);
    }
}

#[test]
fn a_saved_profile_round_trips_its_panel_preset_without_pixels_reaching_the_file() {
    let harness = Harness::start_lcd("lcd-profile", "2.0.4");
    let mut wanted = preset(DisplayMode::DualInfographic);
    wanted.readings[0].metric = nzxt_core::display::LcdMetric::LiquidTemperature;
    wanted.orientation = nzxt_core::display::Orientation::Deg180;

    let profile = Profile {
        name: "Panel".into(),
        program: CoolingProgram::Onboard,
        device: None,
        lighting: Vec::new(),
        display: Some(wanted.clone()),
    };

    {
        let mut client = harness.client();
        client
            .request(Request::SaveProfile {
                profile: profile.clone(),
            })
            .unwrap();
    }

    // The stored file carries a description, not a picture.
    let stored = std::fs::read_to_string(harness.paths.config_file()).unwrap();
    assert!(stored.contains("dual_infographic"), "{stored}");
    assert!(stored.contains("liquid_temperature"), "{stored}");
    assert!(stored.contains("deg180"), "{stored}");
    for leaked in ["rgb565", "0x36", "framebuffer", "pixels", "115200"] {
        assert!(
            !stored.to_ascii_lowercase().contains(leaked),
            "{leaked:?} leaked into the configuration file:\n{stored}"
        );
    }

    // A fresh daemon over the same directory reads the same preset back.
    let restarted = Daemon::start_with(
        Paths::new(
            harness.paths.runtime_dir.join("second"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
        &harness.proc_root,
        FAST_INTERVAL,
        RgbBackend::None,
        LcdBackend::Link(Box::new(FakeKraken::new("2.0.4").link())),
    )
    .unwrap();
    let reloaded = restarted
        .status()
        .devices
        .iter()
        .map(|device| device.id)
        .collect::<Vec<_>>();
    assert!(reloaded.contains(&KRAKEN_BASE));

    let mut client = harness.client();
    let (_, profiles) = client.profiles().unwrap();
    let stored_profile = profiles
        .iter()
        .find(|candidate| candidate.name == "Panel")
        .unwrap();
    assert_eq!(stored_profile.display.as_ref(), Some(&wanted));
}
