// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What this daemon claims, and what it says when it cannot.
//!
//! A conflict is never a startup failure: the daemon comes up read-only and
//! names who is in the way. These cases prove the claim and the refusal are the
//! same fact seen from two sides, and that every blocked capability reaches the
//! client with a reason an operator can act on.

mod common;

use kori_core::capability::CapabilityId;
use kori_core::ipc::AccessMode;
use kori_core::{KRAKEN_BASE, RGB_CONTROLLER};
use kori_daemon::state::Daemon;
use kori_daemon::{LcdBackend, RgbBackend};
use kori_hardware_linux::SysfsRoot;

use common::{FAST_INTERVAL, Harness};

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
fn read_only_mode_names_the_conflict() {
    let harness = Harness::start_read_only("conflict-detail");
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
    assert!(!rgb.blocked.is_empty());
    assert!(rgb.blocked.iter().all(|blocked| !blocked.reason.is_empty()));

    let kraken = status
        .devices
        .iter()
        .find(|device| device.id == KRAKEN_BASE)
        .unwrap();
    assert!(!kraken.blocked.is_empty());
    assert!(
        kraken
            .blocked
            .iter()
            .all(|blocked| !blocked.reason.is_empty())
    );
}
