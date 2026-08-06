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
use nzxt_core::ipc::{
    AccessMode, ConfigState, HardwareState, IpcError, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request,
    Response, read_frame, write_frame,
};
use nzxt_core::profile::{CoolingProgram, Profile, SAFE_PROFILE_NAME, TemperatureCurve};
use nzxt_core::{KRAKEN_BASE, RGB_CONTROLLER};
use nzxt_daemon::server::ShutdownHandle;
use nzxt_daemon::state::Daemon;
use nzxt_daemon::{Paths, Server};
use nzxt_hardware_linux::SysfsRoot;
use nzxt_hardware_linux::testing::FakeSysfs;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A daemon serving a fake machine on a real socket.
struct Harness {
    base: PathBuf,
    paths: Paths,
    fake: FakeSysfs,
    shutdown: ShutdownHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    fn start(name: &str) -> Self {
        Self::start_with(name, true)
    }

    fn start_with(name: &str, grant_write: bool) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("nzxt-ipc-{name}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let fake = FakeSysfs::new(&format!("ipc-{name}-{unique}"));
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();
        fake.add_rgb_controller();
        fake.add_unrelated_device();
        if grant_write {
            // Simulates an installed udev rule granting the two PWM nodes.
            for attribute in ["pwm1", "pwm2"] {
                fake.grant_write(&hwmon, attribute);
            }
            for channel in 1..=2 {
                for point in 1..=40 {
                    fake.grant_write(&hwmon, &format!("temp{channel}_auto_point{point}_pwm"));
                }
            }
        }

        let paths = Paths::new(base.join("run"), base.join("config"));
        let daemon = Daemon::start(paths.clone(), &SysfsRoot::new(fake.root_path())).unwrap();
        let server = Server::bind(&paths.socket, daemon).unwrap();
        let shutdown = server.shutdown_handle();
        let thread = std::thread::spawn(move || server.run());

        // The socket exists before bind returns, so a connect cannot race it.
        Self {
            base,
            paths,
            fake,
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
    let second = Daemon::start(
        harness.paths.clone(),
        &SysfsRoot::new(harness.fake.root_path()),
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
    assert!(matches!(outcome.hardware, HardwareState::NotApplied { .. }));

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
    let restarted = Daemon::start(
        Paths::new(
            harness.paths.runtime_dir.join("second"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
    )
    .unwrap();
    assert_eq!(restarted.status().active_profile, "Silent");
    assert_eq!(restarted.status().config, ConfigState::Loaded);
}

#[test]
fn a_corrupt_configuration_recovers_to_the_safe_profile() {
    let harness = Harness::start("corrupt");
    let config_file = harness.paths.config_file();
    std::fs::create_dir_all(config_file.parent().unwrap()).unwrap();
    std::fs::write(&config_file, "schema_version = 1\nactive_pro").unwrap();

    let daemon = Daemon::start(
        Paths::new(
            harness.paths.runtime_dir.join("recovery"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
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
