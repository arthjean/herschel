// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The configuration the daemon holds, and what it asks of it.
//!
//! One value in memory, loaded once at startup and rewritten on every committed
//! change. It answers three questions the rest of the daemon asks: which named
//! profiles exist, what the operator last put on the hardware, and what a start
//! should therefore replay. The precedence between the last two lives here
//! rather than at the caller, because it is one rule per kind of write and the
//! caller that had to restate it drifted from the one that did not.
//!
//! How the file is read, validated and replaced is the `document` module's job.

mod document;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use kori_core::display::DisplayPreset;
use kori_core::ipc::ConfigState;
use kori_core::lighting::LightingCommand;
use kori_core::profile::{
    CONFIG_SCHEMA_VERSION, CoolingProgram, Profile, SAFE_PROFILE_NAME, validate_profile,
};

pub use document::{ConfigDocument, ConfigError, SessionState};
use document::{preserve, read_document, write_atomically};

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

    /// Remove a stored profile, activating the safe one when it was active.
    ///
    /// Both edits land in one commit. The file is replaced by a rename, so a
    /// single document carrying "this profile is gone and the safe one is
    /// active" is one atomic transition: no reader can see the file between
    /// them, and a write that fails leaves the profile both present and
    /// selected rather than deleted from the selection but not from the list.
    pub fn delete_profile(&mut self, name: &str) -> Result<Option<String>, ConfigError> {
        if name == SAFE_PROFILE_NAME {
            return Err(ConfigError::MissingActiveProfile(name.to_string()));
        }
        if !self.document.profiles.iter().any(|p| p.name == name) {
            return Err(ConfigError::MissingActiveProfile(name.to_string()));
        }

        self.commit_change(|document| {
            document.profiles.retain(|p| p.name != name);
            if document.active_profile == name {
                document.active_profile = SAFE_PROFILE_NAME.to_string();
                return Some(SAFE_PROFILE_NAME.to_string());
            }
            None
        })
    }

    /// The lighting a start should replay: the active profile, with every
    /// channel the session has since committed put over it.
    ///
    /// Per channel rather than wholesale. A profile that lights three channels
    /// and one channel edited afterwards is four facts, and taking the session
    /// as a whole would silently unlight the two the operator never touched.
    pub fn lighting_to_restore(&self) -> Vec<LightingCommand> {
        let profile = self.active_profile();
        let session = &self.document.session;
        let mut commands: Vec<LightingCommand> = profile
            .lighting
            .into_iter()
            .map(|command| match session.channel(command.channel) {
                Some(committed) => committed.clone(),
                None => command,
            })
            .collect();
        for command in &session.lighting {
            if !commands.iter().any(|held| held.channel == command.channel) {
                commands.push(command.clone());
            }
        }
        commands
    }

    /// The thermal program the session holds, with no fallback to the profile.
    ///
    /// Named for what it is rather than for what a start does with it, because
    /// unlike [`Self::display_to_restore`] it does *not* fall back: `None` means
    /// nothing has been committed since the profile was chosen, and the caller
    /// reports the profile by name in that case. A program is one fact, like a
    /// preset and unlike a set of lighting channels, so the session wins
    /// outright when it holds one rather than being merged part by part.
    pub fn session_program(&self) -> Option<&CoolingProgram> {
        self.document.session.program.as_ref()
    }

    /// Record the program the device was just confirmed to be running.
    pub fn record_program(&mut self, program: &CoolingProgram) -> Result<(), ConfigError> {
        if self.document.session.program.as_ref() == Some(program) {
            return Ok(());
        }
        let program = program.clone();
        self.commit_change(|document| document.session.program = Some(program))
    }

    /// The preset a start should put back on the panel.
    ///
    /// The session wins outright here, because a preset is one picture: there
    /// is no half of it the profile could still own.
    pub fn display_to_restore(&self) -> Option<DisplayPreset> {
        self.document
            .session
            .display
            .clone()
            .or_else(|| self.active_profile().display)
    }

    /// Record the preset the panel was just told to show.
    ///
    /// A preset identical to the one already recorded writes nothing, so
    /// re-applying the same picture does not touch the disk.
    pub fn record_display(&mut self, preset: &DisplayPreset) -> Result<(), ConfigError> {
        if self.document.session.display.as_ref() == Some(preset) {
            return Ok(());
        }
        let preset = preset.clone();
        self.commit_change(|document| document.session.display = Some(preset))
    }

    /// Record the program a channel was just told to run.
    pub fn record_lighting(&mut self, command: &LightingCommand) -> Result<(), ConfigError> {
        if self.document.session.channel(command.channel) == Some(command) {
            return Ok(());
        }
        let command = command.clone();
        self.commit_change(|document| {
            match document
                .session
                .lighting
                .iter_mut()
                .find(|held| held.channel == command.channel)
            {
                Some(existing) => *existing = command,
                None => document.session.lighting.push(command),
            }
        })
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
