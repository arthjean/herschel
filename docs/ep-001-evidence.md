# EP-001 evidence map

Every acceptance criterion of the Validated Native Foundation, with the code
that implements it and the check that proves it. Automated checks are test
names inside `cargo test --workspace`; manual checks name the artifact.

Machine under test: Fedora 44, Linux `7.1.6-201.fc44.x86_64`, Wayland session
with XWayland available, NZXT Kraken Base `1e71:300e` (bcdDevice 0200) and NZXT
RGB Controller `1e71:2021` (bcdDevice 0105).

## US-001: Validate GPUI on the target Linux desktop

| Criterion | Implementation | Proof |
|---|---|---|
| 920x640 window with two selects, four color controls, a rotate action and a custom-painted circular preview | `crates/app/src/shell.rs` (LCD destination), `crates/app/src/preview.rs` | `docs/screenshots/lcd-color-popover.png`, `docs/screenshots/lcd-rotated-180.png`. The spike's own controls are gone: US-017 replaced this screen with the real editor, which carries three selects and six color controls over a preview rendered from the actual framebuffer. The screenshots above are kept because they are what the spike proved; the current equivalents are `the_screen_offers_only_the_modes_it_can_configure_completely`, `the_editor_exposes_the_six_color_controls_the_story_names`, `rotation_walks_the_validated_increment_and_returns_to_zero` and `docs/ep-004-evidence.md` |
| Median first frame <=700 ms over five cold launches; idle `RssAnon` <=110 MiB; idle `VmRSS` <=320 MiB; five-minute idle CPU <=1.5% | `crates/app/src/startup.rs`, `crates/app/src/main.rs` | Measurements below |
| Keyboard-only traversal in a logical order with a visible focus state | `Destination::tab_index`, `SCREEN_TAB_BASE`, `interactive()` focus style | `docs/screenshots/monitoring-keyboard-focus.png`; `rail_tab_order_is_stable_and_precedes_screen_controls` |
| No clipping at 200% desktop scale | Layout uses `min_w_0` and wrapping text throughout | `docs/screenshots/monitoring-200-percent.png` (1840x1280 device pixels for 920x640 logical) |
| Unavailable backend exits without panic, naming the backend and the next action | `startup::detect_backend`, `main.rs` preflight | `no_backend_reports_what_was_attempted_and_the_next_action`; manual: `env -u WAYLAND_DISPLAY -u DISPLAY ./target/release/nzxt-control` exits 1 with the diagnostic |
| Workspace matches the hardware-only scope in `README.md`, no browser runtime | `Cargo.toml`, `README.md`, `crates/app/src/offline.rs` | `cargo tree` contains no browser engine, WebView or HTTP server; see the network note below |

### Measurements

Release build, five cold launches, `NZXT_STARTUP_TRACE=1`:

| Launch | Wayland | X11 |
|---|---|---|
| 1 | 404.4 ms | 565.4 ms |
| 2 | 334.8 ms | 275.6 ms |
| 3 | 298.1 ms | 286.8 ms |
| 4 | 327.4 ms | measured over three launches |
| 5 | 295.8 ms | |

Median on Wayland: **327.4 ms**, against a 700 ms budget. Passes.

Re-measured on the certified build, after the review corrections below, five
cold launches on Wayland: 307.7, 370.6, 328.9, 301.0, 314.2 ms. Median
**314.2 ms**.

### Idle resource use

Release build (2024 edition, refusing HTTP client), window open and untouched,
sampled from `/proc/<pid>/status` over 300 seconds:

| Metric | Measured | Budget | Margin |
|---|---|---|---|
| `RssAnon`, what the process allocates | **81.3 MiB** | <=110 MiB Month 1, <=100 MiB Month 6 | 26% / 19% |
| Total `VmRSS` | **253.2 MiB** | <=320 MiB ceiling | 21% |
| Idle CPU, five-minute average | **1.10%** | <=1.5% Month 1, <=1.2% Month 6 | 27% / 8% |

PSS is 209.7 MiB. All within budget.

