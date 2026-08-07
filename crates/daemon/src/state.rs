// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's authoritative state and the handler for every typed request.
//!
//! One instance owns the capability record, the per-device locks and the
//! configuration. The server wraps it in a mutex, so requests are serialized:
//! two clients can never interleave a command.

use std::time::{Duration, Instant};

use nzxt_core::capability::{CapabilityId, CapabilityRecord, DeviceRecord, LcdTopology};
use nzxt_core::diagnostics::{DiagnosticsLog, EventKind};
use nzxt_core::display::DisplayPreset;
use nzxt_core::ipc::{
    AccessMode, ActivationOutcome, ApplyOutcome, BlockedCapability, ConfigState, DaemonStatus,
    DeviceStatus, DisplayOutcome, HardwareState, IpcError, LightingOutcome, OwnershipConflict,
    PROTOCOL_VERSION, Request, Response,
};
use nzxt_core::lighting::{LightingCommand, validate_command};
use nzxt_core::profile::{
    CoolingProgram, Incompatibility, Profile, incompatibilities, program_incompatibilities,
    validate_program,
};
use nzxt_core::telemetry::{CollectorFailure, TelemetrySnapshot};
use nzxt_core::{DeviceId, KRAKEN_BASE, RGB_CONTROLLER, capability::Evidenced};
use nzxt_hardware_linux::SysfsRoot;
use nzxt_hardware_linux::probe::probe;

use crate::config::{ConfigError, Configuration};
use crate::cooling::CoolingExecutor;
use crate::display::DisplayExecutor;
use crate::lighting::LightingExecutor;
use crate::ownership::{self, DeviceLock};
use crate::paths::Paths;
use crate::telemetry::{Sampler, default_interval};

/// Version reported to clients and diagnostics.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the daemon's lighting commands go.
///
/// The controller is the one device this workspace reaches through a character
/// device rather than through sysfs, so it is the one that has to be injectable:
/// an integration test drives the real command, ownership and cadence code
/// against a controller that answers the same reports, and never opens `/dev`.
pub enum RgbBackend {
    /// Open the `hidraw` node the sysfs probe resolved.
    Hidraw,
    /// Talk to a supplied transport instead.
    Transport(Box<dyn nzxt_hardware_linux::rgb::HidTransport>),
    /// Do not talk to the controller at all.
    None,
}

/// Where the daemon's frames go.
///
/// Mirrors [`RgbBackend`] and exists for the same reason: the panel is reached
/// through a character device and a `usbfs` node, so an integration test drives
/// the real render, deduplication and transfer code against a Kraken that
/// answers the same reports without opening `/dev`.
pub enum LcdBackend {
    /// Open the `hidraw` node and claim the bulk interface sysfs resolved.
    Nodes,
    /// Talk to a supplied link instead.
    Link(Box<nzxt_hardware_linux::lcd::LcdLink>),
    /// Do not talk to the panel at all.
    None,
}

/// Everything one daemon instance owns.
pub struct Daemon {
    paths: Paths,
    capabilities: CapabilityRecord,
    /// Held for the daemon's lifetime; dropping releases the device.
    locks: Vec<DeviceLock>,
    conflicts: Vec<OwnershipConflict>,
    config: Configuration,
    diagnostics: DiagnosticsLog,
    /// The 1 Hz collectors. Dropping the daemon stops them.
    sampler: Sampler,
    /// The sole writer of the thermal path.
    cooling: CoolingExecutor,
    /// The sole writer of the lighting path.
    lighting: LightingExecutor,
    /// The sole writer of the panel.
    display: DisplayExecutor,
    /// Collector failures already recorded, so one fault is logged once.
    reported_failures: Vec<CollectorFailure>,
}

impl Daemon {
    /// Probe the machine, take what ownership is available and load config.
    ///
    /// Ownership failure is not startup failure: the daemon stays up in
    /// read-only mode and reports the conflict.
    pub fn start(paths: Paths, sysfs: &SysfsRoot) -> std::io::Result<Self> {
        Self::start_with(
            paths,
            sysfs,
            &nzxt_hardware_linux::sensors::proc_root_from_env(),
            default_interval(),
            RgbBackend::Hidraw,
            LcdBackend::Nodes,
        )
    }

