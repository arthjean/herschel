#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Entry point of the hardware daemon.
//!
//! Refuses to run as root, probes the machine, takes what ownership it can and
//! serves the local socket. Every failure path exits with a diagnostic instead
//! of a panic.

use std::process::ExitCode;

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
  --capabilities   Print the versioned capability record as JSON and exit.
                   Read-only: no socket is opened and no device is written.
  --version        Print the version and exit.
  --help           Print this message.
";

fn run() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        None => serve(),
        Some("--capabilities") => print_capabilities(),
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

/// Emit the capability record without touching ownership or the socket.
///
/// This is the artefact later stories read to find their exact prerequisite.
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

    let server = Server::bind(&paths.socket, daemon)
        .map_err(|error| format!("could not bind {}: {error}", paths.socket.display()))?;
    server.run();
    Ok(())
}
