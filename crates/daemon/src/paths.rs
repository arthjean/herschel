// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the daemon keeps its socket, locks and configuration.
//!
//! Every path is per-user and inside a directory the user already owns. The
//! daemon creates nothing outside them and never needs root.

use std::path::PathBuf;

use kori_core::ipc::{SOCKET_FILE_NAME, runtime_dir_from_env, socket_path_from_env};

/// Overrides the configuration directory. Used by tests.
pub const CONFIG_DIR_ENV: &str = "KORI_CONFIG_DIR";

const APP_DIR: &str = "kori";

/// Resolved locations for one daemon instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub runtime_dir: PathBuf,
    pub config_dir: PathBuf,
    pub socket: PathBuf,
}

impl Paths {
    /// Resolve from the environment, following the XDG base directory spec.
    ///
    /// The runtime directory and the socket come from `kori_core::ipc`, which
    /// is the same code the client resolves them with. Two independent
    /// resolutions would let the daemon bind one path while the window looks
    /// for another.
    pub fn from_env() -> Self {
        let config_dir = match env_path(CONFIG_DIR_ENV) {
            Some(path) => path,
            None => match env_path("XDG_CONFIG_HOME") {
                Some(path) => path.join(APP_DIR),
                None => home().join(".config").join(APP_DIR),
            },
        };

        Self {
            runtime_dir: runtime_dir_from_env(),
            config_dir,
            socket: socket_path_from_env(),
        }
    }

    /// Explicit paths, used by tests and by a second instance under a fixture.
    pub fn new(runtime_dir: impl Into<PathBuf>, config_dir: impl Into<PathBuf>) -> Self {
        let runtime_dir = runtime_dir.into();
        let socket = runtime_dir.join(SOCKET_FILE_NAME);
        Self {
            runtime_dir,
            config_dir: config_dir.into(),
            socket,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// Lock file for one device, named after its identifiers.
    pub fn device_lock(&self, device: kori_core::DeviceId) -> PathBuf {
        self.runtime_dir.join(format!("{device}.lock"))
    }

    /// Lock file for the daemon itself, held for the length of the process.
    ///
    /// Named after the application rather than a device, so it cannot collide
    /// with `device_lock`, which always contains a colon.
    pub fn instance_lock(&self) -> PathBuf {
        self.runtime_dir.join("kori.lock")
    }

    /// Create both directories with owner-only permissions.
    pub fn ensure(&self) -> std::io::Result<()> {
        create_private_dir(&self.runtime_dir)?;
        create_private_dir(&self.config_dir)?;
        Ok(())
    }
}

fn create_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path)?;
    // Applied after creation so an inherited umask cannot widen it.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

fn home() -> PathBuf {
    env_path("HOME").unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::KRAKEN_BASE;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn lock_files_are_named_after_the_device() {
        let paths = Paths::new("/run/user/1000/kori", "/home/a/.config/kori");
        assert!(paths.device_lock(KRAKEN_BASE).ends_with("1e71:300e.lock"));
        assert!(paths.socket.ends_with("kori.sock"));
        assert!(paths.config_file().ends_with("config.toml"));
        assert!(paths.instance_lock().ends_with("kori.lock"));
        assert_ne!(paths.instance_lock(), paths.device_lock(KRAKEN_BASE));
    }

    #[test]
    fn the_daemon_binds_the_socket_the_client_connects_to() {
        // Both sides resolve through `kori_core::ipc`. This fails the moment
        // either one grows its own fallback again.
        let paths = Paths::from_env();
        assert_eq!(paths.socket, kori_core::ipc::socket_path_from_env());
        assert_eq!(paths.runtime_dir, kori_core::ipc::runtime_dir_from_env());
        assert!(!paths.socket.starts_with("/tmp"), "{:?}", paths.socket);
    }

    #[test]
    fn directories_are_created_owner_only() {
        let base = std::env::temp_dir().join(format!("kori-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let paths = Paths::new(base.join("run"), base.join("config"));
        paths.ensure().unwrap();

        for dir in [&paths.runtime_dir, &paths.config_dir] {
            let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{dir:?} must not be readable by other users");
        }

        std::fs::remove_dir_all(&base).unwrap();
    }
}
