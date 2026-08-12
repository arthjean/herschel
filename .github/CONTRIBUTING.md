<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Contributing

[`AGENTS.md`](../AGENTS.md) is the working contract for this repository, for
people and for agents alike. Read it before a first change: it holds the
hardware boundaries, the validation sequence, the workspace shape and the
delivery rules, and this file only points at the parts a contributor meets
first.

## Before opening a pull request

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Stop the daemon before the last one (`systemctl --user stop korid`). The
fixtures mirror the machine down to the `hidraw` numbers, so a running `korid`
is correctly detected as a competing writer and the ownership assertions fail.
That is the conflict detector working.

CI runs those four, plus `cargo deny check`, `reuse lint` and the desktop
metadata validators.

## What a change to the hardware path has to carry

Three rules cause most of the review on this project, and none of them are
negotiable:

- **The allowlist is evidence, not a list.** Adding a device to `ALLOWLIST`
  needs capability evidence measured on that hardware. A datasheet, a forum
  post or another project's table is not evidence.
- **A write is gated on a validated firmware.** Lighting and panel writes only
  happen on a firmware revision in `VALIDATED_FIRMWARE`, and an entry is added
  only by running the matching write probe on that firmware with an operator
  watching the hardware. The probe record is committed under `docs/` and the
  constant cites it by name.
- **Unknown is a valid answer.** An unproven capability is reported as
  `Evidenced::Unknown` with its reason and the control stays disabled. Never
  invent a default: a fabricated default is indistinguishable from a validated
  capability once it reaches a control.

Neither binary may require root, and normal operation opens no network socket.

## Style

Commits are conventional and scoped by crate: `feat(daemon):`, `fix(core):`,
`docs:`. Every Rust source starts with the two SPDX header lines. Explain why a
change is what it is, in the comment and in the commit message; this codebase
documents reasons rather than restating the code.

## Reporting instead

A bug, an unsupported device or a security issue each have their own route: see
the [issue templates](https://github.com/arthjean/kori/issues/new/choose) and
[`SECURITY.md`](./SECURITY.md).
