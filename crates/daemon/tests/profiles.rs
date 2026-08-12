// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Named profiles, over the socket.
//!
//! Saving, activating and deleting, plus the two refusals that have to happen
//! before a selection is persisted: a profile the hardware cannot run and a
//! profile pinned to a device that is not there. A configuration that cannot be
//! trusted recovers to the built-in safe profile rather than being interpreted.

mod common;

use std::path::Path;

use kori_core::capability::CapabilityId;
use kori_core::ipc::{ConfigState, HardwareState, IpcError, Request, Response};
use kori_core::profile::{CoolingProgram, Profile, SAFE_PROFILE_NAME};
use kori_daemon::state::Daemon;
use kori_daemon::{LcdBackend, Paths, RgbBackend};
use kori_hardware_linux::SysfsRoot;

use common::{FAST_INTERVAL, Harness, fixed, snapshot};

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
    let harness = Harness::start_read_only("read-only");
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
        device: Some(kori_core::DeviceId::new(0x1e71, 0x2007)),
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
