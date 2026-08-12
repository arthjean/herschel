<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

## Summary

<!-- What changed, and why it is what it is. -->

## Validation

Stop the daemon before the tests: a running `korid` is correctly detected as a
competing writer and the ownership assertions fail.

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --workspace --all-targets`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`

## Hardware path

Skip this section only if the change touches no write path and no capability.

- [ ] No device outside `ALLOWLIST` is opened, and the allowlist is unchanged or
      extended with capability evidence measured on that hardware
- [ ] Lighting and panel writes remain gated on `VALIDATED_FIRMWARE`, and any
      new entry cites a write probe record committed under `docs/`
- [ ] Safety floors are enforced before the write, and the daemon revalidates
      every client value
- [ ] An unproven capability is reported as `Evidenced::Unknown` with a reason,
      and its control is `Disabled { reason }` rather than absent
- [ ] Neither binary requires root, and no network socket is opened
- [ ] Ran on real hardware, previous safe state restored, firmware recorded

## Interface

- [ ] Colors, sizes and fonts come from `crates/app/src/theme.rs`
- [ ] Wayland and X11, 920x640 and 1280x720, 100% and 200% scale
- [ ] The screen can be completed with the keyboard alone
- [ ] A capture is committed under `docs/screenshots/` showing the screen as this
      change leaves it

## Packaging

- [ ] A new linked library is recorded in `packaging/linked-libraries.txt` and
      declared in `packaging/nfpm.yaml` and `packaging/aur/PKGBUILD`
- [ ] A version bump touches `Cargo.toml`, `CHANGELOG.md` and the AppStream
      component together
