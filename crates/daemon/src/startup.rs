// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Assembling one daemon out of one machine.
//!
//! Everything here runs once, before the first request is served: ask the two
//! devices what they are, log what was found, take what ownership is available,
//! and load the configuration. It is separated from [`crate::state`] because a
//! reader following a command through the daemon does not need to know how an
//! `LcdLink` gets opened, and a reader working out what this process claims at
//! startup does not need the request handlers in the way.
//!
//! Nothing here fails a startup. A device that is absent, silent or already held
//! by someone else produces an evidenced unknown and a conflict to report, and
//! the daemon comes up read-only rather than not at all.

use kori_core::capability::{CapabilityRecord, Evidenced, LcdTopology};
use kori_core::diagnostics::{DiagnosticsLog, EventKind};
use kori_core::ipc::{ConfigState, OwnershipConflict};
use kori_core::{KRAKEN_BASE, RGB_CONTROLLER};

use crate::config::Configuration;
use crate::display::DisplayExecutor;
use crate::lighting::LightingExecutor;
use crate::ownership::{self, DeviceLock};
use crate::paths::Paths;

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
    Transport(Box<dyn kori_hardware_linux::rgb::HidTransport>),
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
    Link(Box<kori_hardware_linux::lcd::LcdLink>),
    /// Do not talk to the panel at all.
    None,
}

/// Log everything the probe found, before ownership is asked for.
///
/// Serials are registered as secrets on the way past, so a later export cannot
/// carry one whatever it is asked to include.
pub(crate) fn record_discovery(diagnostics: &mut DiagnosticsLog, capabilities: &CapabilityRecord) {
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
    }
}

/// Take the exclusive lock on every supported device, and observe who else
/// already holds one of their nodes.
///
/// A conflict is never a startup failure: it is returned so the daemon can come
/// up read-only and say who is in the way.
pub(crate) fn take_ownership(
    paths: &Paths,
    capabilities: &CapabilityRecord,
    diagnostics: &mut DiagnosticsLog,
) -> (Vec<DeviceLock>, Vec<OwnershipConflict>) {
    let mut locks = Vec::new();
    let mut conflicts = Vec::new();

    for device in capabilities.supported() {
        match ownership::acquire(paths, device.id()) {
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

    (locks, conflicts)
}

/// Load the configuration, recording whether it came back whole.
pub(crate) fn load_configuration(paths: &Paths, diagnostics: &mut DiagnosticsLog) -> Configuration {
    let config = Configuration::load(paths.config_file());
    let event = match config.state() {
        ConfigState::Recovered {
            detail,
            preserved_path,
            ..
        } => EventKind::ConfigRecovered {
            detail: detail.clone(),
            preserved_path: preserved_path.clone(),
        },
        _ => EventKind::ConfigLoaded {
            path: config.path().display().to_string(),
        },
    };
    diagnostics.record(crate::now_unix_ms(), event);
    config
}

/// Ask the controller what it is, fold the answer into the record, and keep the
/// handle only when it answered.
///
/// A controller that could not be opened, or that stayed silent, produces an
/// evidenced unknown in the record and an executor with nothing behind it. That
/// is what keeps "the topology is unknown" and "the write is refused" the same
/// fact rather than two that could drift apart.
pub(crate) fn connect_lighting(
    capabilities: &mut CapabilityRecord,
    backend: RgbBackend,
) -> LightingExecutor {
    use kori_hardware_linux::rgb::{self, HidTransport};

    let (topology, transport): (_, Option<Box<dyn HidTransport>>) = match backend {
        RgbBackend::None => return LightingExecutor::absent(),
        RgbBackend::Hidraw => {
            let node = kori_hardware_linux::probe::hidraw_node(capabilities, RGB_CONTROLLER);
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
    kori_hardware_linux::probe::attach_rgb_topology(capabilities, topology);

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
pub(crate) fn connect_display(
    capabilities: &mut CapabilityRecord,
    backend: LcdBackend,
) -> DisplayExecutor {
    use kori_hardware_linux::lcd;

    let (topology, link): (LcdTopology, _) = match backend {
        LcdBackend::None => return DisplayExecutor::absent(),
        LcdBackend::Nodes => {
            let device = kori_hardware_linux::probe::device_path(capabilities, KRAKEN_BASE);
            let node = device
                .as_deref()
                .and_then(kori_hardware_linux::usb::hidraw_node);
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
    kori_hardware_linux::probe::attach_lcd_topology(capabilities, topology);

    match (link, panel) {
        (Some(link), Some(panel)) => DisplayExecutor::connected(link, panel),
        _ => DisplayExecutor::absent(),
    }
}
