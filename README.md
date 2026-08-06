# NZXT Control Linux

Native open source Linux application to monitor and control NZXT hardware.

> Status: foundation implemented (EP-001), in review. The daemon detects both devices, exposes a typed Unix socket and the GPUI interface displays real state. No hardware write is implemented yet. Working technical name. This project is neither affiliated with nor endorsed by NZXT.

## Measured footprint

| Measurement | Observed | PRD v1.2 budget |
|---|---|---|
| Cold start, median over 5 launches | 327 ms | ≤ 700 ms |
| `RssAnon` at idle, memory allocated by the process | 81.3 MiB | ≤ 110 MiB |
| Total `VmRSS` at idle | 253.2 MiB | ≤ 320 MiB |
| CPU at idle, 5 min average | 1.10% | ≤ 1.5% |

Total `VmRSS` is dominated by the graphics driver and shader compiler mappings linked in by GPUI, shared with the other GPU clients on the machine: an empty GPUI window accounts for 288.1 MiB of it. This is a non-regression ceiling, not an optimization target. The metric the project steers by is `RssAnon`. Full breakdown in [`docs/ep-001-evidence.md`](./docs/ep-001-evidence.md).

## Intent

Build a lightweight desktop application with Rust and GPUI, focused solely on the hardware tasks that matter:

- CPU, GPU, RAM and Kraken monitoring;
- pump, fan and thermal curve control;
- per-channel RGB control;
- Kraken LCD configuration and rendering.

The product takes the operational density and visual restraint of NZXT CAM, with an original identity, original components and original assets.

## Principles

- Native GPUI interface, with no HTML, JavaScript, WebView or browser engine.
- Local operation, with no account, cloud, telemetry or network service.
- Linux `hwmon` first for the thermal path.
- Direct HID/USB access limited to validated RGB and LCD capabilities.
- A single user daemon owns hardware writes.
- No speculative access to an unvalidated model or firmware.
- Neither the GUI nor the daemon runs as root.

## Initial target

Verified development environment:

| Item | Value |
|---|---|
| Distribution | Fedora 44 |
| Kernel | `7.1.6-201.fc44.x86_64` |
| Build toolchain | Rust 1.97.1, 2024 edition (`rust-toolchain.toml`) |
| Minimum supported Rust | 1.90, verified by compilation |
| Kraken | `1e71:300e` NZXT Kraken Base, `bcdDevice` 0200 |
| RGB | `1e71:2021` NZXT RGB Controller, `bcdDevice` 0105 |
| Thermal driver | `kraken2023` on HID interface 1 |

The driver exposes liquid temperature, two RPM/PWM channels and 40 curve points per channel. The Kraken also exposes a class `0xff` interface 0 with no kernel driver: that is the candidate for the LCD transport, to be validated by US-016. The RGB and LCD capabilities stay blocked until their protocol is validated on the real hardware.

Observed capabilities are recorded in [`docs/capability-record.json`](./docs/capability-record.json), with serial numbers redacted.

## Architecture

```text
crates/
├── app             GPUI, screens, native controls and the client data feed
├── daemon          device ownership, sampling, writes and Unix IPC
├── core            capabilities, telemetry, profiles, IPC protocol and diagnostics
└── hardware-linux  sysfs, hwmon, system sensors and the single write path
```

The `lcd-renderer` crate (`DisplayPreset` and the exact framebuffer) will arrive with EP-004, once the LCD transport is proven on `1e71:300e`. It is not created empty: a module nothing calls is not a foundation.

The thermal path goes entirely through the `kraken2023` driver: no kernel driver is detached and no USB endpoint is opened for the thermal side. Telemetry only reads, and three independent collectors (Kraken, CPU/memory, GPU) sample in parallel so that one failing sensor does not stop the others. GPU metrics go through NVML, loaded dynamically: without an NVIDIA driver, the GPU is simply unavailable.

The daemon stays independent from the window in order to serialize commands, detect concurrent writers and restore a compatible profile after reconnection or resume from sleep.

## Usage

```bash
cargo build --release

# Record the real capabilities of the machine (read only, no socket).
./target/release/nzxt-controld --capabilities > docs/capability-record.json

# Start the service, then the interface.
./target/release/nzxt-controld &
./target/release/nzxt-control
```

The daemon refuses to start as root. Without a udev rule, the `hwmon` attributes stay read only and the application says so explicitly instead of failing silently.

Environment variables read:

| Variable | Role |
|---|---|
| `NZXT_CONTROL_SOCKET` | Unix socket path |
| `NZXT_CONTROL_CONFIG_DIR` | Configuration directory |
| `NZXT_CONTROL_RUNTIME_DIR` | Lock and socket directory |
| `NZXT_SYSFS_ROOT` | sysfs root, for tests against a fake tree |
| `NZXT_PROC_ROOT` | `/proc` root, for the same tests |
| `NZXT_STARTUP_TRACE` | Prints the delay to the first frame |
| `NZXT_EXIT_AFTER_FIRST_FRAME` | Exits after the first frame, to measure startup |

## Validation

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## v1 scope

The application holds four primary destinations:

1. Monitoring
2. Cooling
3. Lighting
4. LCD

Explicitly out of scope: Web Integrations, cloud, accounts, firmware updates, remote API, unvalidated NZXT devices and non-NZXT hardware control.

## Product plan

- [Full PRD](./tasks/prd-native-nzxt-hardware-control.md)
- [Epic and story tracking](./tasks/prd-native-nzxt-hardware-control-status.json)

The first story validates GPUI under Wayland and X11 with a representative LCD screen, then measures startup, memory, CPU, keyboard focus and scaling before extending the interface.

## Research

The [initial exploration of the NZXT GitHub organization and the Linux ecosystem](./nzxt-linux-github-research.md) is kept as decision history. Its initial recommendation of a Web Integrations runtime has been replaced by the hardware-only PRD.

## License

[GPL-3.0-or-later](./LICENSE). No external code is imported before its license and its compatibility have been verified.

The dependency inventory and the compatibility audit are still due before any package distribution (US-020).