Re-measured on the certified build over a fresh 300-second sample: `RssAnon`
**81.1 MiB**, total `VmRSS` **253.2 MiB**, `RssFile` 172.0 MiB, idle CPU
**1.100%**, 30 threads. The review corrections touch startup path resolution, a
control gate and a document clone on write, and the figures reproduce, so the
budget is certified against the reviewed build rather than inherited from the
implementation run.

Those budgets are the ones PRD v1.2 sets, and they were derived from this
measurement rather than assumed before it. The v1.0 figures (100 MiB RSS, 1.0%
CPU) were unreachable with this stack, and the control measurement is what
established that: a GPUI window containing a single `div`, built from the same
GPUI 0.2.2 and the same release profile, costs **288.1 MiB and 1.00%** with the
same 30 threads. The empty window is more expensive than the complete shell.

The resident set breaks down as:

| Segment | Size |
|---|---|
| `RssFile` (shared library and device mappings) | 176.6 MiB |
| `RssAnon` (memory this process allocated) | 81.3 MiB |
| of which `[heap]` | 68.8 MiB |

The file-backed half is the graphics stack: `libnvidia-gpucomp` (17.8 MiB),
`libLLVM` for shader compilation (22.5 MiB across two mappings),
`libnvidia-glcore` (12.3 MiB), `libnvidia-rtcore` (19.0 MiB) and 20 MiB of
`/dev/nvidiactl` mappings, all shared with every other GPU client on the
machine. The 30 threads and the 1.0% idle CPU are present in the empty window
too, so they belong to GPUI's executor and the driver, not to the shell.

This is why the PRD now tracks two figures instead of one: `RssAnon` is what
the project controls and what the Month-6 target tightens, while total `VmRSS`
is a regression ceiling that moves with the installed driver.

## US-002: Validate the exact hardware capability matrix

| Criterion | Implementation | Proof |
|---|---|---|
| Records VID, PID, serial, firmware, USB interfaces, bound drivers and mapped `hwmon` attributes for both devices | `crates/hardware-linux/src/{usb,hwmon,probe}.rs` | `docs/capability-record.json`; `probe_records_identity_interfaces_and_hwmon_for_both_devices` |
| Liquid temperature, both RPM/PWM channels, enable modes and every curve point are represented | `probe::kraken_capabilities`, `hwmon::curve_channels` | `both_curve_channels_expose_forty_points`, `readings_carry_the_driver_label` |
| Unavailable attributes are `unknown` with evidence, never a fabricated default | `capability::Evidenced` | `absent_attributes_stay_unknown_with_their_source`, `identity_marks_absent_fields_unknown_with_evidence` |
| An unknown VID/PID is reported unsupported, with no writable file or endpoint opened | `core::is_allowlisted`, `sysfs::is_writable` uses `access(2)` | `unknown_devices_are_reported_and_never_opened`, `write_permission_is_tested_without_opening_the_file` |
| Later RGB, cooling and LCD stories can identify their capability prerequisite | `CapabilityId`, `CapabilityState::Unvalidated { required_story }` | `unvalidated_surfaces_name_the_story_that_unblocks_them`; the record names US-013 and US-016 |

The record shows the Kraken's interface 0 is class `0xff` with no bound driver,
while interface 1 carries `usbhid` and `kraken2023`. That is the coexistence
evidence US-016 needs, recorded rather than assumed.

## US-003: Enforce one unprivileged hardware writer

