//! One writer per device, and honest reporting when that cannot be guaranteed.
//!
//! Ownership is an advisory `flock` on a per-device file plus an inspection of
//! which processes already hold the device's HID nodes. Neither forces access
//! away from another program: a conflict downgrades this application to
//! read-only, it never detaches a driver or kills a holder.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use nzxt_core::DeviceId;
use nzxt_core::ipc::OwnershipConflict;
use rustix::fs::{FlockOperation, flock};

use crate::paths::Paths;

/// An acquired per-device lock, released when dropped.
#[derive(Debug)]
pub struct DeviceLock {
    device: DeviceId,
    path: PathBuf,
    // Held for its lifetime: closing the descriptor releases the lock.
    _file: File,
}

impl DeviceLock {
    pub fn device(&self) -> DeviceId {
        self.device
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Try to become the single writer for `device`.
///
/// Returns the conflict instead of an error when another holder exists, so the
/// caller can report it rather than retry.
pub fn acquire(paths: &Paths, device: DeviceId) -> Result<DeviceLock, OwnershipConflict> {
    let path = paths.device_lock(device);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|error| OwnershipConflict {
            device: Some(device),
            resource: path.display().to_string(),
            detail: format!("Lock file could not be opened: {error}"),
        })?;

    if let Err(error) = flock(&file, FlockOperation::NonBlockingLockExclusive) {
        let holder = std::fs::read_to_string(&path).unwrap_or_default();
        let holder = holder.trim();
        let detail = if holder.is_empty() {
            format!("Another process holds the device lock ({error}).")
        } else {
            format!("Another instance holds the device lock (pid {holder}).")
        };
        return Err(OwnershipConflict {
            device: Some(device),
            resource: path.display().to_string(),
            detail,
        });
    }

    // Record the owner so a later conflict can name it.
    let mut file = file;
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    let _ = file.flush();

    Ok(DeviceLock {
        device,
        path,
        _file: file,
    })
}

/// HID device nodes belonging to `sysfs_path`.
///
/// The Kraken exposes one `hidraw` node through the interface `kraken2023`
/// binds to. Another program holding it is the observable form of a competing
/// writer.
pub fn hid_nodes(sysfs_path: &Path) -> Vec<PathBuf> {
    let mut nodes = Vec::new();
    let mut stack = vec![sysfs_path.to_path_buf()];

    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name == "hidraw" {
                for node in std::fs::read_dir(&path).into_iter().flatten().flatten() {
                    if let Some(node_name) = node.file_name().to_str()
                        && node_name.starts_with("hidraw")
                    {
                        nodes.push(PathBuf::from("/dev").join(node_name));
                    }
                }
            } else if name.contains(':') || name == "hwmon" {
                // Only descend through interface and class directories; the
                // rest of a USB device tree cannot hold a hidraw node.
                stack.push(path);
            }
        }
    }

    nodes.sort();
    nodes.dedup();
    nodes
}

/// Processes other than this one holding any of `nodes`.
///
/// Only processes whose `/proc/<pid>/fd` this user can read are visible. A
/// holder running as another user is invisible here, so a clean result is
/// reported as "none observed", never as proof of exclusivity.
pub fn observed_holders(nodes: &[PathBuf]) -> Vec<OwnershipConflict> {
    if nodes.is_empty() {
        return Vec::new();
    }

    let own_pid = std::process::id();
    let mut conflicts = Vec::new();

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return conflicts;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if pid == own_pid {
            continue;
        }

        let Ok(descriptors) = std::fs::read_dir(entry.path().join("fd")) else {
            continue; // Another user's process, or it exited mid-scan.
        };

        for descriptor in descriptors.flatten() {
            let Ok(target) = std::fs::read_link(descriptor.path()) else {
                continue;
            };
            if nodes.contains(&target) {
                conflicts.push(OwnershipConflict {
                    device: None,
                    resource: target.display().to_string(),
                    detail: format!(
                        "Process {pid} ({}) already holds this device node.",
                        process_name(pid).unwrap_or_else(|| "unknown".to_string())
                    ),
                });
                break;
            }
        }
    }

    conflicts
}

fn process_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::KRAKEN_BASE;

    fn temp_paths(name: &str) -> (PathBuf, Paths) {
        let base = std::env::temp_dir().join(format!("nzxt-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let paths = Paths::new(base.join("run"), base.join("config"));
        paths.ensure().unwrap();
        (base, paths)
    }

    #[test]
    fn the_first_holder_wins_and_the_second_is_told_who_holds_it() {
        let (base, paths) = temp_paths("exclusive");

        let first = acquire(&paths, KRAKEN_BASE).expect("first acquisition succeeds");
        assert_eq!(first.device(), KRAKEN_BASE);

        let conflict = acquire(&paths, KRAKEN_BASE).expect_err("second acquisition conflicts");
        assert_eq!(conflict.device, Some(KRAKEN_BASE));
        assert!(
            conflict.detail.contains(&std::process::id().to_string()),
            "{}",
            conflict.detail
        );

        drop(first);
        acquire(&paths, KRAKEN_BASE).expect("lock is released on drop");

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn locks_are_per_device() {
        let (base, paths) = temp_paths("per-device");
        let kraken = acquire(&paths, KRAKEN_BASE).unwrap();
        let rgb = acquire(&paths, nzxt_core::RGB_CONTROLLER).unwrap();
        assert_ne!(kraken.path(), rgb.path());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn hid_nodes_are_found_through_interface_directories() {
        let base = std::env::temp_dir().join(format!("nzxt-hid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let device = base.join("1-9");
        std::fs::create_dir_all(device.join("1-9:1.1/0003:1E71:300E.000B/hidraw/hidraw3")).unwrap();
        std::fs::create_dir_all(device.join("power")).unwrap();

        let nodes = hid_nodes(&device);
        assert_eq!(nodes, vec![PathBuf::from("/dev/hidraw3")]);

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_node_this_process_holds_is_not_reported_as_a_conflict() {
        let path = std::env::temp_dir().join(format!("nzxt-own-fd-{}", std::process::id()));
        let file = File::create(&path).unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();

        assert!(observed_holders(&[canonical]).is_empty());

        drop(file);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_empty_node_list_scans_nothing() {
        assert!(observed_holders(&[]).is_empty());
    }
}