    /// Start against explicit sources and an explicit sampling interval.
    ///
    /// Only the sources and the cadence change; every other behavior is
    /// identical, so a test exercises the same paths without waiting on real
    /// seconds or mutating a process-global variable.
    pub fn start_with(
        paths: Paths,
        sysfs: &SysfsRoot,
        proc_root: &std::path::Path,
        interval: Duration,
        rgb: RgbBackend,
        lcd: LcdBackend,
    ) -> std::io::Result<Self> {
        paths.ensure()?;

        let mut diagnostics = DiagnosticsLog::default();
        diagnostics.record(
            crate::now_unix_ms(),
            EventKind::DaemonStarted {
                version: DAEMON_VERSION.to_string(),
                socket_path: paths.socket.display().to_string(),
            },
        );

        let mut capabilities = probe(sysfs);
        // The controller is asked what it is before ownership and before the
        // capability states are logged, so the record the rest of this function
        // reads is the final one. The two requests carry no color and no mode,
        // so asking changes nothing the operator can see.
        let lighting = connect_lighting(&mut capabilities, rgb);
        // The panel is asked the same way and for the same reason: two query
        // reports that carry no picture, no brightness and no orientation.
        let display = connect_display(&mut capabilities, lcd);

        let mut locks = Vec::new();
        let mut conflicts = Vec::new();

        for rejected in &capabilities.rejected {
            diagnostics.record(
                crate::now_unix_ms(),
                EventKind::DeviceRejected {
                    device: rejected.id,
                    reason: rejected.reason.clone(),
                },
            );
        }

        for device in capabilities.supported() {
            // Serials are secrets: register them so they never reach an export.
            if let Evidenced::Known { value, .. } = &device.usb.serial {
                diagnostics.add_secret(value.clone());
            }

            diagnostics.record(
                crate::now_unix_ms(),
                EventKind::DeviceDiscovered {
                    device: device.id(),
                    sysfs_path: device.usb.sysfs_path.clone(),
                },
            );
            for capability in &device.capabilities {
                diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::CapabilityResolved {
                        device: device.id(),
                        capability: capability.id,
                        state: capability
                            .state
                            .blocked_reason()
                            .unwrap_or_else(|| "writable".to_string()),
                    },
                );
            }

            match ownership::acquire(&paths, device.id()) {
                Ok(lock) => {
                    diagnostics.record(
                        crate::now_unix_ms(),
                        EventKind::OwnershipAcquired {
                            device: device.id(),
                            lock_path: lock.path().display().to_string(),
                        },
                    );
                    locks.push(lock);
                }
                Err(conflict) => {
                    diagnostics.record(
                        crate::now_unix_ms(),
                        EventKind::OwnershipConflict {
                            device: conflict.device,
                            resource: conflict.resource.clone(),
                            detail: conflict.detail.clone(),
                        },
                    );
                    conflicts.push(conflict);
                }
            }

            // A program already holding the device's HID node is a competing
            // writer. It is reported, never forced away.
            let nodes = ownership::hid_nodes(std::path::Path::new(&device.usb.sysfs_path));
            for conflict in ownership::observed_holders(&nodes) {
                let conflict = OwnershipConflict {
                    device: Some(device.id()),
                    ..conflict
                };
                diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::OwnershipConflict {
                        device: conflict.device,
                        resource: conflict.resource.clone(),
                        detail: conflict.detail.clone(),
                    },
                );
                conflicts.push(conflict);
            }
        }

        let config = Configuration::load(paths.config_file());
        match config.state() {
            ConfigState::Recovered {
                detail,
                preserved_path,
                ..
            } => diagnostics.record(
                crate::now_unix_ms(),
                EventKind::ConfigRecovered {
                    detail: detail.clone(),
                    preserved_path: preserved_path.clone(),
                },
            ),
            _ => diagnostics.record(
                crate::now_unix_ms(),
                EventKind::ConfigLoaded {
                    path: config.path().display().to_string(),
                },
            ),
        }

        let mut daemon = Self {
            paths,
            capabilities,
            locks,
            conflicts,
            config,
            diagnostics,
            sampler: Sampler::start(sysfs, proc_root, interval),
            cooling: CoolingExecutor::open(sysfs),
            lighting,
            display,
            reported_failures: Vec::new(),
        };

        let read_only = daemon.access_mode().is_read_only();
        let reason = daemon
            .read_only_reason()
            .unwrap_or_else(|| "every supported capability is writable".to_string());
        daemon.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::AccessModeChanged { read_only, reason },
        );

        // The active profile is put back on the hardware immediately, so a
        // restart resumes the program the operator chose instead of leaving
        // the cooler on whatever the firmware defaulted to.
        daemon.restore_active_profile();

        Ok(daemon)
    }

    /// Re-apply the configured profile after a start.
    ///
    /// A refusal is recorded and the daemon carries on: an incompatible or
    /// unwritable profile must never keep the service from coming up.
    fn restore_active_profile(&mut self) {
        let profile = self.config.active_profile();
        self.apply_profile_lighting(&profile);
        self.apply_profile_display(&profile);

        if profile.program == CoolingProgram::Onboard {
            return;
        }
        let hardware = match self.execute(&profile.program) {
            Ok(outcome) => outcome.hardware,
            Err(error) => HardwareState::NotApplied {
                reason: error.to_string(),
            },
        };
        self.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::ProfileActivated {
                name: profile.name,
                hardware,
            },
        );
    }

    /// Put every lighting channel a profile names where the profile wants it.
    ///
    /// A refusal is recorded and the next channel is still attempted: a
    /// controller that is read-only must not keep a cooling profile from being
    /// restored, and a channel that is gone must not hide the ones that remain.
    /// Put the panel where the profile wants it, when the profile sets it.
    ///
    /// A refusal is recorded and the daemon carries on, for the same reason a
    /// refused lighting channel does not stop a cooling profile.
    fn apply_profile_display(&mut self, profile: &Profile) {
        let Some(preset) = profile.display.clone() else {
            return;
        };
        if let Err(error) = self.show(&preset) {
            self.diagnostics.record(
                crate::now_unix_ms(),
                EventKind::DisplayApplied {
                    mode: preset.mode.key().to_string(),
                    hardware: HardwareState::NotApplied {
                        reason: error.to_string(),
                    },
                    frames: 0,
                },
            );
        }
    }

    fn apply_profile_lighting(&mut self, profile: &Profile) {
        for command in profile.lighting.clone() {
            if let Err(error) = self.illuminate(&command) {
                self.diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::LightingApplied {
                        channel: command.channel,
                        program: command.program.summary(),
                        hardware: HardwareState::NotApplied {
                            reason: error.to_string(),
                        },
                        writes: 0,
                    },
                );
            }
        }
    }

    pub fn capabilities(&self) -> &CapabilityRecord {
        &self.capabilities
    }

    /// Devices this daemon holds the exclusive lock for.
    pub fn locked_devices(&self) -> Vec<DeviceId> {
        self.locks.iter().map(DeviceLock::device).collect()
    }

    pub fn diagnostics_mut(&mut self) -> &mut DiagnosticsLog {
        &mut self.diagnostics
    }

    /// Read-only unless ownership is complete and something is writable.
    pub fn access_mode(&self) -> AccessMode {
        if self.conflicts.is_empty() && self.has_writable_capability() {
            AccessMode::ReadWrite
        } else {
            let mut conflicts = self.conflicts.clone();
            if conflicts.is_empty() {
                conflicts.push(OwnershipConflict {
                    device: None,
                    resource: "hwmon".to_string(),
                    detail: "No writable control attribute is available to this user. \
                             Check the installed udev rule."
                        .to_string(),
                });
            }
            AccessMode::ReadOnly { conflicts }
        }
    }

    fn read_only_reason(&self) -> Option<String> {
        match self.access_mode() {
            AccessMode::ReadWrite => None,
            AccessMode::ReadOnly { conflicts } => Some(
                conflicts
                    .iter()
                    .map(|conflict| conflict.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
        }
    }

    fn has_writable_capability(&self) -> bool {
        self.capabilities
            .supported()
            .any(|device| device.capabilities.iter().any(|c| c.state.is_writable()))
    }

    fn device_status(&self, device: &DeviceRecord) -> DeviceStatus {
        let mut writable = Vec::new();
        let mut blocked = Vec::new();
        for capability in &device.capabilities {
            match capability.state.blocked_reason() {
                None => writable.push(capability.id),
                Some(reason) => blocked.push(BlockedCapability {
                    capability: capability.id,
                    reason,
                }),
            }
        }

        DeviceStatus {
            id: device.id(),
            present: true,
            owned: self
                .conflicts
                .iter()
                .all(|conflict| conflict.device != Some(device.id())),
            writable,
            blocked,
        }
    }

    pub fn status(&self) -> DaemonStatus {
        DaemonStatus {
            daemon_version: DAEMON_VERSION.to_string(),
            protocol_version: PROTOCOL_VERSION,
            access: self.access_mode(),
            devices: self
                .capabilities
                .supported()
                .map(|device| self.device_status(device))
                .collect(),
            active_profile: self.config.active_profile_name().to_string(),
            config: self.config.state().clone(),
            lighting: self.lighting.state(),
            display: self.display.state(),
            socket_path: self.paths.socket.display().to_string(),
        }
    }

    /// Handle one typed request.
    ///
    /// Every rejection path returns before anything is written, so an invalid
    /// request cannot reach the hardware or the configuration file.
    pub fn handle(&mut self, request: Request) -> Response {
        let response = self.dispatch(request);
        if let Response::Error(error) = &response {
            self.diagnostics.record(
                crate::now_unix_ms(),
                EventKind::RequestRejected {
                    error: error.clone(),
                },
            );
        }
        response
    }

    fn dispatch(&mut self, request: Request) -> Response {
        match request {
            Request::Hello { protocol_version } => {
                if protocol_version != PROTOCOL_VERSION {
                    return Response::Error(IpcError::UnsupportedProtocol {
                        requested: protocol_version,
                        supported: PROTOCOL_VERSION,
                    });
                }
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_version: DAEMON_VERSION.to_string(),
                }
            }
            Request::Status => Response::Status(Box::new(self.status())),
            Request::Capabilities => Response::Capabilities(Box::new(self.capabilities.clone())),
            Request::Profiles => Response::Profiles {
                active: self.config.active_profile_name().to_string(),
                profiles: self.config.profiles(),
            },
            Request::SaveProfile { profile } => self.save_profile(profile),
            Request::ActivateProfile { name } => self.activate_profile(&name),
            Request::DeleteProfile { name } => self.delete_profile(&name),
            Request::Telemetry => Response::Telemetry(Box::new(self.telemetry())),
            Request::ApplyProgram { program } => match self.execute(&program) {
                Ok(outcome) => Response::Applied(Box::new(outcome)),
                Err(error) => Response::Error(error),
            },
            Request::ApplyLighting { command } => match self.illuminate(&command) {
                Ok(outcome) => Response::Lit(outcome),
                Err(error) => Response::Error(error),
            },
            Request::ApplyDisplay { preset } => match self.show(&preset) {
                Ok(outcome) => Response::Shown(Box::new(outcome)),
                Err(error) => Response::Error(error),
            },
            Request::Diagnostics => Response::Diagnostics(self.diagnostics.export(
                crate::now_unix_ms(),
                DAEMON_VERSION,
                Some(self.capabilities.clone()),
            )),
        }
    }

    /// The latest sampling pass, with any change in collector health logged.
    pub fn telemetry(&mut self) -> TelemetrySnapshot {
        let snapshot = self.sampler.snapshot();

        // A device that has gone away invalidates the executor's record of
        // what it committed. Without this, an Apply after a reconnect could
        // deduplicate against a program the hardware no longer holds and
        // silently write nothing.
        if !snapshot.kraken.present {
            self.cooling.forget();
        }

        for failure in &snapshot.failed {
            if !self.reported_failures.contains(failure) {
                self.diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::CollectorFailed {
                        collector: failure.collector,
                        detail: failure.detail.clone(),
                    },
                );
            }
        }
        for previous in &self.reported_failures {
            if !snapshot
                .failed
                .iter()
                .any(|failure| failure.collector == previous.collector)
            {
                self.diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::CollectorRecovered {
                        collector: previous.collector,
                    },
                );
            }
        }
        self.reported_failures = snapshot.failed.clone();

        snapshot
    }

    /// Gate a cooling program, then write it.
    ///
    /// Every refusal happens before the executor is reached, so a rejected
    /// request cannot produce a partial write. The order matters: values are
    /// validated first because an out-of-range duty is wrong whatever the
    /// device reports, then the capability record, then ownership.
    fn execute(&mut self, program: &CoolingProgram) -> Result<ApplyOutcome, IpcError> {
        validate_program(program)?;

        if *program == CoolingProgram::Onboard {
            // Nothing is written, so no capability is required. This is the
            // program the daemon falls back to, and it must never be refusable.
            return Ok(self.cooling.apply(Instant::now(), program));
        }

        {
            let device = self
                .capabilities
                .device(KRAKEN_BASE)
                .filter(|device| device.is_supported())
                .ok_or(IpcError::NoDevice)?;
            let details = program_incompatibilities(program, device);
            if !details.is_empty() {
                return Err(IpcError::Incompatible { details });
            }
        }

        if let AccessMode::ReadOnly { conflicts } = self.access_mode() {
            return Err(IpcError::ReadOnly {
                reason: conflicts
                    .iter()
                    .map(|conflict| conflict.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            });
        }

        let outcome = self.cooling.apply(Instant::now(), program);
        self.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::ProgramApplied {
                hardware: outcome.hardware.clone(),
                writes: outcome.writes,
            },
        );
        Ok(outcome)
    }

    /// Gate a lighting command, then send it.
    ///
    /// The order mirrors [`Daemon::execute`] and matters for the same reason.
    /// Values are checked first because a malformed color is wrong whatever the
    /// controller reports. The topology is checked next, because a channel that
    /// does not exist must be refused before any capability question. Then the
    /// capability record, which is what carries the "this firmware was never
    /// validated" refusal. Then ownership. The executor is reached last, so a
    /// rejected request cannot produce a partial write.
    fn illuminate(&mut self, command: &LightingCommand) -> Result<LightingOutcome, IpcError> {
        validate_command(command, self.lighting.channel_count())?;

        {
            let device = self
                .capabilities
                .device(RGB_CONTROLLER)
                .filter(|device| device.is_supported())
                .ok_or(IpcError::NoDevice)?;
            let capability = command.program.required_capability();
            if !device.can_write(capability) {
                return Err(IpcError::Incompatible {
                    details: vec![Incompatibility {
                        capability,
                        reason: device
                            .capability(capability)
                            .and_then(|entry| entry.state.blocked_reason())
                            .unwrap_or_else(|| {
                                format!(
                                    "{} is absent from the capability record.",
                                    capability.label()
                                )
                            }),
                    }],
                });
            }
        }

        if let AccessMode::ReadOnly { conflicts } = self.access_mode() {
            return Err(IpcError::ReadOnly {
                reason: conflicts
                    .iter()
                    .map(|conflict| conflict.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            });
        }

        let outcome = self.lighting.apply(Instant::now(), command)?;
        self.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::LightingApplied {
                channel: outcome.channel,
                program: outcome.program.summary(),
                hardware: outcome.hardware.clone(),
                writes: outcome.writes,
            },
        );
        Ok(outcome)
    }

    /// Gate a panel preset, then render and send it.
    ///
    /// The order mirrors [`Daemon::illuminate`] and matters for the same
    /// reason. The preset is validated first because a mode with no file is
    /// wrong whatever the panel reports. Then the capability record, which is
    /// what carries the "this firmware was never validated" refusal and the
    /// "there may be no panel behind this cap" one. Then ownership. The
    /// executor is reached last, so a rejected request renders nothing and
    /// sends nothing.
    fn show(&mut self, preset: &DisplayPreset) -> Result<DisplayOutcome, IpcError> {
        preset.validate()?;

        {
            let device = self
                .capabilities
                .device(KRAKEN_BASE)
                .filter(|device| device.is_supported())
                .ok_or(IpcError::NoDevice)?;
            for capability in [CapabilityId::LcdFrame, CapabilityId::LcdDisplayControl] {
                if !device.can_write(capability) {
                    return Err(IpcError::Incompatible {
                        details: vec![Incompatibility {
                            capability,
                            reason: device
                                .capability(capability)
                                .and_then(|entry| entry.state.blocked_reason())
                                .unwrap_or_else(|| {
                                    format!(
                                        "{} is absent from the capability record.",
                                        capability.label()
                                    )
                                }),
                        }],
                    });
                }
            }
        }

        if let AccessMode::ReadOnly { conflicts } = self.access_mode() {
            return Err(IpcError::ReadOnly {
                reason: conflicts
                    .iter()
                    .map(|conflict| conflict.detail.clone())
                    .collect::<Vec<_>>()
                    .join(" "),
            });
        }

        let samples = preset.samples(&self.sampler.snapshot());
        let outcome = self.display.apply(preset, &samples)?;
        self.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::DisplayApplied {
                mode: preset.mode.key().to_string(),
                hardware: outcome.hardware.clone(),
                frames: outcome.frames,
            },
        );
        Ok(outcome)
    }

    /// Redraw the panel from the latest sample.
    ///
    /// Called once a second by the server's own ticker, never by a client, so
    /// the panel keeps its readings current with the window closed. A tick that
    /// finds nothing to do costs one render and no transfer, because the
    /// executor compares the picture it produced against the one the panel
    /// already holds.
    pub fn tick_display(&mut self) {
        let Some(preset) = self.display.active().cloned() else {
            return;
        };
        let samples = preset.samples(&self.sampler.snapshot());
        if let Some(outcome) = self.display.refresh(&samples)
            && outcome.frames > 0
        {
            self.diagnostics.record(
                crate::now_unix_ms(),
                EventKind::DisplayApplied {
                    mode: preset.mode.key().to_string(),
                    hardware: outcome.hardware,
                    frames: outcome.frames,
                },
            );
        }
    }

    /// Record a sample that arrived while a frame was still being written.
    pub fn drop_display_frame(&mut self) {
        self.display.drop_frame();
    }

    fn save_profile(&mut self, profile: Profile) -> Response {
        let name = profile.name.clone();
        match self.config.save_profile(profile) {
            Ok(()) => {
                self.diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::ProfileSaved { name: name.clone() },
                );
                Response::Saved { name }
            }
            Err(error) => Response::Error(config_error(error)),
        }
    }

    /// Devices a profile applies to.
    ///
    /// A profile pinned to one device matches only that device. An unpinned
    /// profile matches every device that exposes at least one capability its
    /// program needs, so a cooling profile is never checked against the RGB
    /// controller.
    fn targets_for(&self, profile: &Profile) -> Vec<&DeviceRecord> {
        let required = profile.program.required_capabilities();
        self.capabilities
            .supported()
            .filter(|device| match profile.device {
                Some(id) => device.id() == id,
                None => required
                    .iter()
                    .any(|capability| device.capability(*capability).is_some()),
            })
            .collect()
    }

    fn activate_profile(&mut self, name: &str) -> Response {
        let Some(profile) = self.config.profile(name) else {
            return Response::Error(IpcError::ProfileNotFound {
                name: name.to_string(),
            });
        };

        // Compatibility is checked against every supported device before the
        // selection is persisted, so an impossible profile never becomes
        // active.
        let onboard = profile.program == CoolingProgram::Onboard;
        if !onboard {
            let targets = self.targets_for(&profile);
            if targets.is_empty() {
                return Response::Error(IpcError::NoDevice);
            }

            let details: Vec<_> = targets
                .iter()
                .flat_map(|device| incompatibilities(&profile, device))
                .collect();
            if !details.is_empty() {
                return Response::Error(IpcError::Incompatible { details });
            }
            if let AccessMode::ReadOnly { conflicts } = self.access_mode() {
                return Response::Error(IpcError::ReadOnly {
                    reason: conflicts
                        .iter()
                        .map(|c| c.detail.clone())
                        .collect::<Vec<_>>()
                        .join(" "),
                });
            }
        }

        match self.config.activate(name) {
            Ok(profile) => {
                self.apply_profile_lighting(&profile);
                self.apply_profile_display(&profile);
                // The selection is persisted first, then written. A write that
                // fails leaves the profile selected and its hardware state
                // reported honestly, rather than silently reverting a choice
                // the operator made.
                let applied = match self.execute(&profile.program) {
                    Ok(outcome) => outcome,
                    Err(error) => ApplyOutcome::untouched(HardwareState::NotApplied {
                        reason: error.to_string(),
                    }),
                };
                self.diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::ProfileActivated {
                        name: profile.name.clone(),
                        hardware: applied.hardware.clone(),
                    },
                );
                Response::Activated(ActivationOutcome {
                    name: profile.name,
                    hardware: applied.hardware.clone(),
                    applied: Some(applied),
                })
            }
            Err(error) => Response::Error(config_error(error)),
        }
    }

    fn delete_profile(&mut self, name: &str) -> Response {
        match self.config.delete_profile(name) {
            Ok(activated_instead) => {
                self.diagnostics.record(
                    crate::now_unix_ms(),
                    EventKind::ProfileDeleted {
                        name: name.to_string(),
                    },
                );
                Response::Deleted {
                    name: name.to_string(),
                    activated_instead,
                }
            }
            Err(error) => Response::Error(config_error(error)),
        }
    }

    /// Record a client that failed the local peer check.
    pub fn record_client_rejected(&mut self, reason: String) {
        self.diagnostics
            .record(crate::now_unix_ms(), EventKind::ClientRejected { reason });
    }

    pub fn record_client_accepted(&mut self, uid: u32, pid: i32) {
        self.diagnostics
            .record(crate::now_unix_ms(), EventKind::ClientAccepted { uid, pid });
    }

    pub fn record_client_disconnected(&mut self, detail: String) {
        self.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::ClientDisconnected { detail },
        );
    }
}

