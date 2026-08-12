// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! USB enumeration through sysfs.
//!
//! sysfs already exposes every identity field the capability record needs, so
//! the probe needs neither libusb nor a writable descriptor.

use std::path::{Path, PathBuf};

use kori_core::DeviceId;
use kori_core::capability::{Evidenced, UsbEndpoint, UsbIdentity, UsbInterface};

use crate::sysfs::{
    SysfsRoot, bound_driver, read_attribute, read_hex_u8, read_hex_u16, sorted_entries,
};

/// A USB device found on the bus, before the allowlist is consulted.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub id: DeviceId,
    pub path: PathBuf,
}

/// Every USB device sysfs exposes, sorted by bus path.
///
/// Entries without `idVendor` are hubs, interfaces or ports and are skipped.
pub fn enumerate(root: &SysfsRoot) -> Vec<DiscoveredDevice> {
    sorted_entries(&root.usb_devices())
        .into_iter()
        .filter_map(|path| {
            let vendor = read_hex_u16(&path.join("idVendor"))?;
            let product = read_hex_u16(&path.join("idProduct"))?;
            Some(DiscoveredDevice {
                id: DeviceId::new(vendor, product),
                path,
            })
        })
        .collect()
}

/// Build the identity record of a device directory.
pub fn identity(id: DeviceId, path: &Path) -> UsbIdentity {
    let source = path.display().to_string();
    let field = |name: &str| match read_attribute(&path.join(name)) {
        Some(value) => Evidenced::known(value, format!("{source}/{name}")),
        None => Evidenced::unknown(
            format!("{name} is not exposed by the device"),
            format!("{source}/{name}"),
        ),
    };

    UsbIdentity {
        id,
        manufacturer: field("manufacturer"),
        product: field("product"),
        serial: field("serial"),
        // USB has no firmware attribute. bcdDevice is the device release
        // number, which is the closest published value, and it is labeled as
        // such rather than presented as a firmware version.
        firmware: field("bcdDevice"),
        sysfs_path: source,
    }
}

