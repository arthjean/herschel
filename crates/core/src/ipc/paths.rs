// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the socket and the per-device locks live on this machine.
//!
//! Filesystem layout rather than protocol: the daemon binds where this says and
//! the client connects where this says, from the same functions, so the two
//! cannot drift onto different paths.

use std::path::{Path, PathBuf};

/// Environment variable that overrides the socket path, used by tests and by
/// anyone running a second daemon against a fake sysfs root.
pub const SOCKET_PATH_ENV: &str = "KORI_SOCKET";

/// Socket file name inside the per-user runtime directory.
pub const SOCKET_FILE_NAME: &str = "kori.sock";

/// Environment variable that overrides the runtime directory holding the
/// socket and the per-device locks.
pub const RUNTIME_DIR_ENV: &str = "KORI_RUNTIME_DIR";

/// Directory name both processes append to a base runtime directory.
const APP_DIR: &str = "kori";

/// Resolve the runtime directory from explicit inputs.
///
/// Split from the environment so the fallback order is testable without
/// mutating a process-global variable.
///
/// The last resort is the user's own cache directory, never `/tmp`: a socket
/// in a world-writable directory can be created by another user before the
/// daemon binds, and a client reaching it would take its device list, its
/// ownership state and its capability record from whoever won that race.
pub fn runtime_dir(
    override_dir: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    match (override_dir, xdg_runtime_dir) {
        (Some(path), _) => path.to_path_buf(),
        (None, Some(path)) => path.join(APP_DIR),
        (None, None) => home
            .unwrap_or_else(|| Path::new("."))
            .join(".cache")
            .join(APP_DIR),
    }
}

/// The runtime directory this machine resolves to.
pub fn runtime_dir_from_env() -> PathBuf {
    runtime_dir(
        env_path(RUNTIME_DIR_ENV).as_deref(),
        env_path("XDG_RUNTIME_DIR").as_deref(),
        env_path("HOME").as_deref(),
    )
}

/// The socket path this machine resolves to.
///
/// The daemon binds here and the client connects here, from this one function,
/// so the two cannot drift onto different paths.
pub fn socket_path_from_env() -> PathBuf {
    match env_path(SOCKET_PATH_ENV) {
        Some(path) => path,
        None => runtime_dir_from_env().join(SOCKET_FILE_NAME),
    }
}

/// A non-empty environment variable read as a path.
fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_directory_never_falls_back_to_a_world_writable_place() {
        let home = Path::new("/home/a");
        let resolved = runtime_dir(None, None, Some(home));
        assert!(resolved.starts_with(home), "{resolved:?}");
        assert!(!resolved.starts_with("/tmp"), "{resolved:?}");
        assert!(!resolved.starts_with("/var/tmp"), "{resolved:?}");
    }

    #[test]
    fn the_runtime_directory_prefers_the_override_then_xdg() {
        assert_eq!(
            runtime_dir(
                Some(Path::new("/explicit")),
                Some(Path::new("/run/user/1000")),
                Some(Path::new("/home/a"))
            ),
            Path::new("/explicit")
        );
        assert_eq!(
            runtime_dir(
                None,
                Some(Path::new("/run/user/1000")),
                Some(Path::new("/home/a"))
            ),
            Path::new("/run/user/1000/kori")
        );
    }

    #[test]
    fn the_socket_sits_inside_the_runtime_directory() {
        let directory = runtime_dir(None, Some(Path::new("/run/user/1000")), None);
        assert_eq!(
            directory.join(SOCKET_FILE_NAME),
            Path::new("/run/user/1000/kori/kori.sock")
        );
    }
}
