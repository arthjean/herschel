//! Read-only sysfs access.
//!
//! Every function here reads or tests permissions. None opens a file for
//! writing, which is what keeps the probe safe to run against an unknown
//! device.

use std::path::{Path, PathBuf};

use rustix::fs::{Access, access};

/// Environment variable that relocates the sysfs root.
///
/// Used by tests and by anyone running the daemon against a recorded fixture.
/// It cannot grant access the invoking user does not already have.
pub const SYSFS_ROOT_ENV: &str = "NZXT_SYSFS_ROOT";

/// The sysfs tree the probe reads from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsRoot {
    root: PathBuf,
}

impl Default for SysfsRoot {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SysfsRoot {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `/sys`, unless [`SYSFS_ROOT_ENV`] overrides it.
    pub fn from_env() -> Self {
        match std::env::var_os(SYSFS_ROOT_ENV) {
            Some(path) if !path.is_empty() => Self::new(path),
            _ => Self::new("/sys"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn join(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// Directory holding one entry per USB device.
    pub fn usb_devices(&self) -> PathBuf {
        self.join("bus/usb/devices")
    }

    /// Directory holding one entry per `hwmon` instance.
    pub fn hwmon(&self) -> PathBuf {
        self.join("class/hwmon")
    }
}

/// Read a sysfs attribute and trim the trailing newline.
///
/// Returns `None` when the attribute is absent or unreadable, which the caller
/// turns into an explicit unknown rather than a default.
pub fn read_attribute(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let trimmed = text.trim_end_matches(['\n', '\r']).to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Read a hexadecimal attribute such as `idVendor`.
pub fn read_hex_u16(path: &Path) -> Option<u16> {
    u16::from_str_radix(read_attribute(path)?.trim(), 16).ok()
}

/// Read a hexadecimal byte attribute such as `bInterfaceClass`.
pub fn read_hex_u8(path: &Path) -> Option<u8> {
    u8::from_str_radix(read_attribute(path)?.trim(), 16).ok()
}

/// Whether the current user can read the file, without opening it.
pub fn is_readable(path: &Path) -> bool {
    access(path, Access::READ_OK).is_ok()
}

/// Whether the current user can write the file, without opening it.
///
/// `access(2)` answers the question that matters, which is whether a later
/// write would be permitted, and it never creates a writable descriptor.
pub fn is_writable(path: &Path) -> bool {
    access(path, Access::WRITE_OK).is_ok()
}

/// Name of the kernel driver bound to `path`, from its `driver` symlink.
pub fn bound_driver(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path.join("driver")).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

/// Directory entries of `path`, sorted by name for stable output.
pub fn sorted_entries(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nzxt-sysfs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn attributes_are_trimmed_and_absent_files_stay_unknown() {
        let dir = temp_dir("attrs");
        fs::write(dir.join("idVendor"), "1e71\n").unwrap();
        fs::write(dir.join("blank"), "\n").unwrap();

        assert_eq!(
            read_attribute(&dir.join("idVendor")).as_deref(),
            Some("1e71")
        );
        assert_eq!(read_hex_u16(&dir.join("idVendor")), Some(0x1e71));
        assert_eq!(read_attribute(&dir.join("blank")), None);
        assert_eq!(read_attribute(&dir.join("missing")), None);
        assert_eq!(read_hex_u16(&dir.join("missing")), None);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_permission_is_tested_without_opening_the_file() {
        let dir = temp_dir("perms");
        let read_only = dir.join("temp1_input");
        let writable = dir.join("pwm1");
        fs::write(&read_only, "27900\n").unwrap();
        fs::write(&writable, "255\n").unwrap();
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&writable, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(is_readable(&read_only));
        assert!(!is_writable(&read_only));
        assert!(is_writable(&writable));
        assert!(!is_writable(&dir.join("missing")));

        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn root_prefers_the_environment_override() {
        let root = SysfsRoot::new("/tmp/fake-sys");
        assert_eq!(
            root.usb_devices(),
            Path::new("/tmp/fake-sys/bus/usb/devices")
        );
        assert_eq!(root.hwmon(), Path::new("/tmp/fake-sys/class/hwmon"));
    }
}