| Criterion | Implementation | Proof |
|---|---|---|
| One process lock per device, one user-owned Unix socket | `daemon/src/ownership.rs`, `daemon/src/server.rs`, `core::ipc::socket_path_from_env` | `one_lock_is_held_per_supported_device`, `the_socket_is_owner_only`, `the_first_holder_wins_and_the_second_is_told_who_holds_it`, `the_daemon_binds_the_socket_the_client_connects_to`; live: `srw-------` at `/run/user/1000/nzxt-control/nzxt-control.sock`, two locks held |
| Local peer authenticated, typed message validated, operation serialized | `server::peer_credentials` (SO_PEERCRED), `Arc<Mutex<Daemon>>` | `a_client_completes_the_handshake_and_reads_status`, `several_clients_are_served_without_interleaving` |
| A conflicting holder puts the application in read-only mode without forcing access | `Daemon::access_mode`, `ownership::observed_holders` | `read_only_mode_names_the_conflict`, `one_lock_is_held_per_supported_device` (second daemon is read-only) |
| Out-of-range, unknown or malformed commands produce zero hardware writes and a typed rejection | `Daemon::dispatch`, `ipc::read_frame` ceiling | `malformed_and_oversized_frames_are_refused_without_touching_hardware`, `out_of_range_values_are_rejected_with_their_accepted_range` (both compare a full mtime snapshot of the hwmon tree) |
| The daemon survives a client crash and the onboard program continues | one thread per connection, no hardware handle in the client | `the_daemon_survives_a_client_that_disappears_mid_request` |
| Zero listening TCP/UDP sockets | Only `UnixListener` exists in the codebase | `ss -tulpn` shows no entry for the serving daemon's pid, and `/proc/<pid>/fd` holds exactly one socket |

## US-004: Build the native shell and component contract

| Criterion | Implementation | Proof |
|---|---|---|
| Monitoring, Cooling, Lighting, LCD plus a secondary Settings | `shell::Destination` | `the_shell_exposes_four_primary_destinations_and_one_secondary`; screenshots |
| Fixed dark rail, charcoal surface, low-contrast separators, one violet accent, tabular numerals, no vendor asset | `crates/app/src/theme.rs` | `separators_stay_low_contrast_without_disappearing`, `body_text_meets_aa_on_every_surface`, `text_on_the_accent_meets_aa_in_every_interaction_state`; `numeric_font()` sets `tnum` on a fixed-advance family |
| Centralized tokens and nine primitives with hover, focus, active, disabled and error states | `theme.rs`, `components.rs` | `interactive()` is the single source of those states; `a_disabled_control_carries_its_reason`, `a_color_field_with_invalid_input_is_in_error_not_silently_reset` |
| No horizontal scroll at 920x640; pointer targets >=40x40 | `TARGET_MIN`, `min_w_0` on every flex column | `pointer_targets_are_at_least_forty_logical_pixels`; screenshots at 920x640 |
| No device, missing permission or read-only conflict disables write controls behind one actionable message | `link::LinkState::control_state` for capability-scoped controls, `LinkState::write_state` for the profile selector, `LinkState::banner` | `without_a_daemon_every_control_is_disabled_with_one_actionable_message`, `a_read_only_conflict_disables_controls_and_shows_the_conflict`, `an_unvalidated_capability_disables_its_control_and_names_the_story`, `a_read_only_conflict_also_disables_the_profile_selector`, `the_profile_selector_is_enabled_only_when_a_device_can_be_written` |
| A popover near a window edge stays fully visible | `popover_surface` uses `gpui::deferred(gpui::anchored().snap_to_window_with_margin(...))` | `docs/screenshots/lcd-color-popover.png` |

The popover placement is delegated to GPUI's `anchored` element rather than a
hand-rolled calculation. An earlier local `resolve_placement` helper was removed
once `anchored` covered the case: keeping both would have left dead code that
looks like it decides something.

## US-005: Persist configuration and diagnostics locally

| Criterion | Implementation | Proof |
|---|---|---|
| A saved profile is written atomically, the prior valid file surviving until commit | `config::write_atomically` (temp file, `fsync`, `rename`, directory `fsync`), `config::commit_change` rolls the in-memory document back when the write fails | `saving_replaces_the_file_atomically_and_leaves_no_temporary`, `a_saved_profile_survives_a_reload`, `a_failed_write_leaves_neither_the_file_nor_the_daemon_holding_the_profile` |
| A restart loads the same active profile without network access | `Configuration::load`, `Daemon::start` | `the_active_profile_survives_a_daemon_restart` |
| A truncated, corrupt or future-version file is preserved, safe defaults activate, the UI names the recovery action | `config::preserve`, `ConfigState::Recovered` | `a_truncated_file_is_preserved_and_safe_defaults_take_over`, `a_future_schema_version_is_a_recovery_case_not_a_guess`, `a_corrupt_configuration_recovers_to_the_safe_profile` |
| Diagnostics carry timestamps, capability ids, state transitions and typed errors, with serials redacted by default | `core::diagnostics` | `diagnostics_are_exported_without_serial_numbers`, `export_redacts_secrets_from_free_text_events` |
| An export contains no credentials, environment variables, arbitrary home files or network data | `DiagnosticsExport` is built only from owned values | `export_contains_only_declared_fields` |

