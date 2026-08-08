// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Entry point of the hardware daemon.
//!
//! Refuses to run as root, probes the machine, takes what ownership it can and
//! serves the local socket. Every failure path exits with a diagnostic instead
//! of a panic.

use std::process::ExitCode;

use nzxt_core::lighting::{Brightness, LightingProgram, Rgb};
use nzxt_core::{KRAKEN_BASE, RGB_CONTROLLER};
use nzxt_daemon::rgb_probe::ProbeScope;
use nzxt_daemon::state::Daemon;
use nzxt_daemon::{DAEMON_VERSION, Paths, Server};
use nzxt_hardware_linux::SysfsRoot;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("nzxt-controld: {message}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
Usage: nzxt-controld [OPTIONS]

Options:
  --capabilities     Print the versioned capability record as JSON and exit.
                     Read-only: no socket is opened and no device is written.
  --rgb-probe        The same record, with the RGB controller's topology read
                     from the device. Asks the controller what it is; sends no
                     color, no mode and no parameter, so it changes nothing.
  --rgb-write-probe  US-013's write probe. Sends lighting commands this product
                     has not validated on this firmware, one channel at a time,
                     and refuses to start without a typed confirmation. Prints
                     the evidence record as JSON on stdout.
      --with-effects   Also test the two animated candidates (US-015).
      --sweep-effects  Instead, sweep every effect speed and direction the
                       Lighting screen can select, on the first channel only.
                       This is what evidences the selectable values (US-015).
      --restore HEX    Leave every channel on this color instead of off.
  --probe            The capability record with both devices asked what they
                     are. This is what re-records docs/capability-record.json,
                     because both topologies belong in it. Read-only: every
                     report it sends is a query.
  --lcd-probe        The same, for the Kraken's panel alone. Sends two query reports that carry no picture, no
                     brightness and no orientation, so it changes nothing. A
                     device with no panel answers the second one not at all,
                     which is how this tells the two apart.
  --lcd-write-probe  US-016's write probe. Sends solid frames this product has
                     not validated on this firmware, one color at a time, and
                     refuses to start without a typed confirmation. Prints the
                     evidence record as JSON on stdout.
  --version          Print the version and exit.
  --help             Print this message.
";

fn run() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        None => serve(),
        Some("--capabilities") => print_capabilities(),
        Some("--rgb-probe") => print_rgb_capabilities(),
        Some("--rgb-write-probe") => write_probe(&arguments[1..]),
        Some("--probe") => print_full_capabilities(),
        Some("--lcd-probe") => print_lcd_capabilities(),
        Some("--lcd-write-probe") => lcd_write_probe(),
        Some("--version") => {
            println!("nzxt-controld {DAEMON_VERSION}");
            Ok(())
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(unknown) => Err(format!("unknown argument {unknown}\n\n{USAGE}")),
    }
}

/// The capability record with the controller's own answer folded in.
///
/// Separate from `--capabilities` because this one opens a device node. The
/// two requests it sends are queries, so the controller's state is untouched,
/// but the distinction between "read sysfs" and "talk to the device" is worth
/// keeping in the command line rather than hiding behind one flag.
fn print_rgb_capabilities() -> Result<(), String> {
    let sysfs = SysfsRoot::from_env();
    let mut record = nzxt_hardware_linux::probe(&sysfs);

    let node = record
        .device(RGB_CONTROLLER)
        .filter(|device| device.is_supported())
        .and_then(|device| {
            nzxt_hardware_linux::usb::hidraw_node(std::path::Path::new(&device.usb.sysfs_path))
        });
    let (topology, _link) = nzxt_hardware_linux::rgb::connect(node);
    nzxt_hardware_linux::probe::attach_rgb_topology(&mut record, topology);

    record.redact_serials();
    let json = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("could not encode the capability record: {error}"))?;
    println!("{json}");
    Ok(())
}

