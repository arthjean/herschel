<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Security policy

## Supported versions

Fixes land on the latest release. Run the newest one from the
[releases page](https://github.com/arthjean/kori/releases/latest) or from your
package manager.

## Reporting a vulnerability

**Do not open a public issue for a security report.**

Email **arthur.jean@strivex.fr** with a description and its impact, the steps or
proof of concept, the Kori version (`korid --version`), and your distribution,
kernel and session type. Expect an acknowledgement within 72 hours. Once a fix
is released, credit is given unless you ask otherwise.

## What counts as a vulnerability here

This software drives cooling hardware and reaches devices directly, so the
interesting failures are not the usual web ones. In rough order of severity:

- A write reaching a device outside `ALLOWLIST` in `crates/core/src/lib.rs`, or
  a path that opens a device node the allowlist does not name.
- A lighting or panel write that happens on a firmware absent from
  `rgb::VALIDATED_FIRMWARE` or `lcd::VALIDATED_FIRMWARE`, which means a gate in
  `crates/hardware-linux/src/gates.rs` was bypassed rather than answered.
- A duty written below the floors in `crates/core/src/profile.rs`, or any path
  where a client value reaches hardware without the daemon revalidating it.
- Either binary acquiring a privilege it did not start with, or running as root
  at all. `korid` refuses to start as root; a way around that refusal is a
  report.
- The udev rule in `packaging/udev/70-kori.rules` granting access beyond the two
  allowlisted devices, or beyond the attributes
  `crates/hardware-linux/src/control.rs` writes.
- Any network socket opened during normal operation. There should be none.
- A package script (`packaging/scripts/`) doing something as root beyond
  creating the `kori` group and refreshing caches.

## What is not a vulnerability

A capability reported as `Evidenced::Unknown`, or a control disabled with a
reason, is the product refusing to act on an unproven capability. That is the
designed behavior, not a defect. An unsupported device is a feature request; the
allowlist is extended only with capability evidence measured on real hardware,
which is what the *Unsupported hardware* issue template asks for.