/// Interface directories of a device, in name order.
///
/// They are named `<device>:<config>.<interface>`, where the configuration is
/// whichever one the kernel selected. Found by listing rather than by guessing:
/// `bConfigurationValue` is a byte, and a scan over an invented range that
/// missed the real configuration would report no interfaces at all.
///
/// One walk for every caller, and that is the point rather than a convenience.
/// [`interface_driver`] is what [`crate::usbfs::Usbfs::claim`] refuses on, so a
/// second copy of this filter drifting out of step with this one is precisely
/// how an interface a kernel driver owns would start looking free.
pub fn interface_dirs(device: &Path) -> Vec<PathBuf> {
    let Some(device_name) = device.file_name().and_then(|name| name.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{device_name}:");

    sorted_entries(device)
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect()
}

/// The kernel driver bound to one interface of a device, when there is one.
///
/// The interface number is read from `bInterfaceNumber`, the same evidence
/// [`interfaces`] records, rather than parsed back out of the directory name.
///
/// This lives here, beside the other sysfs walks, rather than in the module
/// that acts on the answer: `usbfs` claims interfaces, it does not enumerate
/// them, and the one refusal that protects the thermal path must not rest on a
/// private second reading of the same directories.
pub fn interface_driver(device: &Path, interface: u8) -> Option<String> {
    interface_dirs(device)
        .into_iter()
        .find(|entry| read_hex_u8(&entry.join("bInterfaceNumber")) == Some(interface))
        .and_then(|entry| bound_driver(&entry))
}

/// Interfaces of a device, with the kernel driver bound to each.
pub fn interfaces(path: &Path) -> Vec<UsbInterface> {
    let mut found: Vec<UsbInterface> = interface_dirs(path)
        .into_iter()
        .filter_map(|entry| {
            let number = read_hex_u8(&entry.join("bInterfaceNumber"))?;
            let source = entry.display().to_string();
            Some(UsbInterface {
                number,
                class: read_hex_u8(&entry.join("bInterfaceClass")).unwrap_or(0xff),
                subclass: read_hex_u8(&entry.join("bInterfaceSubClass")).unwrap_or(0),
                protocol: read_hex_u8(&entry.join("bInterfaceProtocol")).unwrap_or(0),
                driver: match bound_driver(&entry) {
                    Some(driver) => Evidenced::known(driver, format!("{source}/driver")),
                    None => Evidenced::unknown(
                        "no kernel driver is bound to this interface",
                        format!("{source}/driver"),
                    ),
                },
                endpoints: endpoints(&entry),
            })
        })
        .collect();

    found.sort_by_key(|interface| interface.number);
    found
}

/// Endpoints published by one interface, in address order.
///
/// Endpoint directories are named `ep_XX`. `direction` and `type` are taken as
/// the kernel already decoded them, so the record cannot contradict the kernel
/// about the same endpoint. A directory missing `bEndpointAddress` is not an
/// endpoint and is skipped rather than recorded with a guessed address.
pub fn endpoints(interface: &Path) -> Vec<UsbEndpoint> {
    let mut found: Vec<UsbEndpoint> = sorted_entries(interface)
        .into_iter()
        .filter(|entry| {
            entry
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ep_"))
        })
        .filter_map(|entry| {
            Some(UsbEndpoint {
                address: read_hex_u8(&entry.join("bEndpointAddress"))?,
                direction: read_attribute(&entry.join("direction")).unwrap_or_default(),
                transfer: read_attribute(&entry.join("type")).unwrap_or_default(),
                max_packet_size: read_hex_u16(&entry.join("wMaxPacketSize")).unwrap_or(0),
                interval: read_hex_u8(&entry.join("bInterval")).unwrap_or(0),
            })
        })
        .collect();

    found.sort_by_key(|endpoint| endpoint.address);
    found
}

/// The `hidraw` node the kernel created for a USB device, when it created one.
///
/// The chain is device -> interface -> HID device -> `hidraw/hidrawN`. Walking
/// it is what links an allowlisted VID/PID to the node the transport opens, so
/// no code has to hard-code a device number that changes on every reconnect.
pub fn hidraw_node(device: &Path) -> Option<PathBuf> {
    interface_dirs(device)
        .into_iter()
        .flat_map(|interface| sorted_entries(&interface))
        .flat_map(|entry| sorted_entries(&entry.join("hidraw")))
        .find_map(|node| {
            let name = node.file_name()?.to_str()?;
            name.starts_with("hidraw")
                .then(|| PathBuf::from("/dev").join(name))
        })
}

/// Walk up from `start` to the USB device directory that owns it.
///
/// `hwmon` instances hang off a HID device, which hangs off a USB interface,
/// which hangs off the USB device. Walking up until `idVendor` appears links
/// the two without hard-coding that depth.
pub fn owning_device(start: &Path) -> Option<(DeviceId, PathBuf)> {
    let mut current = std::fs::canonicalize(start).ok()?;
    loop {
        if let (Some(vendor), Some(product)) = (
            read_hex_u16(&current.join("idVendor")),
            read_hex_u16(&current.join("idProduct")),
        ) {
            return Some((DeviceId::new(vendor, product), current));
        }
        current = current.parent()?.to_path_buf();
        if current.as_os_str().is_empty() || current == Path::new("/") {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeSysfs;
    use kori_core::KRAKEN_BASE;

    #[test]
    fn enumeration_skips_entries_without_identifiers() {
        let fake = FakeSysfs::new("usb-enumerate");
        fake.add_kraken();
        fake.add_rgb_controller();
        fake.add_unrelated_device();
        // A port directory carries no idVendor and must be ignored.
        fake.add_bare_directory("bus/usb/devices/usb1");

        let found = enumerate(&fake.root());
        let ids: Vec<_> = found.iter().map(|d| d.id.to_string()).collect();
        assert_eq!(ids, vec!["1e71:2021", "1e71:300e", "046d:c52b"]);
    }

    #[test]
    fn identity_marks_absent_fields_unknown_with_evidence() {
        let fake = FakeSysfs::new("usb-identity");
        let path = fake.add_kraken();
        fake.remove_attribute(&path, "serial");

        let identity = identity(KRAKEN_BASE, &path);
        assert_eq!(
            identity.product.value().map(String::as_str),
            Some("NZXT Kraken Base")
        );
        match &identity.serial {
            Evidenced::Unknown { reason, source } => {
                assert!(reason.contains("serial"), "{reason}");
                assert!(source.ends_with("/serial"), "{source}");
            }
            other => panic!("expected unknown serial, got {other:?}"),
        }
    }

    #[test]
    fn interfaces_report_the_bound_driver_or_its_absence() {
        let fake = FakeSysfs::new("usb-interfaces");
        let path = fake.add_kraken();

        let found = interfaces(&path);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].number, 0);
        assert_eq!(found[0].class, 0xff);
        assert!(
            !found[0].driver.is_known(),
            "vendor interface must be unbound"
        );
        assert_eq!(found[1].number, 1);
        assert_eq!(found[1].class, 0x03);
        assert_eq!(found[1].driver.value().map(String::as_str), Some("usbhid"));
    }

    #[test]
    fn the_controller_resolves_to_the_node_the_kernel_created() {
        let fake = FakeSysfs::new("usb-hidraw");
        let device = fake.add_rgb_controller();

        assert_eq!(hidraw_node(&device), Some(PathBuf::from("/dev/hidraw12")));

        // Both devices publish one. `kraken2023` starts with
        // `HID_CONNECT_HIDRAW`, so the Kraken's thermal interface carries a
        // node beside the driver, and each device resolves to its own rather
        // than to the first node the walk finds.
        let kraken = fake.add_kraken();
        assert_eq!(hidraw_node(&kraken), Some(PathBuf::from("/dev/hidraw10")));

        // A device whose kernel created no node resolves to nothing rather
        // than borrowing another device's.
        fake.remove_rgb_hidraw();
        assert_eq!(hidraw_node(&device), None);
        assert_eq!(hidraw_node(&kraken), Some(PathBuf::from("/dev/hidraw10")));

        fake.remove_kraken_hidraw();
        assert_eq!(hidraw_node(&kraken), None);
    }

    #[test]
    fn endpoints_are_read_in_address_order_with_the_kernels_own_decoding() {
        let fake = FakeSysfs::new("usb-endpoints");
        let device = fake.add_rgb_controller();

        let interfaces = interfaces(&device);
        assert_eq!(interfaces.len(), 1);
        let endpoints = &interfaces[0].endpoints;
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].address, 0x02);
        assert_eq!(endpoints[0].direction, "out");
        assert_eq!(endpoints[1].address, 0x81);
        assert_eq!(endpoints[1].direction, "in");
        assert!(
            endpoints
                .iter()
                .all(|endpoint| endpoint.max_packet_size == 64 && endpoint.interval == 1)
        );
    }

    #[test]
    fn an_interface_is_found_whatever_configuration_the_kernel_selected() {
        // The configuration number is part of the directory name and is the
        // kernel's choice. Resolving it by scanning an invented range would
        // report no driver for an interface that has one, and `Usbfs::claim`
        // would then take an interface a driver owns: the single mistake the
        // claim path exists to make impossible.
        let fake = FakeSysfs::new("usb-configuration");
        let device = fake.add_bare_directory("bus/usb/devices/3-2");
        fake.add_interface_in_configuration(&device, 9, 0, 0xff, None);
        fake.add_interface_in_configuration(&device, 9, 1, 0x03, Some("usbhid"));

        assert_eq!(interface_driver(&device, 1).as_deref(), Some("usbhid"));
        assert_eq!(interface_driver(&device, 0), None);
        assert_eq!(interface_driver(&device, 2), None);
    }

    #[test]
    fn every_walk_over_a_device_sees_the_same_interfaces() {
        // The driver check, the interface record and the node lookup used to
        // carry a copy of this filter each. They agree here because they now
        // ask the same function, and that is what keeps a claim from being
        // offered an interface the record knows a driver owns.
        let fake = FakeSysfs::new("usb-one-walk");
        let device = fake.add_kraken();

        let dirs = interface_dirs(&device);
        assert_eq!(dirs.len(), 2, "the Kraken publishes two interfaces");
        let recorded = interfaces(&device);
        assert_eq!(recorded.len(), dirs.len());
        for interface in &recorded {
            assert_eq!(
                interface.driver.value().map(String::as_str),
                interface_driver(&device, interface.number).as_deref(),
                "interface {} disagrees between the record and the claim check",
                interface.number
            );
        }

        // A directory that is not an interface of this device is never walked,
        // whichever door asks.
        fake.add_bare_directory("bus/usb/devices/1-9/power");
        assert_eq!(interface_dirs(&device).len(), 2);
    }

    #[test]
    fn hwmon_instances_resolve_to_their_usb_device() {
        let fake = FakeSysfs::new("usb-owner");
        let device = fake.add_kraken();
        let hid = fake.add_kraken_hwmon();

        let (id, path) = owning_device(&hid).unwrap();
        assert_eq!(id, KRAKEN_BASE);
        assert_eq!(
            std::fs::canonicalize(path).unwrap(),
            std::fs::canonicalize(device).unwrap()
        );
    }
}
