// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Types shared by the Kori daemon and the GPUI client.
//!
//! Nothing in this crate touches hardware. It defines the vocabulary both
//! processes agree on: what a device can do, what a reading is worth, what a
//! profile contains and what may travel over the local Unix socket.

pub mod capability;
pub mod client;
pub mod diagnostics;
pub mod display;
pub mod ipc;
pub mod lighting;
pub mod profile;
pub mod telemetry;

/// The product's own name.
///
/// Original branding, never NZXT's. It lives here rather than in the
/// client's theme because the daemon draws it onto the panel too, and the
/// wordmark on the hardware and the wordmark in the window must be one string.
pub const PRODUCT_NAME: &str = "Kori";

/// Vendor and product identifiers of a USB device.
///
/// Formatted as lowercase hexadecimal `vvvv:pppp` to match `lsusb` output.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DeviceId {
    pub vendor: u16,
    pub product: u16,
}

impl DeviceId {
    pub const fn new(vendor: u16, product: u16) -> Self {
        Self { vendor, product }
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vendor, self.product)
    }
}

/// The two devices this version is allowed to touch.
///
/// Detection and writes stay inside this allowlist until another device is
/// validated against real hardware.
pub const KRAKEN_BASE: DeviceId = DeviceId::new(0x1e71, 0x300e);
pub const RGB_CONTROLLER: DeviceId = DeviceId::new(0x1e71, 0x2021);

/// Every device id the product accepts, in probe order.
pub const ALLOWLIST: [DeviceId; 2] = [KRAKEN_BASE, RGB_CONTROLLER];

/// Whether a discovered device may be opened at all.
pub fn is_allowlisted(id: DeviceId) -> bool {
    ALLOWLIST.contains(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_renders_like_lsusb() {
        assert_eq!(KRAKEN_BASE.to_string(), "1e71:300e");
        assert_eq!(RGB_CONTROLLER.to_string(), "1e71:2021");
    }

    #[test]
    fn allowlist_rejects_unknown_devices() {
        assert!(is_allowlisted(KRAKEN_BASE));
        assert!(is_allowlisted(RGB_CONTROLLER));
        assert!(!is_allowlisted(DeviceId::new(0x1e71, 0x2007)));
        assert!(!is_allowlisted(DeviceId::new(0x046d, 0xc52b)));
    }
}
