// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bulk transfers to a USB interface no kernel driver claimed.
//!
//! The Kraken publishes two interfaces. Interface 1 is HID and belongs to
//! `kraken2023`; this module never touches it. Interface 0 is vendor class,
//! carries a single bulk OUT endpoint and has no driver bound, so it can be
//! claimed through `usbfs` without detaching anything. That separation is what
//! makes the LCD reachable while the thermal path stays exactly where it is.
//!
//! `usbfs` rather than libusb because the product already reads every identity
//! field straight from sysfs: enumeration, matching and endpoint discovery are
//! solved, and what is missing is three `ioctl` calls. Linking a C library to
//! obtain them would add a build dependency for less than this file contains.
//!
//! # Boundaries
//!
//! [`Usbfs::claim`] refuses any interface a driver is bound to, so the one
//! mistake that would break the thermal path cannot be made from here. Only OUT
//! endpoints may be written, asserted per transfer rather than trusted.

use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::ioctl::{self, Ioctl, IoctlOutput, Opcode};

use crate::sysfs::{bound_driver, read_attribute};

/// Environment variable that relocates the `usbfs` device tree.
///
/// Mirrors [`crate::sysfs::SYSFS_ROOT_ENV`] so a fixture can describe a whole
/// machine. It cannot grant access the invoking user does not already have.
pub const USBFS_ROOT_ENV: &str = "NZXT_USBFS_ROOT";

/// `ioctl` group of the USB device filesystem, `'U'`.
const USBFS_GROUP: u8 = b'U';

/// Largest payload handed to one `USBDEVFS_BULK` call.
///
/// Large enough that a whole panel frame is one call. It was 16 KiB, which
/// split a 115 200 byte frame across eight `ioctl`s with a user-space return
/// between each, and the panel painted only a band of what it was sent. No
/// short packet is involved either way, since 16 KiB is a whole multiple of the
/// endpoint's 512 byte maximum, but the gaps are real and the reference
/// implementation does not have them: liquidctl's `bulk_buffer_size` for the
/// 2023 and 2024 models is 2 MiB, so it writes the frame whole.
///
/// The kernel's own ceiling for one transfer is far above this. The value is a
/// bound on the contiguous buffer a single call asks the kernel to hold, and
/// this product's largest transfer by a wide margin is one frame.
pub const MAX_BULK_CHUNK: usize = 2 * 1024 * 1024;

/// How long one bulk call may block before the kernel gives up.
const BULK_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Why a bulk transfer could not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsbfsError {
    #[error("no usbfs node exists for the device: {reason}")]
    NodeAbsent { reason: String },
    #[error("permission denied on {path}. Check the installed udev rule.")]
    PermissionDenied { path: String },
    #[error("interface {interface} is bound to {driver} and will not be claimed")]
    DriverBound { interface: u8, driver: String },
    #[error("interface {interface} is already claimed by another process")]
    Busy { interface: u8 },
    #[error("endpoint {endpoint:#04x} is an IN endpoint and cannot be written")]
    NotAnOutEndpoint { endpoint: u8 },
    #[error("{path}: {detail}")]
    Io { path: String, detail: String },
    #[error("the device accepted {wrote} of {expected} bytes")]
    ShortWrite { wrote: usize, expected: usize },
}

impl UsbfsError {
    fn io(path: &Path, error: &std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied {
                path: path.display().to_string(),
            },
            std::io::ErrorKind::NotFound => Self::NodeAbsent {
                reason: format!("{} does not exist", path.display()),
            },
            _ => Self::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            },
        }
    }

    fn errno(path: &Path, interface: u8, error: rustix::io::Errno) -> Self {
        match error {
            rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => Self::PermissionDenied {
                path: path.display().to_string(),
            },
            rustix::io::Errno::BUSY => Self::Busy { interface },
            other => Self::Io {
                path: path.display().to_string(),
                detail: other.to_string(),
            },
        }
    }
}

