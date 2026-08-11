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

use kori_core::display::DisplayPreset;
use kori_core::ipc::ConfigState;
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
    fn channel(&self, channel: u8) -> Option<&LightingCommand> {
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

fn read_document(path: &Path) -> Result<Option<ConfigDocument>, ConfigError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::profile::{CoolingProgram, TemperatureCurve};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempConfig {
        dir: PathBuf,
    }

    impl TempConfig {
        fn new(name: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "kori-config-{name}-{}-{unique}",
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
            lighting: Vec::new(),
            display: None,
        }
    }

    fn lit(channel: u8, red: u8) -> LightingCommand {
        LightingCommand {
            channel,
            program: kori_core::lighting::LightingProgram::Fixed {
                color: kori_core::lighting::Rgb::new(red, 0, 0),
                brightness: kori_core::lighting::Brightness::FULL,
            },
        }
    }

    #[test]
    fn a_committed_preset_survives_a_reload() {
        // The whole point of the record: the panel comes back on the picture it
        // was left on, without the operator having saved a profile for it.
        let temp = TempConfig::new("session-display");
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = kori_core::display::DisplayMode::SingleReading;
        preset.orientation = kori_core::display::Orientation::Deg180;
        preset.brightness = kori_core::lighting::Brightness::new(45).unwrap();

        let mut config = Configuration::load(temp.file());
        assert_eq!(config.display_to_restore(), None);
        config.record_display(&preset).unwrap();

        let reloaded = Configuration::load(temp.file());
        assert_eq!(reloaded.state(), &ConfigState::Loaded);
        assert_eq!(reloaded.display_to_restore(), Some(preset));
    }

    /// A curve is the one program nothing can read back, so the file is the
    /// only place it survives a restart. This is the Cooling half of
    /// [`a_committed_preset_survives_a_reload`], and it has to hold with no
    /// profile saved at all.
    #[test]
    fn a_committed_curve_survives_a_reload_without_a_profile() {
        let temp = TempConfig::new("session-program");
        let mut drawn = TemperatureCurve::flat(140);
        for (index, point) in drawn.points.iter_mut().enumerate() {
            *point = 140 + index as u8;
        }
        let program = CoolingProgram::Curve {
            pump: drawn.clone(),
            fan: drawn,
        };

        let mut config = Configuration::load(temp.file());
        assert_eq!(config.session_program(), None);
        config.record_program(&program).unwrap();

        let reloaded = Configuration::load(temp.file());
        assert_eq!(reloaded.state(), &ConfigState::Loaded);
        assert_eq!(reloaded.session_program(), Some(&program));
        assert!(
            reloaded.active_profile().is_safe_builtin(),
            "the shape was never saved under a name, and did not need to be"
        );
    }

    #[test]
    fn re_committing_the_same_program_does_not_touch_the_disk() {
        let temp = TempConfig::new("session-program-idempotent");
        let program = CoolingProgram::Fixed { pump: 180, fan: 90 };

        let mut config = Configuration::load(temp.file());
        config.record_program(&program).unwrap();
        std::fs::remove_file(temp.file()).unwrap();
        config.record_program(&program).unwrap();
        assert!(
            !temp.file().exists(),
            "an unchanged program must write nothing"
        );
    }

    #[test]
    fn re_committing_the_same_picture_does_not_touch_the_disk() {
        // A repeated Apply is deduplicated at the panel, and it has to be
        // deduplicated here too: the daemon answers every settled edit, and a
        // file rewritten for a picture that did not change is wear for nothing.
        let temp = TempConfig::new("session-idempotent");
        let preset = DisplayPreset::default_infographic();

        let mut config = Configuration::load(temp.file());
        config.record_display(&preset).unwrap();
        assert!(temp.file().exists());

        // Removing the file makes the next write observable: a second identical
        // record that wrote anything would put it back.
        std::fs::remove_file(temp.file()).unwrap();
        config.record_display(&preset).unwrap();
        assert!(
            !temp.file().exists(),
            "an unchanged preset must write nothing"
        );
    }

    #[test]
    fn the_session_outranks_the_profile_one_channel_at_a_time() {
        let temp = TempConfig::new("session-lighting");
        let mut profile = named("Evening");
        profile.lighting = vec![lit(1, 0x10), lit(2, 0x20)];

        let mut config = Configuration::load(temp.file());
        config.save_profile(profile).unwrap();
        config.activate("Evening").unwrap();
        assert_eq!(
            config.lighting_to_restore(),
            vec![lit(1, 0x10), lit(2, 0x20)]
        );

        // Channel 1 is edited afterwards, channel 3 is lit for the first time.
        config.record_lighting(&lit(1, 0xF0)).unwrap();
        config.record_lighting(&lit(3, 0xF3)).unwrap();

        let reloaded = Configuration::load(temp.file());
        assert_eq!(
            reloaded.lighting_to_restore(),
            vec![lit(1, 0xF0), lit(2, 0x20), lit(3, 0xF3)],
            "the edited channel comes from the session, the untouched one from \
             the profile, and neither hides the other"
        );
    }

    #[test]
    fn a_file_written_before_the_session_existed_still_loads() {
        let temp = TempConfig::new("session-absent");
        std::fs::write(
            temp.file(),
            "schema_version = 1\nactive_profile = \"Onboard safe\"\n",
        )
        .unwrap();

        let config = Configuration::load(temp.file());
        assert_eq!(config.state(), &ConfigState::Loaded);
        assert_eq!(config.session_program(), None);
        assert_eq!(config.display_to_restore(), None);
        assert!(config.lighting_to_restore().is_empty());
    }

    #[test]
    fn nothing_committed_leaves_the_table_out_of_the_file() {
        let temp = TempConfig::new("session-empty");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Quiet")).unwrap();

        let written = std::fs::read_to_string(temp.file()).unwrap();
        assert!(
            !written.contains("[session]"),
            "an empty record must not appear in the operator's file: {written}"
        );
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
            device: Some(kori_core::KRAKEN_BASE),
            lighting: Vec::new(),
            display: None,
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
    fn a_file_from_an_earlier_schema_is_migrated_rather_than_orphaned() {
        let temp = TempConfig::new("earlier-schema");
        // Exactly what the schema-1 build wrote: no lighting section at all.
        std::fs::write(
            temp.file(),
            "schema_version = 1\n\
             active_profile = \"Silent\"\n\n\
             [[profiles]]\n\
             name = \"Silent\"\n\n\
             [profiles.program]\n\
             mode = \"fixed\"\n\
             pump = 120\n\
             fan = 80\n",
        )
        .unwrap();

        let mut config = Configuration::load(temp.file());
        assert_eq!(
            config.state(),
            &ConfigState::Loaded,
            "an added optional field must not orphan an operator's profiles"
        );
        let active = config.active_profile();
        assert_eq!(active.name, "Silent");
        assert!(active.lighting.is_empty());

        // The next save rewrites it at the current version.
        config
            .save_profile(Profile {
                name: "Loud".into(),
                program: CoolingProgram::Fixed {
                    pump: 200,
                    fan: 200,
                },
                device: None,
                lighting: Vec::new(),
                display: None,
            })
            .unwrap();
        let rewritten = std::fs::read_to_string(temp.file()).unwrap();
        assert!(
            rewritten.contains(&format!("schema_version = {CONFIG_SCHEMA_VERSION}")),
            "{rewritten}"
        );
        assert!(Configuration::load(temp.file()).profile("Silent").is_some());
    }

    #[test]
    fn a_file_declaring_no_schema_at_all_is_a_recovery_case() {
        let temp = TempConfig::new("zero-schema");
        std::fs::write(
            temp.file(),
            "schema_version = 0\nactive_profile = \"Onboard safe\"\n",
        )
        .unwrap();
        assert!(matches!(
            Configuration::load(temp.file()).state(),
            ConfigState::Recovered { .. }
        ));
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
                lighting: Vec::new(),
                display: None,
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
    fn deleting_the_active_profile_activates_the_safe_one_in_the_same_write() {
        let temp = TempConfig::new("delete-active");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Loud")).unwrap();
        config.activate("Loud").unwrap();

        let activated = config.delete_profile("Loud").unwrap();
        assert_eq!(activated.as_deref(), Some(SAFE_PROFILE_NAME));
        assert!(config.profile("Loud").is_none());

        let reloaded = Configuration::load(temp.file());
        assert!(reloaded.active_profile().is_safe_builtin());
        assert!(reloaded.profile("Loud").is_none());
    }

    /// A deletion that cannot be written leaves the profile both listed and
    /// selected.
    ///
    /// The two edits are one commit, so there is no order in which the file
    /// ends up with the selection moved and the profile still there. Splitting
    /// them would produce exactly that, and the operator would find their
    /// profile intact but no longer active with nothing on screen saying why.
    #[test]
    fn a_deletion_that_cannot_be_written_changes_nothing_at_all() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return; // Root ignores the directory mode.
        }

        let temp = TempConfig::new("delete-failure");
        let mut config = Configuration::load(temp.file());
        config.save_profile(named("Loud")).unwrap();
        config.activate("Loud").unwrap();

        std::fs::set_permissions(&temp.dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = config.delete_profile("Loud").unwrap_err();
        std::fs::set_permissions(&temp.dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(matches!(error, ConfigError::Io(_)), "{error:?}");
        assert!(config.profile("Loud").is_some());
        assert_eq!(config.active_profile_name(), "Loud");

        let reloaded = Configuration::load(temp.file());
        assert!(reloaded.profile("Loud").is_some());
        assert_eq!(reloaded.active_profile_name(), "Loud");
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
