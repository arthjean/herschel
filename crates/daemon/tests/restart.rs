// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What survives the machine being turned off.
//!
//! Every one of these walks two daemons over the same configuration directory,
//! because that is the only way to prove the replay: the operator edits without
//! ever saving under a name, so what the next start puts back is what the daemon
//! last committed. A curve is the sharpest case, since no readback can recover
//! one and the file is the only place it exists.

mod common;

use kori_core::display::DisplayMode;
use kori_core::ipc::{ConfigState, HardwareState, Request};
use kori_core::profile::{CoolingProgram, SAFE_PROFILE_NAME, TemperatureCurve};
use kori_core::telemetry::PwmMode;
use kori_daemon::state::Daemon;
use kori_daemon::{LcdBackend, Paths, RgbBackend};
use kori_hardware_linux::SysfsRoot;
use kori_hardware_linux::testing::FakeKraken;

use common::{FAST_INTERVAL, Harness, apply, fixed, preset, read_attribute};

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
    // restart has to restore the program itself, not just its selection.
    let hwmon = harness.hwmon_path();
    assert_eq!(read_attribute(&hwmon, "pwm1"), "120");
    assert_eq!(read_attribute(&hwmon, "pwm2"), "80");
    assert_eq!(
        read_attribute(&hwmon, "pwm1_enable"),
        PwmMode::Fixed.to_kernel().to_string()
    );
}

/// A picture the operator chose survives the machine being turned off.
///
/// The panel is edited without ever being saved under a name: the Lighting rows
/// write as they settle. So the record the next start replays is what the
/// daemon last committed, not what the active profile happened to carry, and
/// this walks that from one daemon to the next over the same configuration
/// directory.
#[test]
fn the_last_picture_committed_survives_a_daemon_restart() {
    let harness = Harness::start_lcd("lcd-restart", "2.0.0");
    let chosen = preset(DisplayMode::SingleReading);
    {
        let mut client = harness.client();
        client
            .apply_display(chosen.clone())
            .expect("a validated firmware accepts a frame");
        let (active, _) = client.profiles().unwrap();
        assert_eq!(
            active, SAFE_PROFILE_NAME,
            "no profile was saved, which is the whole point of the record"
        );
    }

    // A fresh daemon over the same configuration directory, as after a reboot,
    // with a panel that has just been powered on and is showing nothing.
    let restarted = Daemon::start_with(
        Paths::new(
            harness.paths.runtime_dir.join("second"),
            harness.paths.config_dir.clone(),
        ),
        &SysfsRoot::new(harness.fake.root_path()),
        &harness.proc_root,
        FAST_INTERVAL,
        RgbBackend::None,
        LcdBackend::Link(Box::new(FakeKraken::new("2.0.0").link())),
    )
    .unwrap();

    // Committed is only ever set by a transfer that completed, so this says the
    // frame went out again rather than that the preset was merely read back.
    assert_eq!(
        restarted.status().display.committed,
        Some(chosen),
        "the panel must come back on the picture it was left on"
    );
}

/// A curve the operator drew survives the machine being turned off, and the
/// next client can see the shape it is running.
///
/// Two facts in one walk, because the screen needs both. The daemon has to put
/// the curve back without any profile having been saved, and it has to report
/// what it committed: a curve is the one program no readback can recover, since
/// the driver publishes no attribute that returns one. A client that could not
/// see it would open on the starting ramp and the operator would draw theirs
/// again, which is the defect this covers.
#[test]
fn the_last_curve_committed_survives_a_daemon_restart_and_is_reported() {
    let harness = Harness::start("curve-restart");
    let mut drawn = TemperatureCurve::flat(140);
    for (index, point) in drawn.points_mut().iter_mut().enumerate() {
        *point = 140 + index as u8;
    }
    let program = CoolingProgram::Curve {
        pump: drawn,
        fan: drawn,
    };

    {
        let mut client = harness.client();
        assert_eq!(
            apply(&mut client, program.clone()).hardware,
            HardwareState::Confirmed
        );
        let status = client.status().unwrap();
        assert_eq!(
            status.cooling.as_ref(),
            Some(&program),
            "the running client must be able to draw what it just committed"
        );
        assert_eq!(
            status.active_profile, SAFE_PROFILE_NAME,
            "no profile was saved, which is the whole point of the record"
        );
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

    assert_eq!(
        restarted.status().cooling.as_ref(),
        Some(&program),
        "the cooler must come back on the curve it was left on"
    );
}
