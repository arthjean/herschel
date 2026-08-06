// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The daemon's authoritative state and the handler for every typed request.
//!
//! One instance owns the capability record, the per-device locks and the
//! configuration. The server wraps it in a mutex, so requests are serialised:
//! two clients can never interleave a command.

use std::time::Duration;

use nzxt_core::capability::{CapabilityRecord, DeviceRecord};
use nzxt_core::diagnostics::{DiagnosticsLog, EventKind};
use nzxt_core::ipc::{
    AccessMode, ActivationOutcome, ApplyOutcome, BlockedCapability, ConfigState, DaemonStatus,
    DeviceStatus, HardwareState, IpcError, OwnershipConflict, PROTOCOL_VERSION, Request, Response,
};
use nzxt_core::profile::{
    CoolingProgram, Profile, incompatibilities, program_incompatibilities, validate_program,
};
use nzxt_core::telemetry::{CollectorFailure, TelemetrySnapshot};
use nzxt_core::{DeviceId, KRAKEN_BASE, capability::Evidenced};
use nzxt_hardware_linux::SysfsRoot;
use nzxt_hardware_linux::probe::probe;

use crate::config::{ConfigError, Configuration};
use crate::cooling::CoolingExecutor;
use crate::ownership::{self, DeviceLock};
use crate::paths::Paths;
use crate::telemetry::{Sampler, default_interval};

/// Version reported to clients and diagnostics.
pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

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
        )
    }

    /// Start against explicit sources and an explicit sampling interval.
    ///
    /// Only the sources and the cadence change; every other behaviour is
    /// identical, so a test exercises the same paths without waiting on real
    /// seconds or mutating a process-global variable.
    pub fn start_with(
        paths: Paths,
        sysfs: &SysfsRoot,
        proc_root: &std::path::Path,
        interval: Duration,
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

        let capabilities = probe(sysfs);
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
            return Ok(self.cooling.apply(program));
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

        let outcome = self.cooling.apply(program);
        self.diagnostics.record(
            crate::now_unix_ms(),
            EventKind::ProgramApplied {
                hardware: outcome.hardware.clone(),
                writes: outcome.writes,
            },
        );
        Ok(outcome)
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
