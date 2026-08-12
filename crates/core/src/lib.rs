// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Types shared by the Kori daemon and the GPUI client.
//!
//! Nothing in this crate touches hardware. It defines the vocabulary both
//! processes agree on: what a device can do, what a reading is worth, what a
//! profile contains and what may travel over the local Unix socket.

/// Count the variants handed to [`wire_enum`], for the length of its `ALL`.
///
/// Written out rather than reached for through a const `len()` so the array's
/// length is produced by the same expansion that produces its elements.
macro_rules! count_wire_variants {
    () => { 0usize };
    ($head:ident $($tail:ident)*) => { 1usize + count_wire_variants!($($tail)*) };
}

/// Declare an enum that travels under a stable key and shows a label.
///
/// Five enums in this crate are read from a select control, written to a socket
/// frame and stored in a configuration file, and each needs the same three
/// things: the list of its variants, the key it travels under, and the words a
/// control puts on screen.
///
/// Written by hand, that was three lists per enum plus a fourth held by serde's
/// `rename_all`, and nothing in the language made the four agree. A test existed
/// for exactly that: it proved `key()` returned the string serde writes. A test
/// whose only job is to prove two copies of a list match is a request to stop
/// keeping two copies, so the key is now declared once and used twice, as the
/// `#[serde(rename)]` and as what `key()` returns. They cannot drift because
/// they are the same token.
///
/// What the assertion in [`keys`] still covers is what this cannot: that no two
/// variants claim one key, and that an unknown key resolves to nothing rather
/// than to a default.
macro_rules! wire_enum {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $key:literal, $label:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $key)]
                $variant,
            )+
        }

        impl $name {
            /// Every variant, in the order they are declared and offered.
            pub const ALL: [Self; count_wire_variants!($($variant)+)] =
                [$(Self::$variant),+];

            /// The stable identifier this variant travels and is stored under.
            ///
            /// The same string serde writes, because it is the same token.
            pub fn key(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)+
                }
            }

            /// The words a control puts on screen for this variant.
            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                }
            }

            /// The variant a stable key names, or nothing at all.
            pub fn from_key(key: &str) -> Option<Self> {
                match key {
                    $($key => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

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

/// Assertions shared by every enum that publishes a stable key per variant.
#[cfg(test)]
pub(crate) mod keys {
    /// Prove that no two variants claim one key, that every key round-trips,
    /// and that an unknown key resolves to nothing.
    ///
    /// The agreement between a key and the string serde writes is no longer
    /// asserted here, because [`wire_enum`] declares it once and uses it as
    /// both. What remains are the two things that expansion cannot rule out.
    /// Two variants given the same key literal would compile: the second arm of
    /// `from_key` would simply never be reached, and a control offering the
    /// second variant would silently activate the first. That is what the
    /// uniqueness check is for.
    ///
    /// `all` is still a list this cannot check for completeness: a variant left
    /// out of it is invisible to this assertion exactly as it is to `from_key`.
    /// Declaring the enum through the macro is what keeps the two in step.
    pub(crate) fn assert_keys_are_the_serde_names<T>(
        all: &[T],
        key: impl Fn(T) -> &'static str,
        from_key: impl Fn(&str) -> Option<T>,
    ) where
        T: Copy + PartialEq + std::fmt::Debug + serde::Serialize,
    {
        assert!(!all.is_empty(), "an enum with no listed variant");

        let mut seen: Vec<&'static str> = Vec::with_capacity(all.len());
        for &variant in all {
            let key = key(variant);
            assert_eq!(
                serde_json::to_value(variant).unwrap(),
                serde_json::Value::String(key.to_string()),
                "{variant:?} keys as {key} but serializes differently"
            );
            assert_eq!(
                from_key(key),
                Some(variant),
                "{variant:?} does not round-trip through its own key"
            );
            assert!(!seen.contains(&key), "{key} is claimed by two variants");
            seen.push(key);
        }

        assert_eq!(
            from_key("a key no variant will ever claim"),
            None,
            "an unknown key must resolve to nothing rather than to a default"
        );
    }
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
