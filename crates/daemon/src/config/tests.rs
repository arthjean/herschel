// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Everything asserted about the configuration, on a real file in a real
//! directory.
//!
//! Its own file because the subject is one module's behavior and the assertions
//! about it are the larger half. Every case here writes and reads an actual
//! `config.toml`: a rule about atomic replacement or about a file from an
//! earlier schema is only worth what the filesystem says about it.

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
    for (index, point) in drawn.points_mut().iter_mut().enumerate() {
        *point = 140 + index as u8;
    }
    let program = CoolingProgram::Curve {
        pump: drawn,
        fan: drawn,
    };

    let mut config = Configuration::load(temp.file());
    assert_eq!(config.program_to_restore(), None);
    config.record_program(&program).unwrap();

    let reloaded = Configuration::load(temp.file());
    assert_eq!(reloaded.state(), &ConfigState::Loaded);
    let restored = reloaded
        .program_to_restore()
        .expect("the committed shape is replayed");
    assert_eq!(restored.program, program);
    assert_eq!(
        restored.profile, None,
        "the shape was never saved under a name, so no profile is named"
    );
    assert!(
        reloaded.active_profile().is_safe_builtin(),
        "the shape was never saved under a name, and did not need to be"
    );
}

/// The session outranks the profile outright, and says so by naming nobody.
///
/// A program is one fact, unlike a set of lighting channels: there is no
/// half of it the profile could still own. The name travels with the answer
/// because it is what decides whether a start reports a profile activation
/// or only a program, and deciding that at the caller is what previously put
/// this rule in two places.
#[test]
fn the_session_program_outranks_the_profiles_and_names_no_profile() {
    let temp = TempConfig::new("session-program-precedence");
    let mut config = Configuration::load(temp.file());
    config.save_profile(named("Loud")).unwrap();
    config.activate("Loud").unwrap();

    let from_profile = config
        .program_to_restore()
        .expect("the profile is replayed");
    assert_eq!(
        from_profile.program,
        CoolingProgram::Fixed { pump: 128, fan: 90 }
    );
    assert_eq!(from_profile.profile.as_deref(), Some("Loud"));

    let edited = CoolingProgram::Fixed {
        pump: 200,
        fan: 150,
    };
    config.record_program(&edited).unwrap();

    let from_session = config
        .program_to_restore()
        .expect("the session is replayed");
    assert_eq!(from_session.program, edited);
    assert_eq!(
        from_session.profile, None,
        "an edit that was never saved under a name must not report one"
    );
}

/// A program that writes nothing is not one a start has to put back.
#[test]
fn an_onboard_program_leaves_nothing_to_replay_whichever_source_holds_it() {
    let temp = TempConfig::new("onboard-nothing-to-replay");
    let mut config = Configuration::load(temp.file());
    assert!(
        config.active_profile().is_safe_builtin(),
        "the built-in profile is the Onboard one"
    );
    assert_eq!(config.program_to_restore(), None);

    // A hand-edited file is the only way the session holds Onboard, since
    // the daemon records a program only once the device confirmed a write
    // and Onboard never reaches an attribute. It is filtered on that path
    // too, so the two sources cannot answer differently.
    config.record_program(&CoolingProgram::Onboard).unwrap();
    assert_eq!(config.program_to_restore(), None);
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
    assert_eq!(config.program_to_restore(), None);
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
    for (index, point) in curve.points_mut().iter_mut().enumerate() {
        *point = 120 + index as u8;
    }
    let profile = Profile {
        name: "Curve".into(),
        program: CoolingProgram::Curve {
            pump: curve,
            fan: curve,
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
