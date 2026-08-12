# AGENTS.md

Rust workspace driving NZXT cooling hardware on Linux. This process owns real
cooling, so the hardware rules below outrank convenience.

## Hardware boundaries

- Touch only the two allowlisted devices, `1e71:300e` (Kraken Base) and
  `1e71:2021` (RGB Controller). The allowlist is `ALLOWLIST` in
  `crates/core/src/lib.rs`; extending it requires validated capability evidence
  from real hardware, not a datasheet.
- Reach the thermal path through the bound `kraken2023` driver and its `hwmon`
  attributes. Never detach a kernel driver. Reach the RGB controller through the
  `hidraw` node `usbhid` already created, for the same reason.
- The panel is reached over two interfaces of the Kraken and neither is
  detached: display commands go through the `hidraw` node `kraken2023` itself
  publishes (it starts with `HID_CONNECT_HIDRAW` so user space can share that
  interface, and the report identifiers do not overlap), and the framebuffer
  goes through interface 0, which is vendor class with no driver bound.
  `usbfs::Usbfs::claim` refuses any interface a driver holds; do not relax that.
- `crates/hardware-linux/src/control.rs` is the only module that writes to
  cooling hardware, and `crates/daemon/src/cooling.rs` is the only caller that
  serializes those writes. `crates/hardware-linux/src/rgb.rs` and
  `crates/daemon/src/lighting.rs` are the same pair for lighting;
  `crates/hardware-linux/src/lcd.rs` and `crates/daemon/src/display.rs` for the
  panel. Add a write there or nowhere.
- No RGB write leaves the process unless the controller answered its topology
  *and* reports a firmware listed in `rgb::VALIDATED_FIRMWARE`. That list is
  filled only from a `--rgb-write-probe` run on real hardware. An empty list
  means every controller is read-only, which is the correct failure direction.
- The same gate guards the panel through `lcd::VALIDATED_FIRMWARE`, filled only
  from a `--lcd-write-probe` run an operator watched. It currently holds
  **`2.0.0`** alone, measured on 2026-08-11 against the owned `1e71:300e`, so
  every other revision stays read-only. Adding an entry means running that probe
  on that firmware, not editing the list. A Kraken that never answers
  `0x31 0x01` may carry no display at all, and is reported that way rather than
  assumed to have one.
- Both gates are resolved in one module, `crates/hardware-linux/src/gates.rs`,
  and they call `rgb::is_validated_firmware` and `lcd::is_validated_firmware`
  rather than testing the lists again. The predicate that decides a write and
  the predicate a test exercises have to be the same function: two spellings of
  it is how the tests keep passing for a gate the write path stopped asking.
  `probe.rs` records what the machine exposes and decides nothing.
- Never make either binary require root. `korid` refuses to start as
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
- Stop `korid` before `cargo test --workspace`. The fixtures mirror the
  machine down to the `hidraw` numbers, and `usb::hidraw_node` maps a fixture
  node onto the real `/dev` path, so a running daemon is correctly detected as a
  competing writer and the ownership assertions in `crates/daemon/tests/ipc.rs`
  fail. That is the conflict detector working, not a broken test.
- Tests run against a fake tree, never the machine's real sysfs: use
  `kori_hardware_linux::testing::FakeSysfs` (feature `testing`) and the
  `KORI_SYSFS_ROOT` / `KORI_PROC_ROOT` overrides. `crates/daemon/tests/ipc.rs`
  is the reference: exercise the daemon over a real socket from the client entry
  point rather than reaching into internals.
- `./target/release/korid --capabilities` reads sysfs only: no socket,
  no device node, serials redacted. `--rgb-probe` and `--lcd-probe` add one
  device's own answer, at the cost of two query reports that carry no color, no
  mode, no picture and no parameter. `--probe` asks both and is what re-records
  `docs/capability-record.json`, because both topologies belong in it and a
  record regenerated by a focused probe drops the other device's evidence.
