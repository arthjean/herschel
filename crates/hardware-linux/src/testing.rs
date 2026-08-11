// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

// These modules only ever build fixtures for tests. Failing to build one is a
// broken test, not a runtime condition, so they panic loudly and immediately.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Fixtures standing in for the parts of the machine a test cannot own.
//!
//! Four independent pieces, one per module. [`FakeSysfs`] is a temporary sysfs
//! and `/proc` tree mirroring the development machine, because the probe reads
//! real files through real permission checks and that is the only way to prove
//! its behavior on absent attributes, unbound drivers and read-only nodes.
//! [`FakeController`] and [`FakeKraken`] answer the reports their devices
//! answer, so the encoding, ownership and cadence code is exercised end to end
//! without hardware. The recorders hold what each of them was sent.
//!
//! Every device answer is built by an encoder living beside the decoder that
//! reads it, never at a literal offset restated here. A fixture that described
//! a device at its own offsets would agree with itself forever while the
//! product read somewhere else on real hardware.

mod controller;
mod kraken;
mod recorder;
mod sysfs;

pub use controller::FakeController;
pub use kraken::FakeKraken;
pub use recorder::{BulkRecorder, ReportRecorder};
pub use sysfs::{FakeSysfs, KRAKEN_FIXTURE_SERIAL, RGB_FIXTURE_SERIAL, running_as_root};
