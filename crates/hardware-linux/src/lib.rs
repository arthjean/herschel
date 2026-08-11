// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Read-only Linux hardware discovery for allowlisted NZXT devices.
//!
//! This crate answers one question: what does this machine actually expose?
//! It reads sysfs, resolves `hwmon` instances to their USB device and tests
//! permissions without opening a writable descriptor. Writing is the daemon's
//! job, and it may only write what a capability record marks writable.

pub mod control;
pub mod gpu;
pub mod hid;
pub mod hwmon;
pub mod lcd;
pub mod probe;
pub mod rgb;
pub mod sensors;
pub mod sysfs;
pub mod usb;
pub mod usbfs;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use control::CoolingControl;
// The transport is exported from the module that owns it. It used to reach the
// root through `rgb`, which left the same types with two spellings and let a
// reader believe the two devices spoke over different code.
pub use hid::{HidError, HidTransport, Hidraw};
pub use hwmon::KrakenHwmon;
pub use probe::probe;
pub use rgb::RgbError;
pub use sysfs::SysfsRoot;
