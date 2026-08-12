// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The controller path, from the client to the report.
//!
//! The write gate is what most of this is about: no lighting byte leaves the
//! process unless the controller answered its topology and its firmware is one a
//! write probe validated. An unvalidated firmware is refused with the evidence it
//! is missing, which is the refusal an operator can act on.

mod common;

use std::io::{BufReader, BufWriter, Write};
use std::time::Duration;

use kori_core::RGB_CONTROLLER;
use kori_core::capability::CapabilityId;
use kori_core::ipc::{
    HardwareState, IpcError, PROTOCOL_VERSION, Request, Response, read_frame, write_frame,
};
use kori_core::lighting::{Brightness, LightingCommand, LightingProgram};
use kori_core::profile::{CoolingProgram, Profile, SAFE_PROFILE_NAME};
use kori_daemon::state::Daemon;
use kori_daemon::{LcdBackend, Paths, RgbBackend};
use kori_hardware_linux::SysfsRoot;
use kori_hardware_linux::testing::FakeController;

use common::{FAST_INTERVAL, Harness, lighting};

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
            kori_core::client::ClientError::Refused(IpcError::Incompatible { .. })
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
        Err(kori_core::client::ClientError::Refused(IpcError::Lighting(error))) => {
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
    match kori_hardware_linux::rgb::VALIDATED_FIRMWARE.first() {
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
                Err(kori_core::client::ClientError::Refused(IpcError::Lighting(error))) => {
                    assert!(error.to_string().contains("one every"), "{error}");
                }
                other => panic!("expected a cadence rejection, got {other:?}"),
            }

            // Off is a different program, so it is a real write once the floor
            // has passed.
            std::thread::sleep(Duration::from_millis(
                kori_core::lighting::MIN_COMMAND_INTERVAL_MS + 10,
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
                "the write path must stay closed until a probe records a firmware"
            );
        }
    }
}

#[test]
fn a_saved_effect_round_trips_without_protocol_bytes_reaching_the_file() {
    let harness = Harness::start_lit("lighting-profile", "9.9.9", 3);
    let effect = LightingProgram::Effect {
        effect: kori_core::lighting::LightingEffect::SpectrumWave,
        colors: Vec::new(),
        brightness: Brightness::new(80).unwrap(),
        speed: kori_core::lighting::EffectSpeed::Faster,
        direction: kori_core::lighting::EffectDirection::Backward,
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
