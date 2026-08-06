// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Schema-versioned local configuration with atomic writes and recovery.
//!
//! A save is a write to a temporary file, an `fsync`, then a `rename`. The
//! rename is atomic, so a reader sees either the previous valid file or the
//! new one, never a half-written mixture. A file that cannot be parsed is
//! preserved for diagnosis instead of being overwritten, and the built-in safe
//! profile takes over.

use std::io::Write;
use std::path::{Path, PathBuf};

use nzxt_core::ipc::ConfigState;
use nzxt_core::profile::{
    CONFIG_SCHEMA_VERSION, Profile, SAFE_PROFILE_NAME, ValidationError, validate_profile,
};
use serde::{Deserialize, Serialize};

/// The on-disk document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDocument {
    pub schema_version: u32,
    pub active_profile: String,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            active_profile: SAFE_PROFILE_NAME.to_string(),
            profiles: Vec::new(),
        }
    }
}

/// Why a configuration file could not be used as-is.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration file is not valid TOML: {0}")]
    Parse(String),
    #[error("configuration declares schema {found}, this build understands {supported}")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("profile {name} is invalid: {source}")]
    InvalidProfile {
        name: String,
        #[source]
        source: ValidationError,
    },
    #[error("active profile {0} is not in the file")]
    MissingActiveProfile(String),
    #[error("duplicate profile name {0}")]
    DuplicateProfile(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Configuration in memory, with how it was obtained.
#[derive(Debug, Clone)]
pub struct Configuration {
    path: PathBuf,
    document: ConfigDocument,
    state: ConfigState,
}

impl Configuration {
    /// Load `path`, recovering to safe defaults when it cannot be trusted.
    ///
    /// Recovery never deletes the unusable file: it is renamed aside so the
    /// operator can inspect or export it.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        match read_document(&path) {
            Ok(Some(document)) => Self {
                path,
                document,
                state: ConfigState::Loaded,
            },
            Ok(None) => Self {
                path,
                document: ConfigDocument::default(),
                state: ConfigState::Defaults,
            },
            Err(error) => {
                let preserved = preserve(&path);
                let preserved_path = preserved
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                Self {
                    path,
                    document: ConfigDocument::default(),
                    state: ConfigState::Recovered {
                        detail: error.to_string(),
                        preserved_path,
                        recovery_action:
                            "Save a profile to write a fresh configuration, or inspect the \
                             preserved file and restore it manually."
                                .to_string(),
                    },
                }
            }
        }
    }

    pub fn state(&self) -> &ConfigState {
        &self.state
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stored profiles, with the built-in safe profile first.
    ///
    /// The safe profile is always present and cannot be shadowed or deleted.
    pub fn profiles(&self) -> Vec<Profile> {
        let mut profiles = vec![Profile::safe()];
        profiles.extend(self.document.profiles.iter().cloned());
        profiles
    }

    pub fn active_profile_name(&self) -> &str {
        &self.document.active_profile
    }

    pub fn profile(&self, name: &str) -> Option<Profile> {
        self.profiles().into_iter().find(|p| p.name == name)
    }

    /// The active profile, falling back to the safe profile if it vanished.
    pub fn active_profile(&self) -> Profile {
        self.profile(&self.document.active_profile)
            .unwrap_or_else(Profile::safe)
    }

    /// Store a profile, replacing one with the same name.
    pub fn save_profile(&mut self, profile: Profile) -> Result<(), ConfigError> {
        validate_profile(&profile).map_err(|source| ConfigError::InvalidProfile {
            name: profile.name.clone(),
            source,
        })?;
        if profile.name == SAFE_PROFILE_NAME {
            return Err(ConfigError::DuplicateProfile(profile.name));
        }

        self.commit_change(|document| {
            match document
                .profiles
                .iter_mut()
                .find(|p| p.name == profile.name)
            {
                Some(existing) => *existing = profile,
                None => document.profiles.push(profile),
            }
        })
    }

    /// Select an existing profile as active.
    pub fn activate(&mut self, name: &str) -> Result<Profile, ConfigError> {
        let profile = self
            .profile(name)
            .ok_or_else(|| ConfigError::MissingActiveProfile(name.to_string()))?;
        let active = profile.name.clone();
        self.commit_change(|document| document.active_profile = active)?;
        Ok(profile)
    }

    /// Remove a stored profile.
    ///
    /// When the deleted profile is active, the safe profile is activated and
    /// committed first, so no window exists where the active profile is gone.
    pub fn delete_profile(&mut self, name: &str) -> Result<Option<String>, ConfigError> {
        if name == SAFE_PROFILE_NAME {
            return Err(ConfigError::MissingActiveProfile(name.to_string()));
        }
        if !self.document.profiles.iter().any(|p| p.name == name) {
            return Err(ConfigError::MissingActiveProfile(name.to_string()));
        }

        let activated = if self.document.active_profile == name {
            self.commit_change(|document| document.active_profile = SAFE_PROFILE_NAME.to_string())?;
            Some(SAFE_PROFILE_NAME.to_string())
        } else {
            None
        };

        self.commit_change(|document| document.profiles.retain(|p| p.name != name))?;
        Ok(activated)
    }

    /// Apply `change` to the document, then persist it.
    ///
    /// A failed write rolls the in-memory document back. Without that, a save
    /// the caller was told had failed would still be listed by `Profiles` and
    /// activatable, so the daemon would report a profile that is not on disk.
    fn commit_change<T>(
        &mut self,
        change: impl FnOnce(&mut ConfigDocument) -> T,
    ) -> Result<T, ConfigError> {
        let previous = self.document.clone();
        let value = change(&mut self.document);
        match self.commit() {
            Ok(()) => Ok(value),
            Err(error) => {
                self.document = previous;
                Err(error)
            }
        }
    }

    /// Write the document atomically.
    fn commit(&mut self) -> Result<(), ConfigError> {
        self.document.schema_version = CONFIG_SCHEMA_VERSION;
        write_atomically(&self.path, &self.document)?;
        if matches!(self.state, ConfigState::Defaults) {
            self.state = ConfigState::Loaded;
        }
        Ok(())
    }
}

