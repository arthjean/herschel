// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The socket, and everything that crosses it.
//!
//! One listener, one peer check, one version handshake, one frame format. Every
//! case here drives the socket rather than the hardware: what a malformed frame
//! does, what an unhandshaken client is told, what two clients see of each
//! other, and what an export is allowed to carry off this machine.

mod common;

use std::io::{BufReader, BufWriter, Write};
use std::time::Duration;

use kori_core::client::Client;
use kori_core::ipc::{
    ConfigState, IpcError, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request, Response, read_frame,
    write_frame,
};
use kori_core::profile::SAFE_PROFILE_NAME;

use common::{Harness, fixed, snapshot};

#[test]
fn a_client_completes_the_handshake_and_reads_status() {
    let harness = Harness::start("handshake");
    let mut client = harness.client();

    assert_eq!(client.daemon_version(), kori_daemon::DAEMON_VERSION);

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
        !json.contains(kori_hardware_linux::testing::KRAKEN_FIXTURE_SERIAL),
        "kraken serial leaked"
    );
    assert!(
        !json.contains(kori_hardware_linux::testing::RGB_FIXTURE_SERIAL),
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
