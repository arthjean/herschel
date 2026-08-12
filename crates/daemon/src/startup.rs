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
    Transport(Box<dyn kori_hardware_linux::hid::HidTransport>),
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
/// up read-only and say who is in the way. Every device node is collected first
/// and looked up in one pass, because the lookup walks every process this user
/// can read and that walk does not get cheaper by being repeated per device.
pub(crate) fn take_ownership(
    paths: &Paths,
    capabilities: &CapabilityRecord,
    diagnostics: &mut DiagnosticsLog,
) -> (Vec<DeviceLock>, Vec<OwnershipConflict>) {
    let mut locks = Vec::new();
    let mut conflicts = Vec::new();
    let mut nodes = Vec::new();

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
            Err(conflict) => conflicts.push(conflict),
        }

        nodes.extend(
            ownership::hid_nodes(std::path::Path::new(&device.usb.sysfs_path))
                .into_iter()
                .map(|node| (node, device.id())),
        );
    }

    // A program already holding a device's HID node is a competing writer. It
    // is reported, never forced away.
    conflicts.extend(ownership::observed_holders(&nodes));

    for conflict in &conflicts {
        diagnostics.record(
            crate::now_unix_ms(),
            EventKind::OwnershipConflict {
                device: conflict.device,
                resource: conflict.resource.clone(),
                detail: conflict.detail.clone(),
            },
        );
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

/// Ask the controller over its `hidraw` node what it is, and fold the answer
/// into the record.
///
/// Both requests carry no color and no mode, so asking changes nothing the
/// operator can see. The handle is handed back because the daemon keeps it;
/// `--rgb-probe` drops it and keeps only the record. Both go through here so the
/// evidence a read-only run prints is built by the same steps the daemon takes,
/// rather than by a second sequence that happens to agree.
pub fn ask_rgb(
    capabilities: &mut CapabilityRecord,
) -> (
    Vec<kori_core::capability::RgbChannel>,
    Option<Box<dyn kori_hardware_linux::hid::HidTransport>>,
) {
    use kori_hardware_linux::hid::HidTransport;

    let node = kori_hardware_linux::probe::hidraw_node(capabilities, RGB_CONTROLLER);
    let (topology, link) = kori_hardware_linux::rgb::connect(node);
    let channels = topology.channels.value().cloned().unwrap_or_default();
    kori_hardware_linux::probe::attach_rgb_topology(capabilities, topology);
    (
        channels,
        link.map(|link| Box::new(link) as Box<dyn HidTransport>),
    )
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
    use kori_hardware_linux::hid::HidTransport;
    use kori_hardware_linux::rgb;

    let (channels, transport): (_, Option<Box<dyn HidTransport>>) = match backend {
        RgbBackend::None => return LightingExecutor::absent(),
        RgbBackend::Hidraw => ask_rgb(capabilities),
        RgbBackend::Transport(mut transport) => {
            let topology = rgb::inspect(transport.as_mut());
            let channels = topology.channels.value().cloned().unwrap_or_default();
            let answered = topology.channels.is_known();
            kori_hardware_linux::probe::attach_rgb_topology(capabilities, topology);
            (channels, answered.then_some(transport))
        }
    };

    // A controller reporting zero channels is not a controller this daemon can
    // address, so it holds no handle to it.
    match transport {
        Some(transport) if !channels.is_empty() => {
            LightingExecutor::connected(transport, &channels)
        }
        _ => LightingExecutor::absent(),
    }
}

/// Ask the Kraken over its own nodes what its panel is, and fold the answer into
/// the record.
///
/// Two query reports that carry no picture, no brightness and no orientation, so
/// asking changes nothing the operator can see. The handle comes back for the
/// same reason [`ask_rgb`] hands one back: the daemon keeps it and `--lcd-probe`
/// drops it, and neither builds the record its own way.
pub fn ask_lcd(
    capabilities: &mut CapabilityRecord,
) -> (
    Option<kori_core::capability::LcdPanel>,
    Option<kori_hardware_linux::lcd::LcdLink>,
) {
    let device = kori_hardware_linux::probe::device_path(capabilities, KRAKEN_BASE);
    let node = device
        .as_deref()
        .and_then(kori_hardware_linux::usb::hidraw_node);
    let (topology, link) = kori_hardware_linux::lcd::connect(device.as_deref(), node);
    let panel = topology.panel.value().cloned();
    kori_hardware_linux::probe::attach_lcd_topology(capabilities, topology);
    (panel, link)
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
    let (panel, link) = match backend {
        LcdBackend::None => return DisplayExecutor::absent(),
        LcdBackend::Nodes => ask_lcd(capabilities),
        LcdBackend::Link(mut link) => {
            // The link builds its own record. It knows which of its two sources
            // is the display node and which is the framebuffer's, and filling
            // both fields from the fused `source()` recorded two nodes that
            // neither existed nor were opened.
            let topology: LcdTopology = link.topology();
            let panel = topology.panel.value().cloned();
            let answered = topology.answered();
            kori_hardware_linux::probe::attach_lcd_topology(capabilities, topology);
            (panel, answered.then_some(*link))
        }
    };

    match (link, panel) {
        (Some(link), Some(panel)) => DisplayExecutor::connected(link, panel),
        _ => DisplayExecutor::absent(),
    }
}
