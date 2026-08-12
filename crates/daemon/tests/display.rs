// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The panel path, from the client to the glass.
//!
//! The same write gate the lighting path has, plus the geometry: a frame is laid
//! out for a panel that answered, or it is not laid out at all. A device that
//! never answered `0x31 0x01` may carry no panel, and is reported that way rather
//! than assumed to have one.

mod common;

use kori_core::KRAKEN_BASE;
use kori_core::capability::CapabilityId;
use kori_core::display::DisplayMode;
use kori_core::ipc::{HardwareState, IpcError, Request};
use kori_core::profile::{CoolingProgram, Profile};
use kori_daemon::state::Daemon;
use kori_daemon::{LcdBackend, Paths, RgbBackend};
use kori_hardware_linux::SysfsRoot;
use kori_hardware_linux::testing::FakeKraken;

use common::{FAST_INTERVAL, Harness, preset};

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
            kori_core::client::ClientError::Refused(IpcError::Incompatible { .. })
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
        kori_core::capability::Evidenced::Known { source, .. } => {
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
            kori_core::client::ClientError::Refused(IpcError::Incompatible { .. })
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
        kori_core::client::ClientError::Refused(IpcError::Display(display)) => {
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
    for firmware in kori_hardware_linux::lcd::VALIDATED_FIRMWARE {
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
    wanted.readings[0].metric = kori_core::display::LcdMetric::LiquidTemperature;
    wanted.orientation = kori_core::display::Orientation::Deg180;

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
