#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! The native GPUI control surface.
//!
//! The window is a view over what the daemon reports. It holds no hardware
//! handle, opens no socket other than the daemon's, and makes no network
//! request.

pub mod components;
pub mod link;
pub mod offline;
pub mod preview;
pub mod shell;
pub mod startup;
pub mod theme;