/// Ask the controller what it is, fold the answer into the record, and keep the
/// handle only when it answered.
///
/// A controller that could not be opened, or that stayed silent, produces an
/// evidenced unknown in the record and an executor with nothing behind it. That
/// is what keeps "the topology is unknown" and "the write is refused" the same
/// fact rather than two that could drift apart.
fn connect_lighting(capabilities: &mut CapabilityRecord, backend: RgbBackend) -> LightingExecutor {
    use nzxt_hardware_linux::rgb::{self, HidTransport};

    let (topology, transport): (_, Option<Box<dyn HidTransport>>) = match backend {
        RgbBackend::None => return LightingExecutor::absent(),
        RgbBackend::Hidraw => {
            let node = capabilities
                .device(nzxt_core::RGB_CONTROLLER)
                .filter(|device| device.is_supported())
                .and_then(|device| {
                    nzxt_hardware_linux::usb::hidraw_node(std::path::Path::new(
                        &device.usb.sysfs_path,
                    ))
                });
            let (topology, link) = rgb::connect(node);
            (
                topology,
                link.map(|link| Box::new(link) as Box<dyn HidTransport>),
            )
        }
        RgbBackend::Transport(mut transport) => {
            let topology = rgb::inspect(transport.as_mut());
            let answered = topology.channels.is_known();
            (topology, answered.then_some(transport))
        }
    };

    let channels = topology.channels.value().cloned().unwrap_or_default();
    nzxt_hardware_linux::probe::attach_rgb_topology(capabilities, topology);

    // A controller reporting zero channels is not a controller this daemon can
    // address, so it holds no handle to it.
    match transport {
        Some(transport) if !channels.is_empty() => {
            LightingExecutor::connected(transport, &channels)
        }
        _ => LightingExecutor::absent(),
    }
}

