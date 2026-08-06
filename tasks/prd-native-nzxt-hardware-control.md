[PRD]
# PRD: Native NZXT Hardware Control for Linux

## Changelog

| Version | Date | Author | Summary |
|---------|------|--------|---------|
| 1.2 | 2026-08-06 | Arthur Jean | Revised the memory and CPU budgets against measured GPUI behavior; split resident set into a driver-dependent ceiling and an application-controlled figure |
| 1.1 | 2026-07-30 | Arthur Jean | Renamed repository to `nzxt-control-linux` and aligned project documentation |
| 1.0 | 2026-07-30 | Arthur Jean | Initial hardware-only product definition |

## Problem Statement

1. NZXT CAM does not provide a Linux application, leaving owners of NZXT cooling, RGB and LCD hardware without a single native control surface for monitoring and configuration.
2. Existing Linux tools cover parts of the problem but optimize for broader hardware coverage, command-line operation or a daemon plus Web UI. They do not provide the focused native GPUI experience required here.
3. Direct USB control is safety-sensitive. An incorrect thermal command, concurrent writers or an unvalidated reverse-engineered packet can leave cooling state uncertain or damage device state.

**Why now:** the development machine exposes an NZXT Kraken Base (`1e71:300e`) and NZXT RGB Controller (`1e71:2021`). Linux `7.1.5-200.fc44.x86_64` binds the Kraken to `kraken2023` and exposes liquid temperature, two RPM/PWM channels and 40 curve points through `hwmon`. The core thermal path can therefore start from a real kernel interface while GPUI and the remaining RGB/LCD paths are validated against owned hardware.

## Overview

Build an unofficial, open-source Linux desktop application for the locally connected NZXT Kraken and RGB controller. The product uses a compact native GPUI interface inspired by the operational density of NZXT CAM while keeping its own name, logo, assets and visual tokens. The first release has four destinations only: Monitoring, Cooling, Lighting and LCD.

A restricted per-user Rust daemon is the only hardware writer. It reads and controls the thermal path through Linux `hwmon`, and uses direct HID/USB only for capabilities that the kernel does not expose. A GPUI client communicates with the daemon through a local Unix socket. This split keeps thermal behavior independent from window lifecycle, supports restoration after wake and makes conflicting writers observable.

The initial workspace boundary is:

```text
crates/
├── app             GPUI window, screens and native controls
├── daemon          device ownership, command queue and Unix IPC
├── core            capability, telemetry, profile and IPC types
├── hardware-linux  hwmon and validated HID/USB backends
└── lcd-renderer    shared DisplayPreset and exact framebuffer output
```

The application contains no browser engine, WebView, HTTP server, remote API, account, cloud synchronization, telemetry or Web Integrations compatibility. Configuration and diagnostics stay local. The first compatibility contract is the exact hardware installed on the development machine; other NZXT models require explicit capability validation before support is advertised.

## Goals

| Goal | Month-1 Target | Month-6 Target |
|------|---------------|----------------|
| Validate owned hardware | Both VID/PID values detected; Kraken telemetry and capability matrix recorded | Monitoring, cooling, RGB and LCD acceptance suites pass on both devices |
| Deliver native control surfaces | Monitoring and Cooling functional at 920x640 | All four destinations functional with zero placeholder controls |
| Bound resource use | UI visible in <=700 ms, idle `RssAnon` <=110 MiB, idle total RSS <=320 MiB and idle CPU <=1.5% on the development machine | Idle `RssAnon` <=100 MiB, idle total RSS <=320 MiB and idle CPU <=1.2% over a 30-minute sample |
| Prove hardware reliability | 30-minute thermal and reconnect test with zero unhandled errors | 8-hour soak plus 10 suspend/resume cycles with zero lost profiles or concurrent writes |
| Preserve local-only operation | Zero application network requests during a 30-minute capture | Zero listening TCP/UDP sockets and zero application network requests in every release test |

## Target Users

### Primary owner-operator

- **Role:** Arthur, using Fedora Linux with the exact Kraken and RGB Controller listed above.
- **Behaviors:** monitors thermals during development and gaming, adjusts pump/fan curves, configures RGB and expects LCD metrics to remain correct after the UI closes.
- **Pain points:** NZXT CAM is unavailable on Linux; existing tools split control across kernel files, command-line utilities and broader control suites.
- **Current workaround:** inspect `hwmon` manually and use separate community tools when their model support overlaps.
- **Success looks like:** both devices are detected in one native window, cooling remains safe without the window, and the four core tasks require no terminal commands after installation.

### Secondary Linux NZXT owner

- **Role:** technically capable Linux user with an explicitly supported NZXT VID/PID and firmware.
- **Behaviors:** checks compatibility before installation, expects local profiles and wants actionable errors instead of guessed support.
- **Pain points:** reverse-engineered hardware support is often reported at product-family level even when firmware behavior differs.
- **Current workaround:** combine liquidctl, CoolerControl, OpenRGB and model-specific scripts.
- **Success looks like:** the compatibility matrix names the exact device and firmware, unsupported operations stay disabled, and diagnostics contain enough evidence for a useful issue report.

## Research Findings

Key findings that informed this PRD:

### Competitive Context