fn read_document(path: &Path) -> Result<Option<ConfigDocument>, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigError::Io(error)),
    };

    let document: ConfigDocument =
        toml::from_str(&raw).map_err(|error| ConfigError::Parse(error.to_string()))?;

    if document.schema_version != CONFIG_SCHEMA_VERSION {
        return Err(ConfigError::UnsupportedSchema {
            found: document.schema_version,
            supported: CONFIG_SCHEMA_VERSION,
        });
    }

    let mut seen = Vec::new();
    for profile in &document.profiles {
        if profile.name == SAFE_PROFILE_NAME || seen.contains(&profile.name) {
            return Err(ConfigError::DuplicateProfile(profile.name.clone()));
        }
        seen.push(profile.name.clone());
        validate_profile(profile).map_err(|source| ConfigError::InvalidProfile {
            name: profile.name.clone(),
            source,
        })?;
    }

    if document.active_profile != SAFE_PROFILE_NAME && !seen.contains(&document.active_profile) {
        return Err(ConfigError::MissingActiveProfile(
            document.active_profile.clone(),
        ));
    }

    Ok(Some(document))
}

/// Rename an unusable file aside, keeping it for diagnosis.
fn preserve(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let stamp = crate::now_unix_ms();
    let target = path.with_extension(format!("corrupt.{stamp}"));
    std::fs::rename(path, &target).ok().map(|_| target)
}

