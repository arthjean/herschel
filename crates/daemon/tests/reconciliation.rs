// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What the daemon settles on its own clock.
//!
//! Neither of these opens a client until after the fact. That is the point: the
//! reconciliation guards writes this daemon made, and a write does not stop
//! needing a guard because nobody is looking at it. Both used to hang off a
//! telemetry request, so a machine with the window closed never noticed.

mod common;

use std::time::Duration;

use kori_core::display::DisplayMode;

use common::{FAST_INTERVAL, Harness, preset};

/// The panel sits on the same device as the thermal path, so a Kraken that
/// goes away takes the panel with it. The record of what the panel is showing
/// has to go too: a panel that comes back has swapped nothing, so a preserved
/// record would let the next Apply deduplicate against a picture the glass no
/// longer holds, and would leave the link believing it is still primed when the
/// device it was primed against is gone.
///
/// The client here only watches. Nothing it sends causes the record to be
/// dropped: the daemon settles that on its own clock, which is what makes the
/// same thing happen with no window open at all.
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

    let status = harness.wait_for_status(&mut client, Duration::from_secs(5), |status| {
        status.display.committed.is_none()
    });
    assert!(
        status.display.committed.is_none(),
        "the panel record must not outlive the device it describes"
    );
    assert!(
        !client.telemetry().unwrap().kraken.present,
        "the device is gone"
    );
    // The preset the operator asked for is deliberately kept, which is what
    // lets the panel resume on its own once the device answers again. Only the
    // claim about what the glass currently holds is dropped. The fixture's link
    // is in memory and never notices the device leave, so `streaming` still
    // reads true here; on real hardware the transport would be gone.
}

/// The daemon settles what the hardware says with nothing connected to it.
///
/// The reconciliation used to hang off the telemetry request, so a machine with
/// the window closed never noticed a device leave and never checked that the
/// curve it wrote is the one the firmware is running. Both are guards on writes
/// this daemon made, and a write does not stop needing a guard because nobody is
/// looking. This opens no client until after the fact, so the only thing that
/// can have moved the state is the daemon's own clock.
#[test]
fn the_daemon_notices_a_device_leaving_with_no_client_connected() {
    let harness = Harness::start_lcd("lcd-headless", "2.0.0");
    {
        let mut client = harness.client();
        client
            .apply_display(preset(DisplayMode::DualInfographic))
            .expect("a validated firmware accepts a frame");
        assert!(client.status().unwrap().display.committed.is_some());
    }

    // Nothing is connected from here until the assertion below.
    for attribute in ["temp1_input", "fan1_input", "fan2_input", "name"] {
        harness
            .fake
            .remove_attribute(&harness.hwmon_path(), attribute);
    }
    std::thread::sleep(FAST_INTERVAL * 20);

    let mut client = harness.client();
    assert!(
        client.status().unwrap().display.committed.is_none(),
        "the daemon must settle this on its own clock, not when a client asks"
    );
}
