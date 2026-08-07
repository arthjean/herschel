// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! The native GPUI control surface.
//!
//! The window is a view over what the daemon reports. It holds no hardware
//! handle, opens no socket other than the daemon's, and makes no network
//! request.

pub mod components;
pub mod cooling;
pub mod display;
pub mod feed;
pub mod lighting;
pub mod link;
pub mod metrics;
pub mod offline;
pub mod preview;
pub mod shell;
pub mod startup;
pub mod theme;
