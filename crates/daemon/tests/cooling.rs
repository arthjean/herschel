// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The thermal path, from the client to the attribute.
//!
//! What reaches `hwmon` and what never does. Every refusal here is asserted
//! against the tree as well as against the response, because a value that was
//! rejected must leave no trace on the hardware, and a transaction that failed
//! partway must leave every channel on the program it was running.

mod common;

use kori_core::capability::CapabilityId;
use kori_core::ipc::{HardwareState, IpcError, Request, Response};
use kori_core::profile::{
    CURVE_POINT_COUNT, Channel, CoolingProgram, CurveNodes, MIN_PUMP_DUTY, Profile,
    TemperatureCurve,
};
use kori_core::telemetry::PwmMode;

use common::{Harness, apply, fixed, read_attribute, snapshot};

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
    curve.points_mut()[30] = 100;
    let response = client
        .request(Request::SaveProfile {
            profile: Profile {
                name: "Falling".into(),
                program: CoolingProgram::Curve {
                    pump: curve,
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
            pump: curve,
            fan: curve,
        },
    );
    assert_eq!(outcome.hardware, HardwareState::Confirmed);
    assert_eq!(outcome.writes, 2 * (CURVE_POINT_COUNT as u32 + 1));

    // Every point landed, in the order the ABI expects.
    assert_eq!(
        harness.fake.written_curve(&harness.hwmon, 1),
        *curve.points()
    );
    assert_eq!(
        harness.fake.written_curve(&harness.hwmon, 2),
        *curve.points()
    );
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
    curve.points_mut()[30] = curve.points()[29] - 1;

    let response = client
        .request(Request::ApplyProgram {
            program: CoolingProgram::Curve {
                pump: curve,
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
    let harness = Harness::start_read_only("apply-read-only");
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
fn a_curve_stops_at_the_last_point_the_kernel_abi_defines() {
    let harness = Harness::start("curve-abi-bound");
    let mut client = harness.client();
    let curve = CurveNodes::flat(200).interpolate();

    apply(
        &mut client,
        CoolingProgram::Curve {
            pump: curve,
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
    assert!(!json.contains(kori_hardware_linux::testing::KRAKEN_FIXTURE_SERIAL));
}