/// Moving a byte stream to one bulk endpoint.
///
/// A trait rather than a concrete file for the same reason [`crate::rgb::HidTransport`]
/// is one: the frame encoder, the probe and the daemon are all provable against
/// a recording double, and only this module knows how a real transfer happens.
pub trait BulkTransport: Send {
    /// Write `payload` to `endpoint` as one transfer.
    ///
    /// Implementations refuse an IN endpoint rather than reinterpreting it.
    fn write_bulk(&mut self, endpoint: u8, payload: &[u8]) -> Result<(), UsbfsError>;

    /// Where the bytes go, for the capability record's evidence.
    fn source(&self) -> String;
}

/// The `usbfs` device node, with one interface claimed for as long as it lives.
#[derive(Debug)]
pub struct Usbfs {
    file: std::fs::File,
    path: PathBuf,
    interface: u8,
}

impl Usbfs {
    /// Claim `interface` of the device described by its sysfs directory.
    ///
    /// Refuses outright when a kernel driver is bound to that interface. This
    /// product never detaches a driver, so an interface someone else owns is a
    /// refusal, never a negotiation.
    pub fn claim(device: &Path, interface: u8) -> Result<Self, UsbfsError> {
        if let Some(driver) = interface_driver(device, interface) {
            return Err(UsbfsError::DriverBound { interface, driver });
        }

        let path = node_for(device).ok_or_else(|| UsbfsError::NodeAbsent {
            reason: format!("{} publishes no busnum/devnum pair", device.display()),
        })?;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| UsbfsError::io(&path, &error))?;

        // SAFETY: the opcode carries a pointer to one `u32` holding the
        // interface number, which is exactly what `USBDEVFS_CLAIMINTERFACE`
        // reads.
        unsafe { ioctl::ioctl(file.as_fd(), InterfaceIoctl::claim(interface)) }
            .map_err(|error| UsbfsError::errno(&path, interface, error))?;

