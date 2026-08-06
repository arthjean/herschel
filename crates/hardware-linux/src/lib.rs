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
pub mod hwmon;
pub mod probe;
pub mod sensors;
pub mod sysfs;
pub mod usb;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use control::CoolingControl;
pub use hwmon::KrakenHwmon;
pub use probe::probe;
pub use sysfs::SysfsRoot;
