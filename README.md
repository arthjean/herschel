# Kori

Native open source Linux application to monitor and control NZXT hardware.

> Status: the daemon detects both devices, exposes a typed Unix socket and the GPUI interface displays real state. Cooling, lighting and panel writes are implemented, each gated on a firmware this project validated on the owned hardware. This project is neither affiliated with nor endorsed by NZXT.

## Measured footprint

| Measurement | Observed | Budget |
|---|---|---|
| Cold start, median over 5 launches | 327 ms | ≤ 700 ms |
| `RssAnon` at idle, memory allocated by the process | 81.3 MiB | ≤ 110 MiB |
| Total `VmRSS` at idle | 253.2 MiB | ≤ 320 MiB |
| CPU at idle, 5 min average | 1.10% | ≤ 1.5% |

Total `VmRSS` is dominated by the graphics driver and shader compiler mappings linked in by GPUI, shared with the other GPU clients on the machine: an empty GPUI window accounts for 288.1 MiB of it. This is a non-regression ceiling, not an optimization target. The metric the project steers by is `RssAnon`.

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

The driver exposes liquid temperature, two RPM/PWM channels and 40 curve points per channel. The Kraken also exposes a class `0xff` interface 0 with no kernel driver: that is the LCD framebuffer transport, validated on firmware `2.0.0`. An RGB or LCD capability stays blocked until its protocol is validated on the real hardware, firmware by firmware.

Observed capabilities are recorded in [`docs/capability-record.json`](./docs/capability-record.json), with serial numbers redacted.

## Architecture

```text
crates/
├── app             GPUI, screens, native controls and the client data feed
├── daemon          device ownership, sampling, writes and Unix IPC
├── core            capabilities, telemetry, profiles, IPC protocol and diagnostics
├── hardware-linux  sysfs, hwmon, system sensors and the single write path
└── lcd-renderer    one DisplayPreset to one exact framebuffer, or to frames
```

The `lcd-renderer` crate turns one `DisplayPreset` into the exact framebuffer, and into the frames an animated picture plays. It has two callers by design: the client renders a preset to preview it, the daemon renders the same preset to send it.

The thermal path goes entirely through the `kraken2023` driver: no kernel driver is detached and no USB endpoint is opened for the thermal side. Telemetry only reads, and three independent collectors (Kraken, CPU/memory, GPU) sample in parallel so that one failing sensor does not stop the others. GPU metrics go through NVML, loaded dynamically: without an NVIDIA driver, the GPU is simply unavailable.

The daemon stays independent from the window in order to serialize commands, detect concurrent writers and restore a compatible profile after reconnection or resume from sleep.

## Access

Neither binary ever runs as root, so writing needs the kernel files to be reachable as your own user. `packaging/udev/70-kori.rules` grants exactly that, on the two allowlisted devices and nothing else:

```bash
sudo groupadd --system kori
sudo usermod --append --groups kori "$USER"
sudo install -m 0644 packaging/udev/70-kori.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --action=change --subsystem-match=hwmon
```

Log out and back in, because group membership is read when the session starts. Then restart the daemon: capabilities are resolved when it opens the device, so a daemon that started before the rule keeps reporting read only.

The two `hidraw` nodes and the Kraken's `usbfs` node are handed to the logged-in user through `uaccess`, which is a session ACL and needs no group. The `hwmon` attributes cannot use it, because `uaccess` places its ACL on a device node under `/dev` and a `hwmon` device has none; `sysfs` carries no POSIX ACLs either, so the group is what the write permission hangs on. Only the four PWM attributes and the eighty curve points change ownership. Every reading attribute is world-readable already and is left untouched.

## Usage

```bash
cargo build --release

# Record the real capabilities of the machine (read only, no socket).
./target/release/korid --capabilities > docs/capability-record.json

# Start the service, then the interface.
./target/release/korid &
./target/release/kori
```

To start the daemon with your desktop session, install it as a user unit.
Nothing here needs root: the binary goes to your own `~/.local/bin` and the unit
to your own `systemd --user` instance.

```bash
install -Dm0755 target/release/korid ~/.local/bin/korid
install -Dm0644 packaging/systemd/korid.service ~/.config/systemd/user/korid.service
systemctl --user daemon-reload
systemctl --user enable --now korid.service
```

`systemctl --user status korid` reports the socket it bound and how many
attributes it found writable on each device. Reinstall the binary and run
`systemctl --user restart korid` after a rebuild, because capabilities are
resolved when the daemon opens the devices.

The unit is wanted by `graphical-session.target`, not `default.target`, so it
starts with your session rather than with the machine. `uaccess` grants the
hidraw nodes through a session ACL, and a daemon started before that ACL exists
reports the panel and the RGB controller as permission denied until it is
restarted. If you enabled an earlier version of the unit, run
`systemctl --user disable korid.service` once before enabling it again, so
the stale `default.target` link goes away.

The daemon refuses to start as root. Without the udev rule above, the `hwmon` attributes stay read only and the application says so explicitly instead of failing silently.

Environment variables read:

| Variable | Role |
|---|---|
| `KORI_SOCKET` | Unix socket path |
| `KORI_CONFIG_DIR` | Configuration directory |
| `KORI_RUNTIME_DIR` | Lock and socket directory |
| `KORI_SYSFS_ROOT` | sysfs root, for tests against a fake tree |
| `KORI_PROC_ROOT` | `/proc` root, for the same tests |
| `KORI_STARTUP_TRACE` | Prints the delay to the first frame |
| `KORI_EXIT_AFTER_FIRST_FRAME` | Exits after the first frame, to measure startup |

## Validation

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## v1 scope

The application holds three primary destinations:

1. Monitoring
2. Cooling
3. Lighting, one card per device: the controller's channels and the Kraken panel

Explicitly out of scope: Web Integrations, cloud, accounts, firmware updates, remote API, unvalidated NZXT devices and non-NZXT hardware control.

## Research

The [initial exploration of the NZXT GitHub organization and the Linux ecosystem](./nzxt-linux-github-research.md) is kept as decision history. Its initial recommendation of a Web Integrations runtime has been replaced by the hardware-only direction described above.

## License

[GPL-3.0-or-later](./LICENSE). No external code is imported before its license and its compatibility have been verified.

The dependency inventory and the compatibility audit are still due before any package distribution.