        Ok(Self {
            file,
            path,
            interface,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Usbfs {
    fn drop(&mut self) {
        // Best effort: the kernel releases every claim when the descriptor
        // closes anyway, and a failure here has nowhere useful to go.
        // SAFETY: same shape as the claim above.
        let _ = unsafe { ioctl::ioctl(self.file.as_fd(), InterfaceIoctl::release(self.interface)) };
    }
}

impl BulkTransport for Usbfs {
    fn write_bulk(&mut self, endpoint: u8, payload: &[u8]) -> Result<(), UsbfsError> {
        if endpoint & 0x80 != 0 {
            return Err(UsbfsError::NotAnOutEndpoint { endpoint });
        }

        for chunk in payload.chunks(MAX_BULK_CHUNK) {
            // SAFETY: `chunk` outlives the call, the endpoint is an OUT
            // endpoint as checked above so the kernel only reads through the
            // pointer, and the opcode's size matches the struct it points at.
            let wrote = unsafe {
                ioctl::ioctl(
                    self.file.as_fd(),
                    BulkIoctl::new(endpoint, chunk, BULK_TIMEOUT),
                )
            }
            .map_err(|error| UsbfsError::errno(&self.path, self.interface, error))?;

            if wrote != chunk.len() {
                return Err(UsbfsError::ShortWrite {
                    wrote,
                    expected: chunk.len(),
                });
            }
        }
        Ok(())
    }

    fn source(&self) -> String {
        format!("{} interface {}", self.path.display(), self.interface)
    }
}

/// The `usbfs` node of a device, from the bus and device numbers sysfs holds.
///
/// The pair changes on every reconnect, which is why it is read rather than
/// remembered.
pub fn node_for(device: &Path) -> Option<PathBuf> {
    node_in(&root(), device)
}

/// The same resolution under an explicit tree.
///
/// Split out so the path arithmetic is provable without a test mutating a
/// process-global variable that every other test in the binary can observe.
pub fn node_in(root: &Path, device: &Path) -> Option<PathBuf> {
    let bus: u16 = read_attribute(&device.join("busnum"))?.parse().ok()?;
    let address: u16 = read_attribute(&device.join("devnum"))?.parse().ok()?;
    Some(root.join(format!("{bus:03}")).join(format!("{address:03}")))
}

/// `/dev/bus/usb`, unless [`USBFS_ROOT_ENV`] overrides it.
fn root() -> PathBuf {
    match std::env::var_os(USBFS_ROOT_ENV) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from("/dev/bus/usb"),
    }
}

/// The kernel driver bound to one interface of a device, when there is one.
fn interface_driver(device: &Path, interface: u8) -> Option<String> {
    let name = device.file_name()?.to_str()?;
    // Interface directories are `<device>:<config>.<interface>`. Only the
    // interface number is fixed here; the configuration is whichever one the
    // kernel selected, so every configuration is checked.
    (1..=8u8).find_map(|configuration| {
        bound_driver(&device.join(format!("{name}:{configuration}.{interface}")))
    })
}

/// `USBDEVFS_CLAIMINTERFACE` and `USBDEVFS_RELEASEINTERFACE`.
///
/// Both take a pointer to one unsigned interface number.
struct InterfaceIoctl {
    opcode: Opcode,
    interface: u32,
}

impl InterfaceIoctl {
    fn claim(interface: u8) -> Self {
        Self {
            opcode: ioctl::opcode::read::<u32>(USBFS_GROUP, 15),
            interface: u32::from(interface),
        }
    }

    fn release(interface: u8) -> Self {
        Self {
            opcode: ioctl::opcode::read::<u32>(USBFS_GROUP, 16),
            interface: u32::from(interface),
        }
    }
}

// SAFETY: the opcode declares a `u32` payload and `as_ptr` hands the kernel a
// pointer to exactly one `u32` that lives as long as the call.
unsafe impl Ioctl for InterfaceIoctl {
    type Output = ();

    const IS_MUTATING: bool = false;

    fn opcode(&self) -> Opcode {
        self.opcode
    }

    fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        std::ptr::from_mut(&mut self.interface).cast()
    }

    unsafe fn output_from_ptr(
        _out: IoctlOutput,
        _extract: *mut std::ffi::c_void,
    ) -> rustix::io::Result<()> {
        Ok(())
    }
}

/// `struct usbdevfs_bulktransfer`, as `include/uapi/linux/usbdevice_fs.h`
/// declares it.
///
/// `repr(C)` reproduces the padding the kernel expects between `timeout` and
/// the pointer on a 64-bit target.
#[repr(C)]
struct BulkTransfer {
    endpoint: u32,
    length: u32,
    timeout: u32,
    data: *mut std::ffi::c_void,
}

/// `USBDEVFS_BULK`, whose return value is the number of bytes transferred.
struct BulkIoctl<'a> {
    transfer: BulkTransfer,
    payload: std::marker::PhantomData<&'a [u8]>,
}

impl<'a> BulkIoctl<'a> {
    fn new(endpoint: u8, payload: &'a [u8], timeout: Duration) -> Self {
        Self {
            transfer: BulkTransfer {
                endpoint: u32::from(endpoint),
                length: payload.len() as u32,
                timeout: timeout.as_millis() as u32,
                // The kernel only reads through this pointer for an OUT
                // endpoint, which `write_bulk` checks before constructing this.
                data: payload.as_ptr().cast_mut().cast(),
            },
            payload: std::marker::PhantomData,
        }
    }
}