- [liquidctl](https://github.com/liquidctl/liquidctl) contains the broadest public NZXT protocol coverage and documents Kraken model/firmware limitations. Its GPL-3.0-or-later implementation is a reference and potential source under compatible licensing, not evidence that every command works on `1e71:300e`.
- [CoolerControl](https://gitlab.com/coolercontrol/coolercontrol) already provides cross-vendor monitoring, profiles, alerts, wake restoration and a daemon plus Web UI. This product differentiates through a smaller NZXT-only native interface rather than duplicating CoolerControl's breadth.
- [OpenRGB](https://github.com/CalcProgrammer1/OpenRGB) detects the RGB Controller PID `1e71:2021` in its NZXT HUE 2 controller code. OpenRGB also warns that reverse-engineered protocols can damage hardware, so RGB writes remain blocked until validated on the owned controller.
- **Market gap:** a native, local-only, single-vendor Linux control instrument with bounded resource use, explicit per-firmware support and a CAM-like operational layout.

### Measured GPUI resource behavior (2026-08-06)

The v1.0 memory budget was written before any measurement existed. US-001 produced one, and it invalidated the original figures rather than the stack.

Release build of the four-destination shell, GPUI 0.2.2, Fedora 44, NVIDIA driver, window open and untouched for 300 seconds:

| Metric | Shell | Empty GPUI window |
|---|---|---|
| Total `VmRSS` | 253.2 MiB | 288.1 MiB |
| `RssAnon` | 81.3 MiB | 98.1 MiB |
| PSS | 209.7 MiB | not sampled |
| Idle CPU, 5-minute average | 1.10% | 1.00% |
| Threads | 30 | 30 |

A GPUI window containing a single `div` costs *more* than the complete shell. Of the resident set, 176.6 MiB is file-backed: `libnvidia-gpucomp`, `libLLVM` for shader compilation, `libnvidia-glcore`, `libnvidia-rtcore` and `/dev/nvidiactl` mappings, all shared with every other GPU client on the machine. The 30 threads and the 1.0% idle CPU are present in the empty window too, so they belong to GPUI's executor and the driver.

Two consequences for the budget:

1. A total-RSS target below roughly 290 MiB is unreachable with this stack on this driver, regardless of what the application does. Total `VmRSS` is therefore a regression ceiling, not an optimization target.
2. `RssAnon` is the figure the project actually controls, and it is the one the Month-6 target tightens.

### Best Practices Applied

- Prefer the Linux kernel [`nzxt-kraken3` hwmon interface](https://docs.kernel.org/hwmon/nzxt-kraken3.html) for liquid temperature, RPM, PWM and onboard curves instead of bypassing the bound driver.
- Keep one hardware writer, serialize commands, deduplicate writes and enter read-only mode when another process owns the endpoint.
- Never run the GUI as root. Grant only the required VID/PID access through narrow udev rules.
- Prevalidate the complete curve before any write, preserve the device/firmware thermal failsafe and restore the last known-good configuration after a partial failure.
- Use GPUI's documented [`Element`](https://docs.rs/gpui/latest/gpui/trait.Element.html) paint lifecycle for gauges and graphs, and its input/focus APIs for keyboard-operable custom controls. Treat detailed X11/Wayland lifecycle behavior as a spike result, not an assumption.

### Sources

- [Initial NZXT GitHub and Linux ecosystem report](../nzxt-linux-github-research.md)
- [GPUI documentation](https://docs.rs/gpui/latest/gpui/index.html)
- [liquidctl Kraken X3/Z3 guide](https://github.com/liquidctl/liquidctl/blob/main/docs/kraken-x3-z3-guide.md)
- [Linux kernel nzxt-kraken3 documentation](https://docs.kernel.org/hwmon/nzxt-kraken3.html)
- [OpenRGB NZXT HUE 2 detector](https://gitlab.com/CalcProgrammer1/OpenRGB/-/blob/42542b6b676738c793bb2a84258498c6fe96e8ac/Controllers/NZXTHue2Controller/NZXTHue2ControllerDetect.cpp)

## Assumptions & Constraints

### Assumptions (to validate)

- `1e71:300e` exposes an LCD endpoint that can coexist with the bound `kraken2023` driver. This is not proven by current `hwmon` evidence and is validated by US-016.
- `1e71:2021` matches the channel topology and safe command subset documented by OpenRGB. This is validated by US-013 before production writes exist.
- GPUI meets the resource, keyboard and X11/Wayland targets on Fedora 44. Validated by US-001 on 2026-08-06: launch time, keyboard traversal, 200% scale and backend-failure handling all pass. The original memory and CPU figures did not, and were replaced by the measured budgets in Non-Functional Requirements rather than by a change of stack.
- The kernel curve points correspond to the documented 20-59 degrees Celsius range and accept a full 40-value update. US-002 records the exact ABI before US-010 can write.
- A per-user systemd daemon can regain both devices after suspend with udev-granted access. US-003 and US-019 validate this.

### Hard Constraints

- Initial certified platform: Fedora 44 on x86_64, Linux `7.1.5-200.fc44.x86_64`.
- Initial device allowlist: Kraken Base `1e71:300e` and RGB Controller `1e71:2021`.
- Thermal control uses `hwmon` while `kraken2023` is bound. Direct USB detachment of the thermal interface is prohibited.
- Direct hardware writes are prohibited for unknown VID/PID, unknown firmware or an unvalidated capability.
- The GUI and daemon run without root privileges after installation.
- Normal operation opens zero network sockets and performs zero network requests.
- No NZXT CAM name, NZXT logo, copyrighted CAM assets or claim of NZXT affiliation appears in the product identity.
- The project license is GPL-3.0-or-later before code is copied or adapted from GPL ecosystem projects.

## Quality Gates

These commands must pass for every user story:

- `cargo fmt --all -- --check` - all Rust source is formatted.
- `cargo check --workspace --all-targets` - the complete workspace type-checks.
- `cargo clippy --workspace --all-targets -- -D warnings` - no Clippy warning is accepted.
- `cargo test --workspace` - unit, integration and mocked hardware tests pass.

For UI stories, additional gates:

- Launch the native build under Wayland and X11 at 920x640 and 1280x720, verify 100% and 200% scale, complete the screen by keyboard only, and capture the result for comparison with the five supplied CAM references.
- Live hardware tests use only the explicit VID/PID allowlist, record the firmware and restore the previous safe state before completion.

## Epics & User Stories

### EP-001: Validated Native Foundation

Establish the Rust/GPUI workspace, prove the Linux UI choice and create the only permitted hardware ownership boundary before feature development.

**Definition of Done:** the native shell runs on Wayland and X11 within the resource budget, both devices have an evidence-backed capability matrix, one daemon owns writes through typed local IPC, configuration survives corruption, and the repository documentation reflects the hardware-only scope.

#### US-001: Validate GPUI on the target Linux desktop

**Description:** As the owner-operator, I want a measured GPUI vertical slice so that the project commits to the native stack only after its Linux behavior is proven.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** None

**Acceptance Criteria:**

- [ ] Given Fedora 44, when the spike launches under Wayland and X11, then it opens a 920x640 native window containing two selects, four color controls, a rotate action and a custom-painted circular preview.
- [ ] Given a release build on the development machine, when measured across five cold launches, then median time to first visible frame is <=700 ms, idle `RssAnon` is <=110 MiB, idle total `VmRSS` is <=320 MiB and five-minute average idle CPU is <=1.5%.
- [ ] Given keyboard-only input, when focus traverses the spike, then every control is reachable once in a logical order and exposes a visible focus state.
- [ ] Given 200% desktop scale, when the window renders at 920x640 logical pixels, then no label, menu or preview is clipped.
- [ ] Given an unavailable display backend or GPUI initialization failure, when launch is attempted, then the process exits without panic and reports the failing backend and next diagnostic action.
- [ ] Given the hardware-only scope in `README.md`, when the story completes, then the implemented workspace matches it and introduces no browser runtime dependency.

#### US-002: Validate the exact hardware capability matrix

**Description:** As the owner-operator, I want a read-only capability probe so that no feature is inferred from a product-family name.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-001

**Acceptance Criteria:**

- [ ] Given the development machine, when the probe runs, then it records VID, PID, serial when exposed, firmware when exposed, USB interfaces, bound kernel drivers and mapped `hwmon` attributes for both devices.
- [ ] Given `kraken2023`, when attributes are inspected, then liquid temperature, both RPM/PWM channels, enable modes and all available curve points are represented in a versioned capability record.
- [ ] Given an attribute or firmware field that is unavailable, when the record is generated, then its state is `unknown` with source evidence and never a fabricated default.
- [ ] Given an unknown VID/PID, when the probe discovers it, then the device is reported as unsupported and no writable file or USB endpoint is opened.
- [ ] Given a completed probe, when another engineer reads the artifact, then each later RGB, cooling and LCD story can identify its exact capability prerequisite.

#### US-003: Enforce one unprivileged hardware writer

**Description:** As the owner-operator, I want a restricted daemon to serialize hardware access so that window crashes and competing tools cannot issue concurrent writes.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-001, US-002

**Acceptance Criteria:**

- [ ] Given a normal user session, when the daemon starts, then it acquires one process lock per supported device and exposes only a user-owned Unix socket.
- [ ] Given a GPUI client, when it requests telemetry or a validated command, then the daemon authenticates the local peer, validates the typed message and serializes the operation.
- [ ] Given a second daemon, liquidctl, CoolerControl or OpenRGB holding a conflicting endpoint, when ownership fails, then this application enters read-only mode and identifies the conflict without forcing access.
- [ ] Given an out-of-range, unknown or malformed IPC command, when it reaches the daemon, then it produces zero hardware writes and returns a typed rejection.
- [ ] Given the GPUI process disconnecting or crashing, when the daemon remains alive, then the last onboard thermal program continues and no fallback depends on the UI event loop.
- [ ] Given normal operation for 30 minutes, when sockets are inspected, then the application has zero listening TCP/UDP sockets.

#### US-004: Build the native shell and component contract

**Description:** As the owner-operator, I want a compact four-destination shell so that every supported hardware task is reachable without unrelated features.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-001

**Acceptance Criteria:**

- [ ] Given the app shell, when it opens, then the only primary destinations are Monitoring, Cooling, Lighting and LCD, plus a secondary Settings entry.
- [ ] Given the supplied CAM references, when the shell is compared, then it uses a fixed dark navigation rail, charcoal work surface, low-contrast separators, one violet selection accent and tabular numerals without copying NZXT logos or assets.
- [ ] Given repeated interface needs, when implemented, then centralized tokens and reusable Button, Select, Toggle, Slider, ColorField, Panel, DeviceRow, Gauge and CurveEditor primitives exist with hover, focus, active, disabled and error states.
- [ ] Given a 920x640 window, when any destination is displayed, then the primary task has no horizontal scroll and all pointer targets are at least 40x40 logical pixels.
- [ ] Given no supported device, missing permission or read-only conflict, when a destination opens, then write controls are disabled and one actionable state message replaces fabricated values.
- [ ] Given an opened select or color popover near a window edge, when its preferred placement does not fit, then it remains fully visible within the window.

#### US-005: Persist configuration and diagnostics locally

**Description:** As the owner-operator, I want atomic local configuration and inspectable diagnostics so that settings survive restarts without creating a cloud dependency.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-003

**Acceptance Criteria:**

- [ ] Given a valid profile change, when it is saved, then a schema-versioned local configuration is written atomically and the prior valid file remains recoverable until commit succeeds.
- [ ] Given a restart, when the configuration is valid, then the daemon loads the same active profile without network access.
- [ ] Given a truncated, corrupted or future-version configuration, when loading fails, then the file is preserved for diagnostics, the last known-good or built-in safe profile is selected, and the UI names the recovery action.
- [ ] Given device or IPC activity, when diagnostics are recorded, then timestamps, capability IDs, state transitions and typed errors are included while serial numbers are redacted by default.
- [ ] Given an export action, when diagnostics are written, then the archive contains no credentials, environment variables, arbitrary home-directory files or network data.

---

### EP-002: Monitoring and Thermal Control

Deliver the daily monitoring and cooling jobs through the kernel-backed path, including explicit invalid, stale and safety states.

**Definition of Done:** Kraken and system metrics update within 1.5 seconds, fixed and curve controls write only validated values, profiles recover after restart/wake, and the Cooling screen exposes every active safety or ownership state.

#### US-006: Stream Kraken telemetry from hwmon

**Description:** As the owner-operator, I want current liquid, pump and fan readings so that cooling state is observable without terminal commands.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-002, US-003

**Acceptance Criteria:**

- [ ] Given the bound `kraken2023` device, when telemetry runs, then liquid temperature, pump RPM, fan RPM and both current PWM values are sampled once per second.
- [ ] Given a new valid sample, when it reaches the GPUI client, then its age is <=1.5 seconds at P95 over 10 minutes.
- [ ] Given a missing channel or unreadable attribute, when a sample is produced, then the value is `unavailable` with a typed cause rather than zero.
- [ ] Given a temporary read failure, when the prior value remains visible, then it is marked stale within 2 seconds and removed after 10 seconds without a valid replacement.
- [ ] Given 10 minutes of sampling, when the filesystem write count is inspected, then telemetry has performed zero writes to `hwmon`.

#### US-007: Stream CPU, GPU and RAM telemetry

**Description:** As the owner-operator, I want local system metrics so that the monitoring dashboard and LCD infographic use the same normalized data.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-001, US-003

**Acceptance Criteria:**

- [ ] Given the development machine, when telemetry runs, then CPU load/temperature, GPU load/temperature and RAM used/total are sampled once per second from local OS or vendor interfaces.
- [ ] Given a valid sample, when normalized, then percentages are clamped to 0-100, temperatures retain one decimal place and memory uses an explicit binary unit.
- [ ] Given a missing GPU driver, sensor permission or unsupported metric, when rendering occurs, then the individual value displays `N/A` while other metrics continue updating.
- [ ] Given a 30-minute capture, when traffic is inspected, then metric collection has made zero network requests.
- [ ] Given an individual collector panic or timeout, when isolation handles it, then the daemon remains alive and reports the failed collector.

#### US-008: Render the monitoring dashboard

**Description:** As the owner-operator, I want a dense live dashboard so that CPU, GPU, RAM and Kraken state can be understood in one view.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-004, US-006, US-007

**Acceptance Criteria:**

- [ ] Given valid telemetry, when Monitoring opens, then CPU, GPU, RAM and Kraken sections show current values, units and one dominant gauge or bar per section.
- [ ] Given 15 minutes of samples, when a chart is shown, then it uses an in-memory rolling window and writes no history database.
- [ ] Given 920x640 and 1280x720 windows, when the dashboard renders, then all four sections remain reachable without horizontal scrolling.
- [ ] Given a stale or unavailable metric, when the dashboard updates, then the affected value and chart gap are visually distinct and meaning is not conveyed by color alone.
- [ ] Given values with different digit widths, when they update, then tabular numerals prevent adjacent labels from moving by more than 1 logical pixel.

#### US-009: Apply validated fixed pump and fan duty

**Description:** As the owner-operator, I want fixed cooling control so that I can select a known duty without issuing raw kernel writes.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-002, US-003, US-006

**Acceptance Criteria:**

- [ ] Given a supported pump or fan channel, when a fixed duty inside its validated range is applied, then the daemon writes it once and reports the resulting mode/value readback when available.
- [ ] Given repeated requests for the current value, when they arrive, then the daemon deduplicates them and performs zero additional device writes.
- [ ] Given a duty below the validated safe minimum, above the maximum or containing a non-number, when Apply is requested, then no write occurs and the control identifies the accepted range.
- [ ] Given a write timeout or partial kernel error, when Apply fails, then the prior confirmed state remains active in the UI and the hardware state is marked uncertain until readback succeeds.
- [ ] Given a channel classified as read-only or absent by US-002, when the screen renders, then fixed control is disabled.

#### US-010: Edit and apply a safe liquid-temperature curve

**Description:** As the owner-operator, I want a visual cooling curve so that onboard cooling responds to liquid temperature without depending on the UI process.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-002, US-003, US-009

**Acceptance Criteria:**

- [ ] Given the validated 40-point kernel ABI, when the editor opens, then it presents 10 control nodes over 20-59 degrees Celsius and linearly interpolates exactly 40 integer PWM values.
- [ ] Given an edited curve, when validation runs, then temperature order is fixed, PWM values remain within the channel's safe range and duty is monotonically non-decreasing.
- [ ] Given pointer or keyboard movement of a node, when editing continues, then no hardware write occurs until Apply is explicitly activated.
- [ ] Given Apply, when the daemon receives the curve, then it prevalidates all 40 values, serializes one curve transaction and records readback for every attribute that supports it.
- [ ] Given a failure after one or more kernel attributes changed, when the transaction aborts, then restoration of the complete last known-good curve is attempted and the UI reports confirmed or uncertain hardware state.
- [ ] Given liquid temperature at or above 60 degrees Celsius, when the driver/firmware failsafe is active, then the application neither disables nor overrides the 100% safety behavior.

#### US-011: Save, activate and recover named profiles

**Description:** As the owner-operator, I want named local profiles so that a validated cooling configuration can be restored after launch and wake.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-005, US-009, US-010

**Acceptance Criteria:**

- [ ] Given a valid fixed or curve configuration, when saved with a unique non-empty name of 1-48 characters, then it appears in the Active Profile selector.
- [ ] Given an active profile, when the daemon restarts or resumes, then it redetects the device, revalidates capabilities and restores the profile within 5 seconds of device availability.
- [ ] Given a profile created for a different VID/PID, firmware or capability set, when activation is attempted, then no hardware write occurs and incompatibilities are listed.
- [ ] Given the active profile is deleted, when deletion is confirmed, then the built-in safe profile becomes active before the file is removed.
- [ ] Given configuration corruption, when recovery occurs, then the built-in safe profile is selected and the corrupt profile remains exportable for diagnosis.

#### US-012: Complete the Cooling control surface and safety states

**Description:** As the owner-operator, I want one Cooling screen for pump, fan and curve state so that control, readback and faults are visible together.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-004, US-006, US-009, US-010, US-011

**Acceptance Criteria:**

- [ ] Given connected hardware, when Cooling opens, then pump and fan rows show RPM, PWM, active mode, temperature source and profile selector above the curve.
- [ ] Given a mode dropdown, when Fixed, Curve or a named profile is selected, then the pending selection is distinct from confirmed hardware state until Apply succeeds.
- [ ] Given liquid temperature >=60 degrees Celsius, or RPM remains zero for three consecutive samples while commanded duty is non-zero, when the condition occurs, then a critical state is displayed within 2 seconds with the affected channel and current readback.
- [ ] Given read-only conflict, lost permission, unplug or stale telemetry, when the condition occurs, then all write controls disable within 2 seconds and retain diagnostic context.
- [ ] Given keyboard-only operation, when the curve and mode controls are used, then every edit, Apply and Cancel action is possible without pointer input.

---

### EP-003: Validated RGB Controller

Validate the exact topology and expose only commands proven safe on the owned `1e71:2021` controller.

**Definition of Done:** channel and LED topology are recorded, fixed/off control works per validated channel, no unknown command can reach the device, and only explicitly tested effects appear in profiles.

#### US-013: Validate the RGB Controller protocol and topology

**Description:** As the owner-operator, I want an evidence-backed RGB capability probe so that Lighting never guesses channel layout or packet format.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-002, US-003

**Acceptance Criteria:**

- [ ] Given `1e71:2021`, when the read-only probe runs, then firmware when exposed, interfaces, endpoints, channel count and any readable LED metadata are recorded.
- [ ] Given explicit operator confirmation and a captured prior state, when the write probe runs, then only an allowlisted low-brightness fixed color and off command are tested one channel at a time.
- [ ] Given each tested command, when it completes, then packet bytes, response, channel, observed result and maximum stable command cadence are recorded without serial-number publication.
- [ ] Given channel count, LED count or firmware remains unknown, when a production write is requested, then Lighting stays read-only and reports the missing evidence.
- [ ] Given the probe fails or the device disconnects, when cleanup runs, then it attempts to restore the captured prior state and reports whether restoration was confirmed.

#### US-014: Control fixed RGB color and off state

**Description:** As the owner-operator, I want per-channel fixed color and off controls so that the most common lighting task is available without exposing raw effects.

**Priority:** P0
**Size:** M (3 pts)
**Dependencies:** Blocked by US-004, US-005, US-013

**Acceptance Criteria:**

- [ ] Given a validated channel, when Lighting opens, then its name, LED count when known, current confirmed mode, brightness and color are shown.
- [ ] Given a valid six-digit hexadecimal color and 0-100% brightness, when Apply is activated, then the preview updates immediately and one rate-limited hardware command follows.
- [ ] Given Off, when applied, then the channel emits zero light and its prior fixed color remains available for restoration.
- [ ] Given invalid hex, unsupported channel or command cadence above the US-013 limit, when Apply is requested, then no hardware write occurs and the exact invalid field is identified.
- [ ] Given a reconnect, when the active profile is compatible, then confirmed fixed/off state is restored within 5 seconds.

#### US-015: Add only validated RGB effects

**Description:** As the owner-operator, I want a small allowlist of tested effects so that animation does not expand into unsafe protocol experimentation.

**Priority:** P1
**Size:** M (3 pts)
**Dependencies:** Blocked by US-013, US-014

**Acceptance Criteria:**

- [ ] Given US-013 proves the command sequence, when the story completes, then Breathing and Spectrum Wave are the only additional effect candidates exposed.
- [ ] Given an effect supports speed or direction on this firmware, when its control appears, then only values validated by the capability record can be selected.
- [ ] Given an effect or parameter is not proven on `1e71:2021`, when Lighting renders, then it is absent rather than shown as disabled or emulated.
- [ ] Given an effect write fails, when state is refreshed, then the last confirmed mode remains selected and the pending mode is discarded.
- [ ] Given a named profile, when a validated effect is saved and reactivated, then its channel parameters round-trip without raw protocol data entering the configuration file.

---

### EP-004: Native Kraken LCD

Prove the LCD transport and deliver the specific native editor and dual-infographic experience selected from the reference screens.

**Definition of Done:** the exact panel resolution/format and safe transfer path are known, one editor model drives both GPUI preview and device framebuffer, and a CPU/GPU infographic remains synchronized for 30 minutes without disrupting `hwmon`.

#### US-016: Validate the LCD transport on `1e71:300e`

**Description:** As the owner-operator, I want a bounded LCD transport spike so that the product does not assume resolution, pixel format or endpoint behavior from another Kraken generation.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-002, US-003

**Acceptance Criteria:**

- [ ] Given `1e71:300e`, when USB descriptors and firmware are inspected, then panel resolution, shape, supported orientation/brightness commands, transfer endpoint and pixel format are recorded or explicitly marked unknown.
- [ ] Given an identified safe transfer sequence, when an original solid-color test frame is sent, then the physical panel displays the expected orientation and color without detaching the `kraken2023` thermal interface.
- [ ] Given dynamic transfer at 1 frame per second for 30 minutes, when telemetry runs concurrently, then there are zero lost `hwmon` samples longer than 2 seconds and zero unhandled USB errors.
- [ ] Given the model, firmware or endpoint cannot be proven, when a frame write is requested, then LCD remains read-only and no sequence from another PID is attempted.
- [ ] Given disconnect during a transfer, when the operation aborts, then the daemon releases the endpoint, remains alive and permits a fresh capability probe after reconnect.

#### US-017: Build the LCD editor, preview and static output

**Description:** As the owner-operator, I want a CAM-inspired native editor so that LCD layout and colors can be configured with immediate visual evidence.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-004, US-005, US-016

**Acceptance Criteria:**

- [ ] Given the LCD destination, when it opens, then it contains display-mode and metric selects, Reading 1/2 colors, Text 1/2 colors, Background, Logo color, Rotate Display and a circular or square preview matching the validated panel.
- [ ] Given any editor change, when state updates, then the GPUI preview repaints within 16.7 ms at P95 without writing to hardware.
- [ ] Given Apply, when rendering completes, then the same typed `DisplayPreset` produces both the preview and the exact-resolution offscreen framebuffer sent to the daemon.
- [ ] Given a six-digit hexadecimal field, when input is invalid, incomplete or out of gamut, then Apply remains disabled, the prior valid preview remains visible and no frame is sent.
- [ ] Given a user-provided static image, when decoding fails or dimensions exceed 8192x8192, then the file is rejected without panic and no partial frame reaches the device.
- [ ] Given product branding, when the default preview renders, then it uses the project's own wordmark or no logo and never embeds the NZXT logo.

#### US-018: Render and stream the dual CPU/GPU infographic

**Description:** As the owner-operator, I want the selected dual-infographic LCD view so that CPU and GPU temperature remain readable on the physical Kraken.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-007, US-016, US-017

**Acceptance Criteria:**

- [ ] Given valid CPU and GPU temperature samples, when Dual Infographic is active, then the LCD shows two colored arcs, two temperatures, CPU/GPU labels and the project wordmark using the selected colors.
- [ ] Given live telemetry, when streaming runs, then the framebuffer updates once per second and displayed data age is <=2 seconds at P95 over 30 minutes.
- [ ] Given one unavailable metric, when a frame renders, then its value is `--` and its arc enters a neutral unavailable state rather than showing zero degrees.
- [ ] Given Rotate Display, when toggled, then preview and physical output rotate together by the validated panel increment and preserve text alignment.
- [ ] Given 30 minutes of output, when resource usage is measured, then LCD rendering adds <=0.5 percentage points average CPU and queues no more than one unsent frame.
- [ ] Given USB backpressure or a transfer failure, when a new metric sample arrives, then stale pending frames are dropped and the daemon retries only after reconnect or an explicit recoverable state.

---

### EP-005: Reliability and Fedora Distribution

Prove lifecycle recovery across all supported features and package the application without broad device permissions or hidden network behavior.

**Definition of Done:** the release passes an 8-hour hardware soak, 10 suspend/resume cycles, reconnect and conflict tests, and installs/uninstalls on Fedora 44 with restricted udev and systemd user assets.

#### US-019: Harden hotplug, suspend and failure recovery

**Description:** As the owner-operator, I want predictable lifecycle recovery so that cooling, RGB and LCD do not depend on a perfect desktop session.

**Priority:** P0
**Size:** L (5 pts)
**Dependencies:** Blocked by US-011, US-014, US-018

**Acceptance Criteria:**

- [ ] Given either supported device is unplugged and reconnected, when it becomes available, then detection, capability validation and compatible profile restoration complete within 5 seconds.
- [ ] Given 10 suspend/resume cycles, when the system returns, then the daemon reacquires each device exactly once and telemetry resumes within 5 seconds on every cycle.
- [ ] Given an 8-hour mixed monitoring, cooling, RGB and LCD soak, when logs are inspected, then there are zero unhandled errors, duplicate writers, queues above 100 commands or stale telemetry periods above 10 seconds.
- [ ] Given the daemon is terminated while an onboard curve is active, when the UI loses IPC, then it reports the daemon loss within 2 seconds and the hardware retains its last confirmed autonomous curve.
- [ ] Given a conflicting writer appears mid-session, when ownership or I/O conflict is detected, then writes stop, the UI becomes read-only and no automatic force-detach occurs.
- [ ] Given recovery cannot confirm hardware state, when the device returns, then the daemon selects read-only uncertain state instead of replaying queued writes.

#### US-020: Package and install the Fedora release

**Description:** As the owner-operator, I want a reproducible Fedora package so that installation grants only required access and starts the background service without manual kernel-file commands.

**Priority:** P1
**Size:** M (3 pts)
**Dependencies:** Blocked by US-003, US-005, US-019

**Acceptance Criteria:**

- [ ] Given a clean Fedora 44 user account, when the RPM is installed, then the GPUI desktop entry, application binary, daemon binary, systemd user unit, exact VID/PID udev rules, GPL license and third-party notices are present.
- [ ] Given the user logs in after installation, when the service starts, then it runs without root and the GUI can read both supported devices through the local socket.
- [ ] Given `lsusb`, when unrelated USB devices are inspected, then the installed udev policy has granted no additional VID/PID access.
- [ ] Given package removal, when uninstall completes, then binaries, desktop entry, service and udev rules are removed while user profiles remain unless explicit data deletion is requested.
- [ ] Given an unsupported distribution or missing runtime dependency, when installation or launch fails, then the error names Fedora 44 as the certified target and identifies the missing dependency without modifying unrelated system configuration.

## Functional Requirements

- FR-01: The system must detect and write only to the explicit `1e71:300e` and `1e71:2021` allowlist until a later PRD adds another validated device.
- FR-02: The system must maintain a versioned capability record per VID/PID and firmware.
- FR-03: One per-user daemon must be the sole hardware writer and the GPUI client must use a local Unix socket.
- FR-04: The system must read Kraken thermal data and apply supported PWM/curve control through `hwmon`.
- FR-05: The system must sample Kraken, CPU, GPU and RAM metrics once per second.
- FR-06: The system must distinguish valid, stale, unavailable, read-only and uncertain values.
- FR-07: The system must support validated fixed pump/fan duty and a monotonic liquid-temperature curve.
- FR-08: The system must validate a complete curve before the first kernel write and attempt last-known-good restoration after partial failure.
- FR-09: The system must save named local profiles and restore a compatible active profile after daemon start, reconnect and wake.
- FR-10: The system must provide Monitoring, Cooling, Lighting and LCD as the only primary destinations.
- FR-11: The system must support RGB fixed color and off only after US-013 validates the exact controller.
- FR-12: The system must expose no RGB effect or parameter absent from the validated allowlist.
- FR-13: The system must send LCD frames only after US-016 proves the exact transport for `1e71:300e`.
- FR-14: The LCD editor must use one `DisplayPreset` for preview and device output.
- FR-15: The system must include the dual CPU/GPU temperature infographic selected in the visual reference.
- FR-16: The system must retain no metric history beyond a 15-minute in-memory rolling window.
- FR-17: The system must make zero network requests and expose no HTTP, TCP or UDP service.
- FR-18: The GUI and daemon must not require root after package installation.
- FR-19: The system must enter read-only state instead of forcing access when ownership or capability is uncertain.
- FR-20: The product must identify itself as unofficial and use original branding and assets.

## Non-Functional Requirements

- **Performance:** median cold start to first visible frame <=700 ms across five launches; idle `RssAnon` <=110 MiB for Month 1 and <=100 MiB for Month 6; idle total `VmRSS` <=320 MiB throughout; 30-minute average idle CPU <=1.5% for Month 1 and <=1.2% for Month 6. Measured from `/proc/{pid}/status` with the window open and untouched. `RssAnon` is the figure this project controls; total `VmRSS` is dominated by the GPU driver and shader-compiler mappings GPUI links against, is shared with every other GPU client on the machine, and varies with the installed driver. See the GPUI measurement in Research Findings.
- **Telemetry latency:** sampling interval 1 second; valid sample age <=1.5 seconds at P95; UI marks data stale after 2 seconds and unavailable after 10 seconds.
- **Control latency:** a valid fixed-duty or RGB Apply command reaches confirmed state or a typed failure within 500 ms at P95 when the device is connected.
- **Rendering:** GPUI preview repaint <=16.7 ms at P95; LCD output frequency exactly 1 frame per second for the dual infographic; at most one unsent LCD frame.
- **Security:** zero network requests, zero listening TCP/UDP sockets, zero GUI/daemon processes running as root, and udev access restricted to two allowlisted VID/PID pairs.
- **Accessibility:** 100% of actions usable by keyboard; visible focus on every interactive control; text contrast >=4.5:1, meaningful non-text contrast >=3:1; no clipping at 200% scale.
- **Capacity:** one daemon supports the two target USB devices, up to eight telemetry streams and up to six RGB channels without a command queue exceeding 100 entries.
- **Reliability:** zero unhandled errors during an 8-hour soak; successful recovery in all 10 consecutive suspend/resume cycles; supported-device reconnect <=5 seconds.
- **Durability:** configuration writes are atomic; the last known-good profile survives 100 consecutive save/restart cycles with zero value changes.
- **Privacy:** diagnostics redact USB serial numbers by default and collect zero credentials, environment variables, browsing data or files outside the application's configuration/log directories.

## Edge Cases & Error States

| # | Scenario | Trigger | Expected Behavior | User Message |
|---|----------|---------|-------------------|--------------|
| 1 | No supported device | First launch with no allowlisted VID/PID | Open read-only shell, show detection evidence, disable writes | "No supported NZXT device detected." |
| 2 | Capability probe active | Device present but firmware/topology unresolved | Show progress and keep every write path closed | "Validating hardware capabilities..." |
| 3 | Kernel read/write error | `hwmon` returns I/O, permission or missing-file error | Preserve confirmed state, mark affected path stale/uncertain, log typed cause | "Cooling state could not be confirmed. No further writes were sent." |
| 4 | Network unavailable | Any network state | Continue unchanged because the application performs no network operation | No message |
| 5 | Permission missing or revoked | udev rule absent, device node permissions change | Enter read-only state and show exact device/path remediation | "Hardware access denied for 1e71:300e. Check the installed udev rule." |
| 6 | Concurrent writer | CoolerControl, OpenRGB, liquidctl or second daemon owns endpoint | Stop writes and retain monitoring where safe | "Another process owns this device. Controls are read-only." |
| 7 | Boundary value | Duty, curve, color, image or profile name outside validated bounds | Reject before hardware/config write and focus invalid field | "Value rejected. Accepted range: {validated range}." |
| 8 | Reversal | User deletes active profile or cancels pending edit | Activate built-in safe profile before deletion; discard pending edits | "Safe profile activated before deletion." |
| 9 | Interrupted flow | Unplug, daemon crash or suspend during write | Drop queued commands, reconnect read-only until state is revalidated | "Device reconnected. Verifying state before restoring controls." |
| 10 | External firmware variance | Known VID/PID with unknown firmware or changed endpoint | Report unsupported firmware and prohibit speculative commands | "Firmware {version} is not validated for this operation." |
| 11 | Corrupt local data | Truncated or future-version configuration | Preserve file, load last-known-good/safe defaults, allow diagnostics export | "Configuration could not be loaded. Safe defaults are active." |
| 12 | Missing telemetry source | GPU driver or sensor unavailable | Show `N/A`, retain other collectors and LCD output | "GPU temperature unavailable." |

## Risks & Mitigations

| # | Risk | Probability | Impact | Mitigation |
|---|------|------------|--------|------------|
| 1 | Incorrect reverse-engineered RGB/LCD command changes or damages device state | Med | High | Separate US-013/US-016 spikes, exact VID/PID/firmware allowlist, captured prior state, rate limits and no cross-model fallback |
| 2 | Concurrent access from liquidctl, CoolerControl or OpenRGB creates undefined writes | High | High | Per-device locks, serialized daemon, read-only conflict state and prohibition on force-detach |
| 3 | GPUI Linux lifecycle or custom controls fail resource/accessibility targets | Med | High | US-001 go/no-go spike before component expansion; separate Wayland/X11 and keyboard tests |
| 4 | Partial `hwmon` curve write leaves uncertain state | Med | High | Full prevalidation, last-known-good snapshot, serialized transaction, restoration attempt and explicit uncertain state |
| 5 | LCD USB access disrupts the bound thermal interface | Med | High | US-016 coexistence test; prohibit thermal interface detachment; block LCD if separation is not proven |
| 6 | Firmware variation makes product-family support claims false | High | Med | Versioned capability matrix and exact per-firmware compatibility reporting |
| 7 | GPL or trademark misuse blocks public distribution | Low | High | GPL-3.0-or-later, third-party notices, no unlicensed code import, original product identity and legal review before first public release |
| 8 | Four product surfaces exceed the available maintenance capacity | Med | Med | Limit v1 to two owned devices, 20 session-sized stories and no plugin/generalization layer |

## Non-Goals

Explicit boundaries for this version:

- Web Integrations, HTML rendering, browser engines, WebViews and remote pages.
- Cloud accounts, login, synchronization, analytics, telemetry, remote API or HTTP server.
- Firmware download, update, recovery or modification.
- Support for NZXT keyboards, mice, capture cards, monitors, audio devices, PSUs or cases beyond the two explicit USB devices.
- Universal hardware abstraction, plugin SDK or automatic support for unvalidated NZXT models.
- GPU overclocking, case fan controllers outside `1e71:2021`, or non-NZXT RGB ecosystems.
- System-wide process tables, network monitoring and storage dashboards.
- Tray/minimize-to-tray behavior, mobile UI and localization in v1.
- Reuse of the NZXT CAM product name, NZXT logo, screenshots or copyrighted assets in the shipped interface.

## Technical Considerations

These questions require engineering confirmation during their validation stories:

- **Architecture:** Should the sole-writer boundary be a `systemd --user` daemon plus GPUI client over Unix socket, or a supervised background mode of one binary? Recommended: separate daemon/client processes because wake recovery and window lifecycle are independent. US-003 confirms Fedora access and process cost.
- **Thermal data path:** Should all supported Kraken thermal control remain on `hwmon` while direct USB is reserved for a separately proven LCD interface? Recommended: yes, with no kernel-driver detachment. US-002 and US-016 confirm interface boundaries.
- **RGB transport:** Should the RGB controller use a maintained HID crate or a narrower direct backend? Recommended: decide after US-013 identifies report format, hotplug needs and kernel binding. No library API is assumed before that spike.
- **GPUI components:** Should the project build the nine required primitives directly or adopt a GPUI component crate? Recommended: implement the bounded set after US-001, unless the spike proves a maintained dependency meets keyboard, resource and styling requirements with less code.
- **LCD renderer:** Should the offscreen framebuffer use a small CPU rasterizer or a GPUI-compatible headless path? Recommended: choose after US-016 establishes resolution, format and one-frame-per-second budget. Preview and device output must still share the typed `DisplayPreset`, not extracted GUI pixels.
- **System telemetry:** Which local interfaces should provide NVIDIA GPU metrics without introducing a daemon-sized dependency? Recommended: compare maintained NVML bindings and available sysfs data during US-007; unsupported metrics remain `N/A`.
- **Configuration:** Should profiles use TOML or JSON with Serde? Recommended: TOML for operator inspection, with schema version, atomic replacement and last-known-good recovery.
- **Licensing:** Is GPL-3.0-or-later sufficient for every adapted liquidctl/OpenRGB fragment and GPUI dependency? Recommended: generate a dependency/license inventory and complete legal review before US-020 publishes an RPM.
- **Migration:** The repository now uses the technical name `nzxt-control-linux` and the research report is retained as historical evidence. Recommended: keep the public product name independent from the repository name. No compatibility migration is required because no application code or user data exists.

## Success Metrics

| Metric | Baseline (current) | Target | Timeframe | How Measured |
|--------|-------------------|--------|-----------|-------------|
| Supported devices with validated capability record | 0 | 2 exact VID/PID records | Month 1 | Probe artifact and mocked/live integration tests |
| Functional primary destinations | 0 of 4 | 4 of 4 with zero placeholder write controls | Month 6 | Acceptance walkthrough and captured native screens |
| Stable mixed-operation session | 0 minutes | 8 hours with zero unhandled error | Before v1 release | Timestamped daemon soak log |
| Suspend/resume recovery | Untested | 10 of 10 cycles recover within 5 seconds | Before v1 release | Automated timestamps plus manual hardware confirmation |
| Median cold start | N/A, no application | <=700 ms across five launches | Month 1 | Monotonic launch instrumentation |
| Idle application memory | 81.3 MiB measured 2026-08-06 | `RssAnon` <=110 MiB Month 1; <=100 MiB Month 6 | Month 1 and Month 6 | `RssAnon` from `/proc/{pid}/status` sampled for 30 minutes |
| Idle resident set | 253.2 MiB measured 2026-08-06 | Total `VmRSS` <=320 MiB | Every release | `VmRSS` from `/proc/{pid}/status`; driver-dependent, tracked for regression only |
| Idle CPU | 1.10% measured 2026-08-06 | <=1.5% Month 1; <=1.2% Month 6 | Month 1 and Month 6 | Process CPU delta over 30 minutes |
| Network activity | Existing project has no runtime | 0 requests and 0 listening TCP/UDP sockets | Every release | Namespace/socket inspection plus 30-minute packet capture |
| Invalid command containment | No command layer | 100% of generated invalid/boundary cases produce zero hardware writes | Every hardware story | Mock backend write counter and property tests |
| Profile durability | No profiles | 100 save/restart cycles with zero changed values | Before v1 release | Automated round-trip test |

## Open Questions

- What exact commercial Kraken model and firmware correspond to `1e71:300e`? Owner: US-002; required before US-016.
- Does its LCD endpoint coexist with `kraken2023` without kernel-driver detachment? Owner: US-016; required before US-017.
- What channel and LED topology does the owned `1e71:2021` expose? Owner: US-013; required before US-014.
- Does GPUI meet the measured X11/Wayland, focus and resource budgets? Owner: US-001; required before US-004.
- Which original public product name should replace the technical repository label `nzxt-control-linux`? Owner: Arthur; required before US-020, not before implementation.
- Does the final dependency graph remain GPL-3.0-or-later compatible? Owner: US-020 license audit; required before public RPM distribution.
[/PRD]