/// Ask the Kraken what its panel is, fold the answer into the record, and keep
/// the handle only when a panel answered *and* the bulk interface was claimed.
///
/// Half a transport is worse than none: a link that could send a display
/// command but not a frame would let the daemon dim a panel it cannot draw on.
fn connect_display(capabilities: &mut CapabilityRecord, backend: LcdBackend) -> DisplayExecutor {
    use nzxt_hardware_linux::lcd;

    let (topology, link): (LcdTopology, _) = match backend {
        LcdBackend::None => return DisplayExecutor::absent(),
        LcdBackend::Nodes => {
            let device = capabilities
                .device(KRAKEN_BASE)
                .filter(|device| device.is_supported())
                .map(|device| std::path::PathBuf::from(&device.usb.sysfs_path));
            let node = device
                .as_deref()
                .and_then(nzxt_hardware_linux::usb::hidraw_node);
            lcd::connect(device.as_deref(), node)
        }
        LcdBackend::Link(mut link) => {
            let inventory = link.inventory().ok().unwrap_or_default();
            let topology = lcd::topology_from(&inventory, &link.source(), Some(&link.source()));
            let answered = topology.answered();
            (topology, answered.then_some(*link))
        }
    };

    let panel = topology.panel.value().cloned();
    nzxt_hardware_linux::probe::attach_lcd_topology(capabilities, topology);

    match (link, panel) {
        (Some(link), Some(panel)) => DisplayExecutor::connected(link, panel),
        _ => DisplayExecutor::absent(),
    }
}

fn config_error(error: ConfigError) -> IpcError {
    match error {
        ConfigError::InvalidProfile { source, .. } => IpcError::Validation(source),
        ConfigError::MissingActiveProfile(name) => IpcError::ProfileNotFound { name },
        ConfigError::DuplicateProfile(name) => IpcError::ProfileNameUnavailable { name },
        other => IpcError::Io {
            detail: other.to_string(),
        },
    }
}