## Review corrections

Three defects were found while certifying this epic and corrected in place.

1. **The client and the daemon resolved the socket path separately, and the two
   fallbacks disagreed.** With `XDG_RUNTIME_DIR` unset the daemon bound
   `$HOME/.cache/nzxt-control/nzxt-control.sock` while the window connected to
   `/tmp/nzxt-control.sock`, so the window reported "the background service is
   not running" against a daemon that was running. `/tmp` is world-writable:
   another local user could create that socket first and hand the window a
   fabricated device list, ownership state and capability record, which is
   exactly the input every control gate reads. `daemon/src/paths.rs` had already
   rejected `/tmp` for this reason; the client had reintroduced it.

   Both sides now resolve through `core::ipc::socket_path_from_env`, so a
   divergence is no longer expressible. Proven live: with `XDG_RUNTIME_DIR`
   unset, the daemon binds under `$HOME/.cache` and a client following the same
   resolution completes the handshake, while `/tmp/nzxt-control.sock` is never
   created. `the_daemon_binds_the_socket_the_client_connects_to` fails if either
   side grows its own fallback again.

2. **The Active Profile selector stayed enabled when no write was possible.**
   Every other write control passes `LinkState::control_state`, but the profile
   selector was hard-coded to `ControlState::Enabled`, so with no daemon, no
   supported device or a read-only conflict it still rendered as an operable
   tab stop. A profile carries whichever program it was saved with, so it
   cannot be gated on one capability id; `LinkState::write_state` gates it on
   the three conditions US-004 names instead. GPUI has no disabled semantics of
   its own, so `Shell::select` now also withholds the click handler rather than
   only restyling the control.

3. **A failed configuration write left the daemon holding a profile that was
   not on disk.** `save_profile` and `activate` mutated the in-memory document
   before committing, so an I/O failure returned an error while `Profiles` kept
   listing the profile and `ActivateProfile` would still accept it.
   `commit_change` now restores the previous document when the write fails.

Reported and not acted on, because none of them affects a criterion: the
unconstructed `SupportState::Unsupported`, `ButtonVariant::Danger` and
`EventKind::DaemonStopping` variants, the unreferenced `Daemon::diagnostics_mut`
and `Server::daemon`, the unused `Gauge::diameter` / `Gauge::arc_color` builders,
and the duplicate `record_client_rejected` on the uid-mismatch path.

## Deliberate scope decisions

1. **No `lcd-renderer` crate and no `telemetry` module yet.** The PRD sketches
   five crates. Creating two of them empty would add symbols nothing calls.
   They arrive with EP-004 and EP-002 respectively; `README.md` states this.

2. **Activating a fixed or curve profile persists the selection and reports
   `NotApplied`.** No EP-001 story writes to hardware, and the cooling stories
   own the executor. The daemon refuses the activation outright when a required
   capability is not writable, and otherwise reports honestly that the hardware
   was not touched, rather than claiming a write it did not perform.

3. **Every control is disabled on this machine.** No udev rule is installed, so
   `access(W_OK)` fails on every `hwmon` control node. That is the correct
   read-only state, and it is what the screenshots show. Installing the rule is
   US-020's job.

## Network note

`gpui` depends on `gpui_http_client`, which pulls `hyper` and a `reqwest` fork
into the dependency graph. That dependency is not optional and cannot be
removed without forking GPUI, so `cargo tree` will show it. It is a linked
library, not a runtime behavior: no code in this workspace calls it, and the
process opens no listening socket.

Rather than rely on that, `crates/app/src/main.rs` installs
`offline::NoNetwork` as the application's HTTP client. Every request fails with
a named refusal, so a future call site cannot quietly reach the network.
Verified by `every_request_is_refused_with_the_target_named` and by `ss -tulpn`
showing no entry for either binary.

There is no browser engine, WebView or HTTP server anywhere in the graph.