- `--rgb-write-probe` is the only command that sends an unvalidated lighting
  command. It takes the same per-device lock the daemon takes, refuses to start
  without a typed confirmation, and records every byte it sent. Run it only with
  the operator watching the hardware.
- `--lcd-write-probe` is the same for the panel: the only command that puts an
  unvalidated frame on the glass. It also takes the per-device lock.
- A frame is not just bytes on an endpoint. Three rules, all measured on the
  glass and all in `LcdLink::send_frame`:
  - **Wait for the panel's answer to every `0x36` command** before moving on.
    It replies `0x37` with the same second byte, in about 12 ms. Streaming the
    framebuffer without waiting made the panel paint a band of each frame and
    keep the rest of the previous picture, while every transfer still reported
    success. liquidctl brackets these commands in `_write_then_read` for this
    reason.
  - **Send the payload in one bulk transfer.** `usbfs::MAX_BULK_CHUNK` is sized
    so a frame is never split; the reference uses a 2 MiB buffer for this
    generation.
  - **Every frame goes out twice**, not just the first. The panel swaps on the
    transfer after the one that filled it. liquidctl's comment saying the
    doubling is "only required once after initialization" ends in a question
    mark, and its code doubles every static image.

  The first rule was found by photographing the glass on 2026-08-08: a solid
  magenta frame, sent and reported `confirmed`, painted a narrow band while the
  rest of the panel kept the previous picture. `confirmed` was honest about
  what it claims, which is that the `ioctl` returned without error for all
  115 200 bytes. The panel was taking a fraction of them.

## Workspace shape

```text
crates/core            shared vocabulary, touches no hardware
crates/hardware-linux  sysfs, hwmon, USB probing, and the single write path
crates/lcd-renderer    one DisplayPreset to one exact framebuffer, or to the
                       frames an animated picture plays
crates/daemon          ownership, sampling, serialized writes, Unix IPC
crates/app             GPUI window, holds no hardware handle
```

Do not create a crate or module before something calls it. `lcd-renderer` has
two callers by design: the client renders a preset to preview it and the daemon
renders the same preset to send it, which is how the preview is proven to match
the glass rather than asserted to. Neither extracts pixels from the other.

## Interface

- Take every color, size and font from `crates/app/src/theme.rs`. Contrast is
  tested against those tokens, so a literal in a component escapes the check.
- A control that cannot act is `ControlState::Disabled { reason }` with operator
  language. Disabled is how the product refuses an unproven write; it is never
  decorative.
- UI work also passes: Wayland and X11, 920x640 and 1280x720, 100% and 200%
  scale, the screen completed by keyboard alone, and a capture committed under
  `docs/screenshots/` showing the screen as the change leaves it. A capture that
  outlives the interface it shows is deleted with it, not kept as history: it
  documents an interface nobody can reach.
- Steer by `RssAnon` (budget 110 MiB) and cold start (700 ms median over 5
  launches). Total `VmRSS` is a non-regression ceiling dominated by the GPU
  driver mappings, not an optimization target.
- Keep the product identity original: no NZXT logo, CAM asset, vendor wordmark
  or affiliation claim. The shell uses `PRODUCT_NAME` and `UNOFFICIAL_NOTICE`
  from `theme.rs`.

## Delivery

- A write probe run is recorded under `docs/` as
  `<device>-write-probe-<firmware>-<scope>.json`, and the constant it unblocks
  cites that file by name. A firmware in a `VALIDATED_FIRMWARE` list with no
  such file next to it is not evidence, it is an assertion.
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

- Why each schema and protocol version was bumped, and which deprecated field
  is still waiting on which condition: `docs/schema-history.md`
- Observed device capabilities: `docs/capability-record.json`
- What each write probe sent and what the operator saw: `docs/lcd-write-probe-2.0.0.json`,
  `docs/rgb-write-probe-1.5.0-fixed-and-off.json`,
  `docs/rgb-write-probe-1.5.0-with-effects.json`,
  `docs/rgb-write-probe-1.5.0-effect-sweep.json`