/// Run US-013's write probe against the connected controller.
///
/// The per-device lock is taken first and held for the whole run, so the probe
/// and the daemon can never write to the controller at the same time. Refusing
/// when the daemon holds it is the point: two writers is the failure this
/// product exists to prevent.
fn write_probe(arguments: &[String]) -> Result<(), String> {
    if rustix::process::geteuid().is_root() {
        return Err("refusing to run as root. Install the udev rule instead.".to_string());
    }

    let mut scope = ProbeScope::FixedAndOff;
    let mut restore = LightingProgram::Off;
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--with-effects" => scope = ProbeScope::WithEffects,
            "--sweep-effects" => scope = ProbeScope::EffectSweep,
            "--restore" => {
                let value = rest
                    .next()
                    .ok_or_else(|| "--restore needs a six-digit color".to_string())?;
                restore = LightingProgram::Fixed {
                    color: Rgb::parse_hex(value).map_err(|error| error.to_string())?,
                    brightness: Brightness::new(60).map_err(|error| error.to_string())?,
                };
            }
            unknown => return Err(format!("unknown argument {unknown}\n\n{USAGE}")),
        }
    }

    let sysfs = SysfsRoot::from_env();
    let record = nzxt_hardware_linux::probe(&sysfs);
    let device = record
        .device(RGB_CONTROLLER)
        .filter(|device| device.is_supported())
        .ok_or_else(|| format!("{RGB_CONTROLLER} is not connected to this machine"))?;

    let paths = Paths::from_env();
    paths
        .ensure()
        .map_err(|error| format!("could not prepare {}: {error}", paths.runtime_dir.display()))?;
    let _lock = nzxt_daemon::ownership::acquire(&paths, RGB_CONTROLLER).map_err(|conflict| {
        format!(
            "another process owns the controller ({}). Stop nzxt-controld and try again.",
            conflict.detail
        )
    })?;

    let node = nzxt_hardware_linux::usb::hidraw_node(std::path::Path::new(&device.usb.sysfs_path));
    let (topology, link) = nzxt_hardware_linux::rgb::connect(node);
    let mut link = link.ok_or_else(|| {
        format!(
            "the controller did not answer: {}",
            topology
                .channels
                .reason()
                .unwrap_or("no reason was recorded")
        )
    })?;

    let report = nzxt_daemon::rgb_probe::run(
        &mut link,
        &mut nzxt_daemon::rgb_probe::Terminal,
        &topology,
        scope,
        restore,
    )
    .map_err(|refusal| refusal.to_string())?;

    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("could not encode the probe record: {error}"))?;
    println!("{json}");
    Ok(())
}

/// The capability record with the panel's own answer folded in.
///
/// Separate from `--capabilities` because this one opens two device nodes. The
/// reports it sends are queries, so the panel's content is untouched, and the
/// bulk interface is claimed only to record that it can be.
fn print_lcd_capabilities() -> Result<(), String> {
    let sysfs = SysfsRoot::from_env();
    let mut record = nzxt_hardware_linux::probe(&sysfs);

    let device = record
        .device(KRAKEN_BASE)
        .filter(|device| device.is_supported())
        .map(|device| std::path::PathBuf::from(&device.usb.sysfs_path));
    let node = device
        .as_deref()
        .and_then(nzxt_hardware_linux::usb::hidraw_node);
    let (topology, _link) = nzxt_hardware_linux::lcd::connect(device.as_deref(), node);
    nzxt_hardware_linux::probe::attach_lcd_topology(&mut record, topology);

    record.redact_serials();
    let json = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("could not encode the capability record: {error}"))?;
    println!("{json}");
    Ok(())
}

/// The capability record with both devices asked what they are.
///
/// Separate from the two focused probes because the committed artifact has to
/// carry both topologies: a record re-generated by one of them would drop the
/// other's evidence and look like a device that answered nothing.
fn print_full_capabilities() -> Result<(), String> {
    let sysfs = SysfsRoot::from_env();
    let mut record = nzxt_hardware_linux::probe(&sysfs);

    let rgb_node = record
        .device(RGB_CONTROLLER)
        .filter(|device| device.is_supported())
        .and_then(|device| {
            nzxt_hardware_linux::usb::hidraw_node(std::path::Path::new(&device.usb.sysfs_path))
        });
    let (rgb_topology, _rgb) = nzxt_hardware_linux::rgb::connect(rgb_node);
    nzxt_hardware_linux::probe::attach_rgb_topology(&mut record, rgb_topology);

    let kraken = record
        .device(KRAKEN_BASE)
        .filter(|device| device.is_supported())
        .map(|device| std::path::PathBuf::from(&device.usb.sysfs_path));
    let lcd_node = kraken
        .as_deref()
        .and_then(nzxt_hardware_linux::usb::hidraw_node);
    let (lcd_topology, _lcd) = nzxt_hardware_linux::lcd::connect(kraken.as_deref(), lcd_node);
    nzxt_hardware_linux::probe::attach_lcd_topology(&mut record, lcd_topology);

    record.redact_serials();
    let json = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("could not encode the capability record: {error}"))?;
    println!("{json}");
    Ok(())
}

