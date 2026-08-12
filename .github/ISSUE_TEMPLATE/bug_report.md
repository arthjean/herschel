---
name: Bug report
about: Something behaves differently from what it reports
title: "[bug] "
labels: ["bug"]
assignees: []
---

<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later

For a device this project does not support yet, use the "Unsupported hardware"
template instead. For a security issue, do not open an issue at all: see
SECURITY.md.
-->

## Environment

- **Distribution and version**:
- **Kernel** (`uname -r`):
- **Session**: Wayland / X11
- **Desktop or compositor**:
- **Kori version** (`korid --version`):
- **Install format**: .deb / .rpm / AUR / tarball / built from source

## What happened

<!-- What did the interface or the daemon do, and what did you expect instead? -->

## Steps to reproduce

1.
2.
3.

## Capability record

This is the single most useful attachment. It reads sysfs only: no socket, no
device node, and serial numbers are redacted before printing.

```bash
korid --capabilities
```

<details>
<summary>korid --capabilities</summary>

```json
paste here
```

</details>

## Daemon output

```bash
systemctl --user status korid
journalctl --user --unit korid --since "1 hour ago"
```

<details>
<summary>logs</summary>

```text
paste here
```

</details>

## Access

A control that stays disabled with a permission reason is usually the udev rule
or the group membership, not a bug. Confirm before filing:

- [ ] `id -nG | grep -q kori` returns true, and I logged out and back in after
      being added to the group
- [ ] `/usr/lib/udev/rules.d/70-kori.rules` or `/etc/udev/rules.d/70-kori.rules`
      exists
- [ ] The daemon was restarted after either of those changed, since capabilities
      are resolved when it opens the devices