/// Serialize to a temporary file, flush it to disk, then rename over the
/// target. The previous file stays intact until the rename lands.
fn write_atomically(path: &Path, document: &ConfigDocument) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    let encoded =
        toml::to_string_pretty(document).map_err(|error| ConfigError::Parse(error.to_string()))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
    }

    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(ConfigError::Io(error));
    }

    // Persist the directory entry so the rename survives a power loss.
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::profile::{CoolingProgram, TemperatureCurve};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempConfig {
        dir: PathBuf,
    }

    impl TempConfig {
        fn new(name: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "nzxt-config-{name}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn file(&self) -> PathBuf {
            self.dir.join("config.toml")
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn named(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            program: CoolingProgram::Fixed { pump: 128, fan: 90 },
            device: None,
        }
    }

    #[test]
    fn a_missing_file_yields_defaults_without_creating_anything() {
        let temp = TempConfig::new("missing");
        let config = Configuration::load(temp.file());

        assert_eq!(config.state(), &ConfigState::Defaults);
        assert!(config.active_profile().is_safe_builtin());
        assert!(!temp.file().exists(), "loading must not write");
    }

    #[test]
    fn a_saved_profile_survives_a_reload() {
        let temp = TempConfig::new("round-trip");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Silent")).unwrap();
        config.activate("Silent").unwrap();

        let reloaded = Configuration::load(temp.file());
        assert_eq!(reloaded.state(), &ConfigState::Loaded);
        assert_eq!(reloaded.active_profile_name(), "Silent");
        assert_eq!(reloaded.active_profile(), named("Silent"));
    }

    #[test]
    fn saving_replaces_the_file_atomically_and_leaves_no_temporary() {
        let temp = TempConfig::new("atomic");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("First")).unwrap();
        config.save_profile(named("Second")).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&temp.dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "config.toml")
            .collect();
        assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
    }

    #[test]
    fn a_hundred_save_and_reload_cycles_do_not_change_a_value() {
        let temp = TempConfig::new("durability");
        let mut curve = TemperatureCurve::flat(120);
        for (index, point) in curve.points.iter_mut().enumerate() {
            *point = 120 + index as u8;
        }
        let profile = Profile {
            name: "Curve".into(),
            program: CoolingProgram::Curve {
                pump: curve.clone(),
                fan: curve.clone(),
            },
            device: Some(nzxt_core::KRAKEN_BASE),
        };

        for _ in 0..100 {
            let mut config = Configuration::load(temp.file());
            config.save_profile(profile.clone()).unwrap();
            config.activate("Curve").unwrap();

            let reloaded = Configuration::load(temp.file());
            assert_eq!(reloaded.state(), &ConfigState::Loaded);
            assert_eq!(reloaded.active_profile(), profile);
        }
    }

    #[test]
    fn a_truncated_file_is_preserved_and_safe_defaults_take_over() {
        let temp = TempConfig::new("truncated");
        std::fs::write(temp.file(), "schema_version = 1\nactive_pro").unwrap();

        let config = Configuration::load(temp.file());
        assert!(config.active_profile().is_safe_builtin());

        let ConfigState::Recovered { preserved_path, .. } = config.state() else {
            panic!("expected recovery, got {:?}", config.state());
        };
        assert!(
            Path::new(preserved_path).exists(),
            "corrupt file must be kept"
        );
        let message = config.state().recovery_message().unwrap();
        assert!(message.contains("Safe defaults are active"), "{message}");
    }

    #[test]
    fn a_future_schema_version_is_a_recovery_case_not_a_guess() {
        let temp = TempConfig::new("future");
        std::fs::write(
            temp.file(),
            "schema_version = 99\nactive_profile = \"Silent\"\n",
        )
        .unwrap();

        let config = Configuration::load(temp.file());
        let ConfigState::Recovered { detail, .. } = config.state() else {
            panic!("expected recovery");
        };
        assert!(detail.contains("99"), "{detail}");
        assert!(config.active_profile().is_safe_builtin());
    }

    #[test]
    fn a_profile_with_invalid_values_is_rejected_on_load() {
        let temp = TempConfig::new("invalid-values");
        std::fs::write(
            temp.file(),
            r#"
schema_version = 1
active_profile = "Bad"

[[profiles]]
name = "Bad"

[profiles.program]
mode = "fixed"
pump = 3
fan = 90
"#,
        )
        .unwrap();

        let config = Configuration::load(temp.file());
        assert!(matches!(config.state(), ConfigState::Recovered { .. }));
        assert!(config.active_profile().is_safe_builtin());
    }

    #[test]
    fn an_unknown_key_is_treated_as_an_unreadable_file() {
        let temp = TempConfig::new("unknown-key");
        std::fs::write(
            temp.file(),
            "schema_version = 1\nactive_profile = \"Onboard safe\"\ntelemetry_url = \"https://x\"\n",
        )
        .unwrap();

        let config = Configuration::load(temp.file());
        assert!(matches!(config.state(), ConfigState::Recovered { .. }));
    }

    #[test]
    fn an_invalid_profile_is_never_written() {
        let temp = TempConfig::new("reject-save");
        let mut config = Configuration::load(temp.file());
        let error = config
            .save_profile(Profile {
                name: "  ".into(),
                program: CoolingProgram::Onboard,
                device: None,
            })
            .unwrap_err();

        assert!(matches!(error, ConfigError::InvalidProfile { .. }));
        assert!(!temp.file().exists());
    }

    #[test]
    fn the_safe_profile_cannot_be_shadowed_or_deleted() {
        let temp = TempConfig::new("safe-profile");
        let mut config = Configuration::load(temp.file());

        assert!(config.save_profile(named(SAFE_PROFILE_NAME)).is_err());
        assert!(config.delete_profile(SAFE_PROFILE_NAME).is_err());
        assert!(config.profile(SAFE_PROFILE_NAME).unwrap().is_safe_builtin());
    }

    #[test]
    fn deleting_the_active_profile_activates_the_safe_one_first() {
        let temp = TempConfig::new("delete-active");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Loud")).unwrap();
        config.activate("Loud").unwrap();

        let activated = config.delete_profile("Loud").unwrap();
        assert_eq!(activated.as_deref(), Some(SAFE_PROFILE_NAME));
        assert!(config.profile("Loud").is_none());

        let reloaded = Configuration::load(temp.file());
        assert!(reloaded.active_profile().is_safe_builtin());
    }

    #[test]
    fn deleting_an_inactive_profile_keeps_the_active_one() {
        let temp = TempConfig::new("delete-inactive");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Keep")).unwrap();
        config.save_profile(named("Drop")).unwrap();
        config.activate("Keep").unwrap();

        assert_eq!(config.delete_profile("Drop").unwrap(), None);
        assert_eq!(config.active_profile_name(), "Keep");
    }

    #[test]
    fn a_failed_write_leaves_neither_the_file_nor_the_daemon_holding_the_profile() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return; // Root ignores the directory mode.
        }

        let temp = TempConfig::new("commit-failure");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Kept")).unwrap();

        // A directory this user cannot write is how a full disk or a revoked
        // permission presents to the daemon.
        std::fs::set_permissions(&temp.dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = config.save_profile(named("Lost")).unwrap_err();
        std::fs::set_permissions(&temp.dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(matches!(error, ConfigError::Io(_)), "{error:?}");
        assert!(
            config.profile("Lost").is_none(),
            "a refused save must not stay in memory"
        );
        assert!(config.profile("Kept").is_some());
        assert!(Configuration::load(temp.file()).profile("Lost").is_none());
    }

    #[test]
    fn the_file_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempConfig::new("permissions");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Silent")).unwrap();

        let mode = std::fs::metadata(temp.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
