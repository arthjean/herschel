// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Sampling, and the alerts it raises.
//!
//! Three collectors, one snapshot, and the property that holds the whole design
//! together: a source that is unavailable says so with its cause instead of
//! reporting a zero. Nothing here writes, which is asserted against the tree
//! rather than promised.

mod common;

use std::time::Duration;

use kori_core::profile::Channel;
use kori_core::telemetry::{Collector, PwmMode, SafetyAlert};

use common::{FAST_INTERVAL, Harness, Machine, now_unix_ms, snapshot};

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
    let occupancy = memory.percent().expect("a sampled total is never zero");
    assert!(occupancy > 0.0 && occupancy < 100.0);
    assert_eq!(snapshot.system.cpu_temperature_c.copied(), Some(46.75));

    assert_eq!(snapshot.interval_ms, FAST_INTERVAL.as_millis() as u64);
    assert!(snapshot.sequence > 0);
}

#[test]
fn an_unreadable_channel_is_unavailable_with_its_cause_rather_than_zero() {
    let harness = Harness::start_prepared("telemetry-missing", |fake, hwmon| {
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
    // The one case that measures the shipped cadence rather than a fast one, so
    // it is the one case that names an interval.
    let harness = Harness::start_on(
        "telemetry-age",
        Machine {
            interval: Duration::from_millis(kori_core::telemetry::SAMPLE_INTERVAL_MS),
            ..Machine::default()
        },
        |_, _| {},
    );
    let mut client = harness.client();

    let mut ages = Vec::new();
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(
            kori_core::telemetry::SAMPLE_INTERVAL_MS,
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
fn a_stalled_channel_raises_an_alert_after_three_samples() {
    let harness = Harness::start_prepared("alert-stall", |fake, hwmon| {
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
    let harness = Harness::start_prepared("alert-liquid", |fake, hwmon| {
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
