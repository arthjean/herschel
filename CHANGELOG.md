<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Changelog

The human-readable history. Two other files carry a version and must agree with
this one at release time: `Cargo.toml` and the AppStream component in
`packaging/desktop/`. `packaging/stage.sh` refuses to build a release when they
disagree, because each file is valid on its own while the set is wrong.

Protocol and schema versions have their own record, with the condition each
deprecated field is still waiting on: [`docs/schema-history.md`](./docs/schema-history.md).

## 0.1.0 - 2026-08-13

First release.

### Added

- Pump and fan control: fixed duties and 40-point curves per channel, written
  through the `kraken2023` driver's `hwmon` attributes, revalidated by the
  daemon against the safety floors before every write.
- Telemetry from three independent collectors sampling in parallel, so one
  failing sensor does not stop the others: liquid temperature and channel speeds
  from the Kraken, CPU and memory from the system, GPU through NVML loaded at
  runtime.
- Lighting control for the RGB controller's channels, gated on a firmware
  revision validated on owned hardware.
- LCD panel presets, rendered by the same crate that feeds the glass, so the
  preview is proven to match the panel rather than asserted to.
- A per-user daemon owning every device write, serializing them, detecting a
  competing writer, and exposing a typed Unix socket.
- A GPUI interface over Wayland and X11, with a desktop entry, a scalable icon
  and an AppStream component.
- Packages for Debian and Fedora families, a tarball with its own installer for
  every other distribution, and an AUR recipe. Every artifact signed through
  Sigstore and installed in a clean container before publication.

### Security

- Neither binary runs as root, and `korid` refuses to start as root.
- Normal operation opens no network socket, enforced by the user unit's
  `RestrictAddressFamilies`.
- Writes reach two allowlisted devices and nothing else. An unproven capability
  is reported as unknown and its control stays disabled with the reason, rather
  than being offered on a guess.
