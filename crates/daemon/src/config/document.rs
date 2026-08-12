// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The file itself: its shape, how it is read, and how it is replaced.
//!
//! A save is a write to a temporary file, an `fsync`, then a `rename`. The
//! rename is atomic, so a reader sees either the previous valid file or the new
//! one, never a half-written mixture. A file that cannot be parsed is preserved
//! for diagnosis instead of being overwritten.
//!
//! Separated from [`crate::config::Configuration`] because the two answer
//! different questions. This module owns what a valid document is and what
//! happens to an invalid one; the parent owns what the daemon asks of the
//! configuration it holds. A reader working out why their file went to recovery
//! does not need the profile API in the way, and a reader following a restore
//! does not need the `fsync` ordering.

use std::io::Write;
use std::path::{Path, PathBuf};

use kori_core::display::DisplayPreset;
use kori_core::lighting::LightingCommand;
use kori_core::profile::{
    CONFIG_SCHEMA_VERSION, CoolingProgram, Profile, SAFE_PROFILE_NAME, ValidationError,
    validate_profile,
};
use serde::{Deserialize, Serialize};

/// What the daemon last committed, outside any named profile.
///
/// A profile is what the operator chose to keep. This is what the hardware is
/// actually running, and the two are not the same fact: every Lighting edit
/// writes as it settles and none of them asks to be saved under a name, so a
/// restart that replayed only the active profile would drop every edit made
/// since that profile was written.
///
/// Only a committed write reaches this record, so it can never claim a write
/// that did not happen. The streaming path never touches it either: a telemetry
/// frame goes out through `DisplayExecutor::refresh`, not through an Apply, so
/// a panel redrawn once a second costs no disk write at all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionState {
    /// The last thermal program the daemon confirmed on the device.
    ///
    /// A curve drawn on the Cooling screen writes as it settles, exactly as a
    /// lighting edit does, and asks to be saved under a name no more than one
    /// does. Without this, the shape the operator drew lived only in the
    /// running window and the next start replayed the profile instead.
    #[serde(default)]
    pub program: Option<CoolingProgram>,
    /// The last program each channel was told to run, one entry per channel.
    #[serde(default)]
    pub lighting: Vec<LightingCommand>,
    /// The last preset the panel was told to show.
    #[serde(default)]
    pub display: Option<DisplayPreset>,
}

impl SessionState {
    /// True while nothing has been committed, which keeps the table out of the
    /// file rather than writing an empty one.
    fn is_empty(&self) -> bool {
        self.program.is_none() && self.lighting.is_empty() && self.display.is_none()
    }

    /// The program committed to `channel`, when one was.
    pub(super) fn channel(&self, channel: u8) -> Option<&LightingCommand> {
        self.lighting.iter().find(|held| held.channel == channel)
    }
}

/// The on-disk document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigDocument {
    pub schema_version: u32,
    pub active_profile: String,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Last committed state, restored ahead of the active profile.
    ///
    /// Written after the array of profiles because TOML renders a table after
    /// the arrays of tables that precede it, and read with `default` so a file
    /// written before this existed still loads.
    #[serde(default, skip_serializing_if = "SessionState::is_empty")]
    pub session: SessionState,
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            active_profile: SAFE_PROFILE_NAME.to_string(),
            profiles: Vec::new(),
            session: SessionState::default(),
        }
    }
}

/// Why a configuration file could not be used as-is.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration file is not valid TOML: {0}")]
    Parse(String),
    /// The document in memory could not be turned back into TOML.
    ///
    /// Separate from [`Self::Parse`] because it says the opposite thing: the
    /// file on disk is fine and it is this process that has nothing valid to
    /// write. Reported as "your file is not valid TOML", an operator would go
    /// looking at a file that was never the problem.
    #[error("configuration could not be encoded as TOML: {0}")]
    Encode(String),
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

pub(super) fn read_document(path: &Path) -> Result<Option<ConfigDocument>, ConfigError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigError::Io(error)),
    };

    let document: ConfigDocument =
        toml::from_str(&raw).map_err(|error| ConfigError::Parse(error.to_string()))?;

    // An older schema is read, not refused. Every version so far has only
    // added optional fields, so a file written by an earlier build parses
    // exactly as it stands and the next save rewrites it at the current
    // version. A version this build has never seen is a different question: it
    // may carry fields whose meaning is unknown, so it goes to recovery rather
    // than being interpreted. Both directions preserve the operator's file.
    if document.schema_version > CONFIG_SCHEMA_VERSION || document.schema_version == 0 {
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
pub(super) fn preserve(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let stamp = crate::now_unix_ms();
    let target = path.with_extension(format!("corrupt.{stamp}"));
    std::fs::rename(path, &target).ok().map(|_| target)
}

/// Serialize to a temporary file, flush it to disk, then rename over the
/// target. The previous file stays intact until the rename lands.
pub(super) fn write_atomically(path: &Path, document: &ConfigDocument) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt;

    let encoded =
        toml::to_string_pretty(document).map_err(|error| ConfigError::Encode(error.to_string()))?;

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