/// Run US-016's write probe against the connected panel.
///
/// The per-device lock is taken first and held for the whole run, so the probe
/// and the daemon can never address the Kraken at the same time. Refusing when
/// the daemon holds it is the point: two writers is the failure this product
/// exists to prevent.
fn lcd_write_probe() -> Result<(), String> {
    if rustix::process::geteuid().is_root() {
        return Err("refusing to run as root. Install the udev rule instead.".to_string());
    }

    let sysfs = SysfsRoot::from_env();
    let record = nzxt_hardware_linux::probe(&sysfs);
    let device = record
        .device(KRAKEN_BASE)
        .filter(|device| device.is_supported())
        .ok_or_else(|| format!("{KRAKEN_BASE} is not connected to this machine"))?;
    let sysfs_path = std::path::PathBuf::from(&device.usb.sysfs_path);

    let paths = Paths::from_env();
    paths
        .ensure()
        .map_err(|error| format!("could not prepare {}: {error}", paths.runtime_dir.display()))?;
    let _lock = nzxt_daemon::ownership::acquire(&paths, KRAKEN_BASE).map_err(|conflict| {
        format!(
            "another process owns the Kraken ({}). Stop nzxt-controld and try again.",
            conflict.detail
        )
    })?;

    let node = nzxt_hardware_linux::usb::hidraw_node(&sysfs_path);
    let (topology, link) = nzxt_hardware_linux::lcd::connect(Some(&sysfs_path), node);
    let mut link = link.ok_or_else(|| {
        format!(
            "the panel is not reachable: {}",
            topology
                .display
                .reason()
                .or_else(|| topology.bulk_node.reason())
                .or_else(|| topology.hid_node.reason())
                .unwrap_or("no reason was recorded")
        )
    })?;

    let report =
        nzxt_daemon::lcd_probe::run(&mut link, &mut nzxt_daemon::lcd_probe::Terminal, &topology)
            .map_err(|refusal| refusal.to_string())?;

    let json = serde_json::to_string_pretty(&report)
        .map_err(|error| format!("could not encode the probe record: {error}"))?;
    println!("{json}");
    Ok(())
}

/// Emit the capability record without touching ownership or the socket.
///
/// This is the artifact later stories read to find their exact prerequisite.
/// Serial numbers are redacted so the output can be attached to an issue or
/// committed without publishing a device identifier.
fn print_capabilities() -> Result<(), String> {
    let mut record = nzxt_hardware_linux::probe(&SysfsRoot::from_env());
    record.redact_serials();
    let json = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("could not encode the capability record: {error}"))?;
    println!("{json}");
    Ok(())
}

fn serve() -> Result<(), String> {
    if rustix::process::geteuid().is_root() {
        return Err(
            "refusing to run as root. Install the udev rule and run this service as your \
             own user."
                .to_string(),
        );
    }

    let paths = Paths::from_env();
    let sysfs = SysfsRoot::from_env();

    paths
        .ensure()
        .map_err(|error| format!("could not create {}: {error}", paths.runtime_dir.display()))?;

    // Single instance is decided here, before sysfs is read and before any
    // device is asked for. The lock, not the socket, is the arbiter, and it is
    // released by the kernel if this process is killed.
    let _instance = nzxt_daemon::ownership::acquire_instance(&paths)
        .map_err(|conflict| format!("could not start: {}", conflict.detail))?;

    // Bound before the hardware is touched, so nothing is acquired by a
    // process that is about to exit, and before anything is printed, so the
    // banner cannot announce a socket that was never taken.
    let listener = nzxt_daemon::server::bind_socket(&paths.socket)
        .map_err(|error| format!("could not bind {}: {error}", paths.socket.display()))?;

    let daemon = Daemon::start(paths.clone(), &sysfs)
        .map_err(|error| format!("could not start: {error}"))?;

    let status = daemon.status();
    let locked = daemon.locked_devices();
    println!(
        "nzxt-controld {DAEMON_VERSION} listening on {}, holding {} device lock{}",
        status.socket_path,
        locked.len(),
        if locked.len() == 1 { "" } else { "s" }
    );
    for device in &status.devices {
        println!(
            "  {} owned={} writable={}",
            device.id,
            device.owned,
            device.writable.len()
        );
    }
    if let Some(message) = status.config.recovery_message() {
        eprintln!("  {message}");
    }

    let server = Server::attach(listener, daemon);
    server.run();
    Ok(())
}
