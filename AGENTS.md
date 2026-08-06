# AGENTS.md

Rust workspace driving NZXT cooling hardware on Linux. This process owns real
cooling, so the hardware rules below outrank convenience.

## Hardware boundaries

- Touch only the two allowlisted devices, `1e71:300e` (Kraken Base) and
  `1e71:2021` (RGB Controller). The allowlist is `ALLOWLIST` in
  `crates/core/src/lib.rs`; extending it requires validated capability evidence
  from real hardware, not a datasheet.
- Reach the thermal path through the bound `kraken2023` driver and its `hwmon`
  attributes. Never detach a kernel driver and never open a USB endpoint for the
  thermal side. The RGB and LCD transports stay unimplemented until US-013 and
  US-016 validate them.
- `crates/hardware-linux/src/control.rs` is the only module that writes to
  cooling hardware, and `crates/daemon/src/cooling.rs` is the only caller that
  serializes those writes. Add a write there or nowhere.
- Never make either binary require root. `nzxt-controld` refuses to start as
  root (`crates/daemon/src/main.rs`). Missing permission degrades to read-only
  with a stated reason; it never escalates.
- Keep the safety floors enforced before the write, not at the UI: `MIN_PUMP_DUTY`
  and the rest of `crates/core/src/profile.rs`. The daemon revalidates every
  client value; the client is not a trusted input.
- Normal operation opens zero network sockets and makes zero network requests.
- Report an unproven capability as `Evidenced::Unknown` with the reason. Never
  fabricate a default to fill a gap, because a fabricated default is
  indistinguishable from a validated capability once it reaches a control.
- On live hardware, record the firmware, stay inside the allowlist, and restore
  the previous safe state before finishing.

## Validation

Every story passes all four, in this order:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- The toolchain is pinned to 1.97.1 in `rust-toolchain.toml`. The declared MSRV
  is 1.90 in `[workspace.package]`. Changing either is a deliberate decision,
  not a side effect.
- `[workspace.lints.clippy]` denies `panic`, `unimplemented` and `dbg_macro`,
  and warns on `unwrap_used`, `expect_used` and `unwrap_in_result`. Tests are
  exempt through `clippy.toml` plus the crate-root line
  `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]`.
  Carry that line into any new crate root; do not add per-call allows.
- Tests run against a fake tree, never the machine's real sysfs: use
  `nzxt_hardware_linux::testing::FakeSysfs` (feature `testing`) and the
  `NZXT_SYSFS_ROOT` / `NZXT_PROC_ROOT` overrides. `crates/daemon/tests/ipc.rs`
  is the reference: exercise the daemon over a real socket from the client entry
  point rather than reaching into internals.
- `./target/release/nzxt-controld --capabilities` is read-only, opens no socket
  and redacts serials. It is the safe way to re-record
  `docs/capability-record.json`.

## Workspace shape

```text
crates/core            shared vocabulary, touches no hardware
crates/hardware-linux  sysfs, hwmon, USB probing, and the single write path
crates/daemon          ownership, sampling, serialized writes, Unix IPC
crates/app             GPUI window, holds no hardware handle
```

Do not create a crate or module before something calls it. `lcd-renderer` is
deliberately absent until EP-004 proves the LCD transport.

## Interface

- Take every color, size and font from `crates/app/src/theme.rs`. Contrast is
  tested against those tokens, so a literal in a component escapes the check.
- A control that cannot act is `ControlState::Disabled { reason }` with operator
  language. Disabled is how the product refuses an unproven write; it is never
  decorative.
- UI work also passes: Wayland and X11, 920x640 and 1280x720, 100% and 200%
  scale, the screen completed by keyboard alone, and a capture committed under
  `docs/screenshots/`.
- Steer by `RssAnon` (budget 110 MiB) and cold start (700 ms median over 5
  launches). Total `VmRSS` is a non-regression ceiling dominated by the GPU
  driver mappings, not an optimization target.
- Keep the product identity original: no NZXT logo, CAM asset, vendor wordmark
  or affiliation claim. The shell uses `PRODUCT_NAME` and `UNOFFICIAL_NOTICE`
  from `theme.rs`.

## Delivery

- `tasks/prd-native-nzxt-hardware-control.md` is the plan of record and
  `tasks/prd-native-nzxt-hardware-control-status.json` tracks `EP-NNN` and
  `US-NNN` status. Update the tracker in the same change as the work it
  describes.
- A completed epic gets `docs/ep-NNN-evidence.md`: one row per acceptance
  criterion, naming the implementing code and the test or observation that
  proves it.
- Start every Rust source with the two SPDX header lines. `REUSE.toml` covers
  only the files that cannot carry a comment.
- Verify a dependency's license against GPL-3.0-or-later before importing it,
  and record the finding next to the dependency (see the `nvml-wrapper` note in
  `crates/hardware-linux/Cargo.toml`).
- Commit with conventional messages scoped by crate: `feat(daemon):`,
  `fix(core):`, `docs:`.
- `.claude/` and `.codex/` are gitignored. Shared agent guidance belongs in this
  file, not in a machine-local rules directory.

## Reference

- Product scope, quality gates and story acceptance: `tasks/prd-native-nzxt-hardware-control.md`
- Kernel ABI, observed attribute modes and measured evidence: `docs/ep-002-evidence.md`
- Startup, memory and CPU measurements: `docs/ep-001-evidence.md`
- Observed device capabilities: `docs/capability-record.json`
