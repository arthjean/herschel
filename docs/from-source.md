<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Building and running from source

What the packages do for you, done by hand. Everything here lands in your own
`~/.local` and your own `systemd --user` instance, so none of it needs root
except the udev rule, which is the one thing that grants a user access to
kernel files.

## Verified environment

| Item | Value |
|---|---|
| Distribution | Fedora 44 |
| Kernel | `7.1.6-201.fc44.x86_64` |
| Build toolchain | Rust 1.97.1, 2024 edition (`rust-toolchain.toml`) |
| Minimum supported Rust | 1.90, verified by compilation |
| Kraken | `1e71:300e` NZXT Kraken Base, `bcdDevice` 0200 |
| RGB | `1e71:2021` NZXT RGB Controller, `bcdDevice` 0105 |
| Thermal driver | `kraken2023` on HID interface 1 |

The driver exposes liquid temperature, two RPM/PWM channels and 40 curve points
per channel. The Kraken also exposes a class `0xff` interface 0 with no kernel
driver: that is the LCD framebuffer transport, validated on firmware `2.0.0`. An
RGB or LCD capability stays blocked until its protocol is validated on the real
hardware, firmware by firmware.

## Build

```bash
cargo build --release

# Record the real capabilities of the machine (read only, no socket).
./target/release/korid --capabilities > docs/capability-record.json

# Start the service, then the interface.
./target/release/korid &
./target/release/kori
```

## Hardware access

Neither binary ever runs as root, so writing needs the kernel files to be
reachable as your own user. `packaging/udev/70-kori.rules` grants exactly that,
on the two allowlisted devices and nothing else:

```bash
sudo groupadd --system kori
sudo usermod --append --groups kori "$USER"
sudo install -m 0644 packaging/udev/70-kori.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --action=change --subsystem-match=hwmon
```

Log out and back in, because group membership is read when the session starts.
Then restart the daemon: capabilities are resolved when it opens the device, so
a daemon that started before the rule keeps reporting read only.

The two `hidraw` nodes and the Kraken's `usbfs` node are handed to the logged-in
user through `uaccess`, which is a session ACL and needs no group. The `hwmon`
attributes cannot use it, because `uaccess` places its ACL on a device node
under `/dev` and a `hwmon` device has none; `sysfs` carries no POSIX ACLs
either, so the group is what the write permission hangs on. Only the four PWM
attributes and the eighty curve points change ownership. Every reading attribute
is world-readable already and is left untouched.

Without the rule, the `hwmon` attributes stay read only and the application says
so explicitly instead of failing silently. The daemon refuses to start as root.

## Run it as a user service

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
`systemctl --user disable korid.service` once before enabling it again, so the
stale `default.target` link goes away.

## Desktop entry and icon

To start the client from the desktop rather than from a shell, install its
entry, its icon and its AppStream component beside the binary.

```bash
install -Dm0755 target/release/kori ~/.local/bin/kori
install -Dm0644 packaging/desktop/kori.desktop ~/.local/share/applications/kori.desktop
install -Dm0644 packaging/icons/kori.svg ~/.local/share/icons/hicolor/scalable/apps/kori.svg
install -Dm0644 packaging/desktop/io.github.arthjean.kori.metainfo.xml \
  ~/.local/share/metainfo/io.github.arthjean.kori.metainfo.xml
update-desktop-database ~/.local/share/applications
gtk-update-icon-cache --force --ignore-theme-index ~/.local/share/icons/hicolor
```

The last two commands refresh caches that some desktops read instead of the
directory. Neither is required to exist: if the command is missing, the entry
still appears on the next login.

The icon ships as one SVG under `hicolor/scalable` rather than as a set of PNGs,
which is what every current desktop reads first and what keeps the mark sharp at
whatever size the shell asks for. `StartupWMClass=kori` in the entry is the same
string as the window's own `app_id`; that match is what makes the desktop draw
this icon for the running window instead of a generic one, and
`cargo test --workspace` asserts the two never drift apart.

## Environment variables

| Variable | Role |
|---|---|
| `KORI_SOCKET` | Unix socket path |
| `KORI_CONFIG_DIR` | Configuration directory |
| `KORI_RUNTIME_DIR` | Lock and socket directory |
| `KORI_SYSFS_ROOT` | sysfs root, for tests against a fake tree |
| `KORI_PROC_ROOT` | `/proc` root, for the same tests |
| `KORI_STARTUP_TRACE` | Prints the delay to the first frame |
| `KORI_EXIT_AFTER_FIRST_FRAME` | Exits after the first frame, to measure startup |

## Workspace

```text
crates/
├── app             GPUI, screens, native controls and the client data feed
├── daemon          device ownership, sampling, writes and Unix IPC
├── core            capabilities, telemetry, profiles, IPC protocol and diagnostics
├── hardware-linux  sysfs, hwmon, system sensors and the single write path
└── lcd-renderer    one DisplayPreset to one exact framebuffer, or to frames
```

The `lcd-renderer` crate turns one `DisplayPreset` into the exact framebuffer,
and into the frames an animated picture plays. It has two callers by design: the
client renders a preset to preview it, the daemon renders the same preset to
send it.

The thermal path goes entirely through the `kraken2023` driver: no kernel driver
is detached and no USB endpoint is opened for the thermal side. Telemetry only
reads, and three independent collectors (Kraken, CPU/memory, GPU) sample in
parallel so that one failing sensor does not stop the others. GPU metrics go
through NVML, loaded dynamically: without an NVIDIA driver, the GPU is simply
unavailable.

The daemon stays independent from the window in order to serialize commands,
detect concurrent writers and restore a compatible profile after reconnection or
resume from sleep.

[`AGENTS.md`](../AGENTS.md) holds the boundaries a change has to respect,
device by device and write path by write path.

## Validation

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Stop `korid` before `cargo test --workspace`: the fixtures mirror the machine
down to the `hidraw` numbers, so a running daemon is correctly detected as a
competing writer and the ownership assertions fail. That is the conflict
detector working, not a broken test.

CI runs those four on every push, plus `cargo deny check` for a license the
graph cannot absorb, `reuse lint` for a file with no copyright, and
`desktop-file-validate` with `appstreamcli validate` for the metadata a software
centre reads.

## Measured footprint

| Measurement | Observed | Budget |
|---|---|---|
| Cold start, median over 5 launches | 327 ms | ≤ 700 ms |
| `RssAnon` at idle, memory allocated by the process | 81.3 MiB | ≤ 110 MiB |
| Total `VmRSS` at idle | 253.2 MiB | ≤ 320 MiB |
| CPU at idle, 5 min average | 1.10% | ≤ 1.5% |

Measured on the machine above. Total `VmRSS` is dominated by the graphics driver
and shader compiler mappings linked in by GPUI, shared with the other GPU
clients on the machine: an empty GPUI window accounts for 288.1 MiB of it. This
is a non-regression ceiling, not an optimization target. The metric the project
steers by is `RssAnon`.

## Decision history

The [initial exploration of the NZXT GitHub organization and the Linux
ecosystem](../nzxt-linux-github-research.md) is kept as history. Its initial
recommendation of a Web Integrations runtime has been replaced by the
hardware-only direction the project took.

[`docs/releasing.md`](./releasing.md) is how a release is cut, verified and
published, and why Flatpak, Snap, arm64 and a signed repository are not part of
it. [`docs/schema-history.md`](./schema-history.md) records why each schema and
protocol version was bumped.