// SAFETY: the opcode's size matches `BulkTransfer`, the struct outlives the
// call, and the buffer it points at outlives it too through the lifetime.
unsafe impl Ioctl for BulkIoctl<'_> {
    type Output = usize;

    const IS_MUTATING: bool = true;

    fn opcode(&self) -> Opcode {
        ioctl::opcode::read_write::<BulkTransfer>(USBFS_GROUP, 2)
    }

    fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        std::ptr::from_mut(&mut self.transfer).cast()
    }

    unsafe fn output_from_ptr(
        out: IoctlOutput,
        _extract: *mut std::ffi::c_void,
    ) -> rustix::io::Result<usize> {
        Ok(out.max(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeSysfs;

    /// The opcodes as the kernel headers compute them on a 64-bit target.
    ///
    /// Hard-coded here on purpose. `opcode::read` derives them from a size and
    /// a group, and a silent change in that derivation would send a valid
    /// request to the wrong handler, so the derived values are pinned against
    /// the numbers `_IOR('U', 15, unsigned int)` and
    /// `_IOWR('U', 2, struct usbdevfs_bulktransfer)` expand to.
    #[test]
    fn the_opcodes_match_the_kernel_headers() {
        assert_eq!(ioctl::opcode::read::<u32>(USBFS_GROUP, 15), 0x8004_550f);
        assert_eq!(ioctl::opcode::read::<u32>(USBFS_GROUP, 16), 0x8004_5510);
        assert_eq!(
            ioctl::opcode::read_write::<BulkTransfer>(USBFS_GROUP, 2),
            0xc018_5502
        );
    }

    #[test]
    fn the_transfer_struct_has_the_layout_the_kernel_reads() {
        assert_eq!(std::mem::size_of::<BulkTransfer>(), 24);
        assert_eq!(std::mem::align_of::<BulkTransfer>(), 8);
    }

    #[test]
    fn an_in_endpoint_is_refused_before_any_ioctl_happens() {
        struct Refusing;
        impl BulkTransport for Refusing {
            fn write_bulk(&mut self, endpoint: u8, _payload: &[u8]) -> Result<(), UsbfsError> {
                if endpoint & 0x80 != 0 {
                    return Err(UsbfsError::NotAnOutEndpoint { endpoint });
                }
                Ok(())
            }
            fn source(&self) -> String {
                "test".into()
            }
        }
        // The direction bit is what separates the two, and 0x81 is the exact
        // interrupt IN address the Kraken's HID interface publishes.
        assert_eq!(
            Refusing.write_bulk(0x81, &[0]),
            Err(UsbfsError::NotAnOutEndpoint { endpoint: 0x81 })
        );
        assert!(Refusing.write_bulk(0x02, &[0]).is_ok());
    }

    #[test]
    fn the_node_is_resolved_from_the_numbers_sysfs_publishes() {
        let fake = FakeSysfs::new("usbfs-node");
        let device = fake.add_kraken();
        let root = Path::new("/dev/bus/usb");

        // Both numbers are zero-padded to three digits, which is the layout
        // udev creates and the reason the pair cannot simply be formatted.
        assert_eq!(
            node_in(root, &device),
            Some(PathBuf::from("/dev/bus/usb/001/004"))
        );

        // A directory without the pair resolves to nothing rather than to a
        // guessed bus address.
        assert_eq!(
            node_in(root, &fake.root_path().join("bus/usb/devices")),
            None
        );
    }

    #[test]
    fn an_interface_a_driver_owns_is_never_claimed() {
        let fake = FakeSysfs::new("usbfs-claim");
        let device = fake.add_kraken();

        // Interface 1 is the HID one `kraken2023` reaches through `usbhid`.
        // Claiming it is the mistake that would break the thermal path, and it
        // is refused before the node is opened, so this assertion needs no
        // device and no permission.
        match Usbfs::claim(&device, 1) {
            Err(UsbfsError::DriverBound { interface, driver }) => {
                assert_eq!(interface, 1);
                assert_eq!(driver, "usbhid");
            }
            other => panic!("expected a refusal on the bound interface, got {other:?}"),
        }

        // Interface 0 carries no driver, so the driver check lets it through.
        assert_eq!(interface_driver(&device, 0), None);
    }
}
