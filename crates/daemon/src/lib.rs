// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! The per-user daemon that owns every write to allowlisted NZXT hardware.
//!
//! It runs without root, listens on one Unix socket inside the user's runtime
//! directory, and holds an exclusive lock per device. When ownership or
//! permission is uncertain it stays up in read-only mode rather than forcing
//! access.

pub mod config;
pub mod ownership;
pub mod paths;
pub mod server;
pub mod state;

use std::time::{SystemTime, UNIX_EPOCH};

pub use paths::Paths;
pub use server::Server;
pub use state::{DAEMON_VERSION, Daemon};

/// Milliseconds since the Unix epoch.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
