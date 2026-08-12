---
name: Unsupported hardware
about: A device or a firmware this project does not drive yet
title: "[hardware] "
labels: ["hardware"]
assignees: []
---

<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

## What this issue can and cannot lead to

Kori writes to two allowlisted devices and to nothing else, and it writes
lighting and panel commands only on firmware revisions validated on real
hardware. Neither list is extended from a datasheet, from another project's
table, or from a report that a device "should be the same". Both are extended
from evidence measured on the device itself.

So this issue is the start of that measurement, not a request that can be
granted by editing a list. Reports that carry the evidence below are what make
it possible at all, and they are genuinely welcome.

## The device

```bash
lsusb -d 1e71:
```

- **Vendor and product id** (`1e71:xxxx`):
- **Marketing name**:
- **What it has**: pump / fans / RGB channels / LCD panel

## What the kernel already does with it

```bash
# Which driver, if any, is bound to each interface
ls -l /sys/bus/usb/devices/*/driver 2>/dev/null | grep -i "$(lsusb -d 1e71: | head -1 | awk '{print $6}')" || true
# Whether a hwmon device appeared
grep -H . /sys/class/hwmon/hwmon*/name 2>/dev/null
```

<details>
<summary>output</summary>

```text
paste here
```

</details>

## What Kori sees today

```bash
korid --capabilities
```

<details>
<summary>korid --capabilities</summary>

```json
paste here
```

</details>

## Are you able to test on it

Extending either list means running a probe against your hardware and watching
what it does, which only you can do.

- [ ] I can build from source and run `korid --probe`
- [ ] I can run a write probe with the hardware in front of me, and report what
      the panel or the lighting actually did
- [ ] I understand this may end with the device staying read-only, which is the
      correct outcome when a capability cannot be proven
