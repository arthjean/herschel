# EP-002 evidence map

Every acceptance criterion of Monitoring and Thermal Control, with the code that
implements it and the check that proves it. Automated checks are test names
inside `cargo test --workspace`; live checks name the observation.

Machine under test: Fedora 44, Linux `7.1.6-201.fc44.x86_64`, Wayland session,
NZXT Kraken Base `1e71:300e` (bcdDevice 0200) bound to `kraken2023` on
`hwmon4`, AMD Ryzen with `k10temp` on `hwmon5`, NVIDIA GeForce RTX 4070 Ti
SUPER through NVML.

## The kernel ABI this epic writes against

Taken from the kernel's own
[`nzxt-kraken3` documentation](https://docs.kernel.org/hwmon/nzxt-kraken3.html)
rather than inferred from another Kraken generation:

| Attribute | Mode | Meaning |
|---|---|---|
| `temp1_input` | RO | Coolant temperature, millidegrees Celsius |
| `fan1_input` / `fan2_input` | RO | Pump and fan tachometers, RPM |
| `pwm1` / `pwm2` | RW | Pump and fan duty, 0-255 |
| `pwm[1-2]_enable` | RW | `0` runs the channel at 100%, `1` applies `pwmN`, `2` applies the curve |
| `temp[1-2]_auto_point[1-40]_pwm` | **WO** | Curve duties over the fixed 20-59 C range |

Two facts from that table shaped the implementation.

**Mode `0` is the firmware failsafe, not "off".** The kernel documents it as
running the channel at 100%. `PwmMode::FullSpeed` carries that meaning, the
stall detector treats mode 0 as a commanded duty of 255, and the safe program
never writes a mode at all.

**The curve points are write-only.** On the development machine they are
`--w-------`, root-owned. A curve can therefore never be read back off the
device, which has three consequences the design had to absorb:

1. `curve_points_confirmed` is reported as `None` rather than as a mismatch. An
   attribute that supports no readback is unconfirmed, not wrong.
2. The last known-good curve can only come from the daemon's own record of what
   it last committed, which is sound precisely because the daemon is the sole
   writer. `ChannelSnapshot::with_curve` folds that record in.
3. Curve points are inert outside curve mode, so a channel that was on the
   failsafe or on a fixed duty is fully restored by its mode and duty alone.
   `ChannelSnapshot::restores_behavior` encodes exactly that.

Order of writes is deliberate: `pwmN` before `pwmN_enable`, so switching into
direct mode applies the duty the operator asked for rather than whatever the
driver was holding; and all forty curve points before `pwmN_enable = 2`, so the
device never runs a half-updated curve.

## US-006: Stream Kraken telemetry from hwmon

| Criterion | Implementation | Proof |
|---|---|---|
| Liquid temperature, pump RPM, fan RPM and both PWM values sampled once per second | `hardware-linux/src/hwmon.rs` (`KrakenHwmon`), `daemon/src/telemetry.rs` (`Sampler`) | `telemetry_reaches_the_client_with_every_section_sampled`; live figures below |
| A new valid sample reaches the client with an age <=1.5 s at P95 | Sampler stamps each section; `Client::telemetry` polls at the same cadence | `a_sample_reaches_the_client_inside_the_freshness_budget` (production 1 s interval); live P95 **296 ms** below |
| A missing channel is `unavailable` with a typed cause, never zero | `telemetry::Reading`, `sysfs::read_attribute_detailed` | `an_unreadable_channel_is_unavailable_with_its_cause_rather_than_zero`, `an_unreadable_metric_is_unavailable_with_its_cause_not_a_zero` |
| A temporary failure keeps the prior value, marked stale within 2 s and removed after 10 s | `telemetry::Tracked`, `STALE_AFTER_MS`, `DROP_AFTER_MS`, `app/src/metrics.rs` | `a_retained_value_becomes_stale_then_disappears`, `a_read_failure_leaves_the_last_value_visible_then_drops_it` |
| Ten minutes of sampling performs zero writes to `hwmon` | Every read goes through `std::fs::read`; no writer exists in the sampling path | `sampling_performs_zero_writes_to_hwmon`, `sampling_writes_nothing_to_hwmon` (both compare a full mtime and size snapshot); live: `hwmon4` mtimes unchanged across a 315-second run |

## US-007: Stream CPU, GPU and RAM telemetry

| Criterion | Implementation | Proof |
|---|---|---|
| CPU load/temperature, GPU load/temperature and RAM used/total sampled once per second from local interfaces | `hardware-linux/src/sensors.rs` (`/proc/stat`, `/proc/meminfo`, `k10temp`), `hardware-linux/src/gpu.rs` (NVML) | `cpu_load_is_the_busy_fraction_between_two_samples`, `memory_reports_used_and_total_in_bytes`, `cpu_temperature_comes_from_the_recognized_driver_only`; live figures below |
| Percentages clamped to 0-100, temperatures to one decimal, memory in an explicit binary unit | `telemetry::clamp_percent`, `format_temperature`, `format_binary_bytes` | `percentages_are_clamped_and_temperatures_keep_one_decimal`, `memory_is_reported_in_explicit_binary_units` |
| A missing GPU driver or sensor shows `N/A` while other metrics keep updating | Three independent collectors; `GpuTelemetry::unavailable`, `MetricView::qualifier` | `a_missing_library_leaves_every_gpu_value_unavailable_with_a_reason`, `an_unavailable_gpu_leaves_every_other_metric_updating`, `a_section_that_stops_updating_ages_alone` |
| Zero network requests during collection | No HTTP client in the daemon; NVML is a local `dlopen` of `libnvidia-ml.so.1`; the client installs `offline::NoNetwork` | `every_request_is_refused_with_the_target_named`; live: `ss -tulpn` lists no entry for either binary, and the daemon holds exactly one socket descriptor |
| A collector panic or timeout leaves the daemon alive and names the failed collector | `daemon/src/telemetry.rs` `run()` wraps each pass in `catch_unwind` on its own thread; `CollectorFailure` reaches the client and the diagnostics log | `a_failed_collector_is_findable_by_name`, `a_failed_collector_is_reported_by_name`; a wedged collector stops refreshing only its own section, which `a_wedged_section_ages_without_dragging_the_others_with_it` pins |

Three threads rather than one loop is the point. A single loop cannot isolate a
*timeout*: one blocking read would freeze every metric. With one thread per
collector, a collector that stops answering simply stops refreshing its own
section, and the client ages that section out while the others stay current.

## US-008: Render the monitoring dashboard

| Criterion | Implementation | Proof |
|---|---|---|
| CPU, GPU, RAM and Kraken sections with values, units and one dominant bar each | `app/src/shell.rs` `monitoring()`, `components::Metric` | Live run below; `a_metric_without_a_value_reads_as_unavailable_not_as_zero` |
| Charts use an in-memory rolling window and write no history database | `telemetry::History` bounded by `HISTORY_WINDOW_MS`, pruned by timestamp | `history_keeps_only_the_window_and_records_gaps`, `the_default_history_window_is_the_fifteen_minutes_the_prd_allows`, `a_series_records_one_point_per_new_sample` |
| No horizontal scrolling at 920x640 and 1280x720 | `metric_row()` wraps; every column carries `min_w_0`; only the work surface scrolls, vertically | Live run at 920x640 |
| A stale or unavailable value and its chart gap are distinct, and not by color alone | `MetricView::qualifier` renders the words `Stale` and `N/A`; `Sparkline` breaks the line and marks a baseline tick under each hole | `a_metric_takes_its_qualifier_from_the_view_it_was_built_from`, `a_sparkline_breaks_its_line_at_every_gap`, `downsampling_never_hides_a_gap` |
| Tabular numerals keep adjacent labels from moving | `theme::numeric_font` with `tnum`, used by every readout | Inherited from EP-001; every new readout uses `numeric_font()` |

## US-009: Apply validated fixed pump and fan duty

| Criterion | Implementation | Proof |
|---|---|---|
| A duty inside the validated range is written once, with its mode/value readback | `hardware-linux/src/control.rs` `apply_fixed`, `daemon/src/cooling.rs` | `a_fixed_duty_is_written_once_and_reported_with_its_readback`, `a_fixed_duty_writes_two_attributes_and_reads_both_back` |
| A repeated request is deduplicated and performs zero device writes | `CoolingExecutor::already_applied` requires both this process's record *and* the device's own readback to agree | `repeating_a_fixed_duty_performs_no_further_write` (compares a full mtime snapshot), `repeating_the_current_program_performs_zero_writes`, `a_channel_moved_behind_our_back_is_rewritten_rather_than_deduplicated` |
| A duty below the minimum, above the maximum or not a number is refused and the control names the range | `profile::validate_duty`, `Channel::min_duty`, `CoolingEditor::set_duty` clamps, IPC decoding rejects a non-number | `an_out_of_range_duty_is_refused_with_its_range_and_writes_nothing`, `editing_never_leaves_the_pump_below_its_validated_floor`, `wrongly_typed_field_is_rejected` |
| A write timeout or partial error leaves the prior confirmed state visible and marks the hardware uncertain | `CoolingExecutor::abort` restores every channel it touched, then reports `NotApplied` or `Uncertain`; the client keeps showing the readback | `a_partial_failure_restores_the_previous_program_and_reports_it`, `a_failure_the_restoration_cannot_undo_reports_an_uncertain_state` |
| A channel the capability record marks read-only or absent disables its control | `Shell::cooling` gates each row on `Channel::duty_capability`, and Apply on `CoolingProgram::required_capabilities` through `LinkState::program_state` | `each_cooling_channel_is_gated_on_its_own_capability`, `a_program_is_refused_unless_every_channel_it_writes_is_writable`, `each_channel_names_its_own_capability`, `applying_without_write_permission_is_refused_and_touches_nothing`; live refusal below |

The per-channel gate was wrong in the first implementation and is worth naming.
Both rows were gated on `CapabilityId::PumpDuty`, so a udev rule covering only
`pwm1` left the fan's controls enabled on a channel the kernel refuses. The
probe already resolves the two channels independently
(`control_capabilities_follow_filesystem_permissions` builds exactly that
state), and `Channel::duty_capability` existed for this and had no caller
anywhere. Apply has the same shape and is now gated on the same capability list
the daemon checks in `program_incompatibilities`, so an enabled Apply is one the
daemon would accept rather than one that fails on the far side of the socket.

## US-010: Edit and apply a safe liquid-temperature curve

| Criterion | Implementation | Proof |
|---|---|---|
| Ten control nodes over 20-59 C, linearly interpolated to exactly 40 integer PWM values | `profile::CurveNodes` | `ten_nodes_span_exactly_the_kernel_range`, `nodes_interpolate_to_exactly_forty_integer_points`, `interpolation_between_two_nodes_is_linear`, `nodes_sit_on_whole_points_and_whole_degrees` |
| Validation fixes temperature order, keeps PWM in the channel's safe range and duty monotonically non-decreasing | Temperatures are positional and cannot be edited; `CurveNodes::set` maintains monotonicity by construction; `validate_curve` re-checks | `editing_one_node_keeps_the_whole_set_monotonic`, `an_edited_node_set_always_validates_as_a_curve`, `curve_must_not_decrease`, `a_pump_curve_node_cannot_be_edited_below_the_pump_floor` |
| Pointer or keyboard node movement writes nothing until Apply | `CoolingEditor` holds pending state only; `Shell::apply` is the sole sender of `Command::Apply` | `every_edit_keeps_the_program_valid`, `a_curve_edit_needs_both_the_record_and_the_reported_mode`; the editor has no `Feed` handle |
| Apply prevalidates all 40 values, serializes one curve transaction and records readback where supported | `Daemon::execute` validates before the executor is reached; `apply_curve` writes 40 points then the mode | `a_curve_apply_writes_forty_values_per_channel_in_one_transaction`, `a_non_monotonic_curve_is_refused_before_the_first_point_is_written`, `a_curve_of_the_wrong_length_is_refused_before_the_first_write` |
| A failure after one or more attributes changed attempts a complete last known-good restoration and reports confirmed or uncertain | `CoolingControl::restore` writes only what differs; `ChannelSnapshot::restores_behavior` decides which of the two states is reported | `a_snapshot_restores_the_program_the_channel_was_running`, `a_channel_on_the_failsafe_is_restored_by_its_mode_alone`, `a_snapshot_that_read_nothing_cannot_claim_a_restoration` |
| The 100% failsafe at or above 60 C is neither disabled nor overridden | The ABI stops at point 40 (59 C); nothing outside the 40 points and `pwmN`/`pwmN_enable` is ever written | `a_curve_stops_at_the_last_point_the_kernel_abi_defines`, `the_critical_alert_states_that_the_failsafe_is_not_overridden`, `a_coolant_at_the_failsafe_threshold_raises_a_critical_alert` |

`restore` writing only what differs is not an optimization. A blind restoration
would rewrite the very attribute whose write had just failed, turn that second
failure into a reported uncertainty, and claim a channel had moved when it had
not. `a_partial_failure_restores_the_previous_program_and_reports_it` fails if
that behavior comes back.

## US-011: Save, activate and recover named profiles

| Criterion | Implementation | Proof |
|---|---|---|
| A valid configuration saved under a unique 1-48 character name appears in the Active Profile selector | `profile::validate_name`, `config::save_profile`, `Shell::save_profile` | `profile_name_bounds_are_enforced`, `profiles_are_saved_activated_and_deleted_through_the_socket` |
| A restart or resume redetects, revalidates and restores the profile within 5 s of device availability | `Daemon::restore_active_profile` runs at the end of `start`, after the probe and the capability resolution | `the_active_profile_survives_a_daemon_restart` now asserts the hardware, not only the selection: `pwm1` reads 120, `pwm2` reads 80, `pwm1_enable` reads 1 |
| A profile for another VID/PID, firmware or capability set is refused with the incompatibilities listed | `profile::incompatibilities`, `program_incompatibilities`, `Daemon::targets_for` | `a_profile_bound_to_an_absent_device_is_refused`, `a_profile_needing_an_unwritable_capability_is_refused`, `incompatible_profile_lists_every_missing_capability` |
| Deleting the active profile activates the built-in safe profile first | `Shell::delete_button` sends `Command::DeleteProfile`; `config::delete_profile` commits the safe selection before removing the entry | `deleting_the_active_profile_activates_the_safe_one_first`, `profiles_are_saved_activated_and_deleted_through_the_socket`, `deleting_a_profile_takes_two_deliberate_activations`, `the_built_in_safe_profile_can_never_be_deleted` |
| Corruption selects the safe profile and keeps the corrupt file exportable | `config::preserve`, `ConfigState::Recovered` | `a_corrupt_configuration_recovers_to_the_safe_profile`, `a_truncated_file_is_preserved_and_safe_defaults_take_over` |

Suspend and resume specifically belong to US-019; this story covers the restart
path, which is the one a fresh daemon exercises.

Deletion had the same shape of defect as the per-channel gate, in the other
direction: the daemon implemented it, the socket test exercised it, and no
control in the window ever sent `Command::DeleteProfile`. A behavior reachable
only from a test is not reachable. The Cooling screen now carries a Delete
control that arms on the first activation and fires on the second, because
removing the configuration an operator is running is not something one stray
click should accomplish, and the built-in safe profile is refused there for the
same reason `config::delete_profile` refuses it.

## US-012: Complete the Cooling control surface and safety states

| Criterion | Implementation | Proof |
|---|---|---|
| Pump and fan rows show RPM, PWM, active mode, temperature source and the profile selector above the curve | `Shell::channel_row`, `Shell::channel_detail`, `Shell::cooling` | Live run below |
| A pending selection is visually distinct from confirmed hardware state until Apply succeeds | `CoolingEditor::pending` compares the edit against the readback for a fixed duty, and against both this client's confirmed record and the reported mode for a curve | `a_fixed_edit_stays_pending_until_the_readback_agrees`, `a_curve_edit_needs_both_the_record_and_the_reported_mode`, `the_onboard_program_is_never_pending` |
| Liquid >=60 C, or zero RPM for three consecutive samples while a duty is commanded, raises a critical state within 2 s naming the channel and readback | `telemetry::AlertTracker`, sampled at 1 Hz so three samples land in three seconds and the alert reaches the client on the next poll | `a_single_zero_rpm_sample_is_not_yet_a_stall`, `the_failsafe_mode_counts_as_a_full_duty_command`, `an_unreadable_sample_neither_raises_nor_clears_a_stall`, `a_stalled_channel_raises_an_alert_after_three_samples`, `a_coolant_at_the_failsafe_threshold_raises_a_critical_alert` |
| A read-only conflict, lost permission, unplug or stale telemetry disables every write control within 2 s, keeping the diagnostic context | `LinkState::cooling_state` adds staleness and device presence to the capability gate; `STALE_AFTER_MS` is 2000 | `stale_cooling_telemetry_disables_write_controls_and_says_how_old_it_is`, `an_absent_kraken_disables_cooling_controls_even_when_the_capability_is_writable`, `cooling_controls_stay_disabled_until_the_first_sample_arrives`, `a_read_only_conflict_disables_controls_and_shows_the_conflict` |
| Every edit, Apply and Cancel is possible without a pointer | Every control is a tab stop, and GPUI activates `on_click` listeners on Enter and Space for the focused element; the curve plot additionally handles the arrow keys | `rail_tab_order_is_stable_and_precedes_screen_controls`, `every_cooling_control_keeps_traversal_order_equal_to_visual_order`; live keyboard walkthrough below. Each channel row reserves a block of `COOLING_ROW_STRIDE` stops covering the row header and every control its open detail can render, so traversal stays in visual order whichever row is open |

The stall detector deliberately ignores an unreadable sample rather than
treating it as evidence either way: a dropped tachometer reading is not proof
that the pump stopped, and it is not proof that it is turning.

## Live measurements

### Telemetry, through the real socket

Read with a plain newline-JSON client against
`/run/user/1000/nzxt-control/nzxt-control.sock`:

| Section | Reading |
|---|---|
| Liquid temperature | 28.5 C |
| Pump | 2964 RPM, duty 255, mode `full_speed` |
| Fan | 1785 RPM, duty 255, mode `full_speed` |
| CPU | 32.8% load, 67.6 C (`k10temp` Tctl) |
| GPU | NVIDIA GeForce RTX 4070 Ti SUPER, 36% load, 38 C, source `NVML` |
| Memory | 11.9 GB of 32.7 GB in use |
| Alerts / failed collectors | none / none |

Every figure matches the raw sysfs values read independently
(`temp1_input` 28500, `fan1_input` 2964, `fan2_input` 1785, `pwm1` 255,
`pwm1_enable` 0).

The device is in mode `0` on both channels, which is the firmware failsafe at
100%. That is the state this machine boots into, and nothing in this epic
changed it.

### Sample age at the client

Twelve consecutive polls at the production one-second cadence, measuring the age
of the reading itself rather than the response:

| Metric | Value | Budget |
|---|---|---|
| Minimum | 287 ms | |
| Median | 292 ms | |
| **P95** | **296 ms** | <=1500 ms |
| Maximum | 303 ms | |

### Zero writes

`hwmon4` modification times and sizes for `pwm1`, `pwm1_enable`, `pwm2`,
`pwm2_enable`, `temp1_input` and `fan1_input`, captured before and after a
315-second run with the sampler at 1 Hz: unchanged. `pwm1` and `pwm2` still read
255, both `_enable` still read 0.

### Typed refusals on the live machine

No udev rule is installed on this machine, so every control attribute is
root-owned and the daemon is correctly in read-only mode.

An Apply of a *valid* program is refused after the capability check, naming the
exact path:

```
"error": "incompatible",
"details": [
  { "capability": "pump_duty",
    "reason": "Read-only: no write permission on .../hwmon4/pwm1." },
  { "capability": "fan_duty",
    "reason": "Read-only: no write permission on .../hwmon4/pwm2." }
]
```

An Apply of an *invalid* program is refused before that, on the value itself:

```
"error": "validation", "kind": "duty_out_of_range",
"channel": "pump", "value": 3, "min": 51, "max": 255
```

The order matters and is deliberate: an out-of-range duty is wrong whatever the
device reports, so it is rejected before ownership is even consulted.

### Network

`ss -tulpn` lists no entry for either binary. The daemon holds exactly one
socket descriptor, the Unix listener. NVML is loaded with `dlopen` from
`libnvidia-ml.so.1`, a local file the driver package installs.

### The two screens, on this machine

Captured from the running release build at 920x640 under X11, against the real
devices:

- [`docs/screenshots/ep-002-monitoring.png`](./screenshots/ep-002-monitoring.png):
  the read-only banner, both allowlisted devices with their firmware and kernel
  binding, and the CPU section with its load and temperature bars and its live
  chart.
- [`docs/screenshots/ep-002-monitoring-gpu-memory-kraken.png`](./screenshots/ep-002-monitoring-gpu-memory-kraken.png):
  GPU 54% and 36.0 C, memory `9.8 GiB of 30.5 GiB in use` at 32%, and Kraken
  liquid 28.6 C with both tachometers, each with its rolling chart and the note
  that the window is held in memory only.
- [`docs/screenshots/ep-002-cooling-read-only.png`](./screenshots/ep-002-cooling-read-only.png):
  both channel rows with RPM, PWM, mode `100% failsafe` and the liquid
  temperature source; the readback age; the disabled fixed-duty controls with
  their accepted ranges; and the Program panel reporting that the selection
  matches what the hardware reports.

The Cooling captures above show the layout as it was measured: one panel per
concern, with every control expanded at once. The screen has since been rebuilt
around one line per channel that opens its own controls, and the two selects
moved under the heading. What each control is gated on did not change, so the
capability evidence in this table still holds; the pictures are the previous
arrangement of it and a capture of the current one is still owed.

Two layout defects were found in those captures and fixed rather than accepted.
The fan's duty readout wrapped mid-value (`128/255 (50%` then `)`), because a
96-pixel minimum was narrower than the longest value the field can hold; it is
now 140 pixels and `flex_none`. And every metric tile sized itself to its label,
which left the section's dominant bar shorter than the word above it; tiles now
share their row so the bar spans the width it is given.

Every destination was exercised by keyboard under X11 (`ctrl-1` through
`ctrl-4`, `ctrl-,`; the panel has since moved onto Lighting, so `ctrl-4` is
gone and the rail now ends at `ctrl-3` plus Settings), then the Cooling screen
was walked with 22 `Tab` presses
followed by `Right`, `Up`, `Down`, `Left`, `Return` and `Space`, and finally
edited with the pointer on the curve plot. The process survived all of it; GPUI
activates `on_click` listeners on Enter and Space for the focused element, which
is what makes every button and select operable without a pointer.

### Resource budget, and the regression this epic caused and fixed

Release build, Monitoring open and untouched, sampled from `/proc/<pid>/status`
over 300 seconds with the daemon running and the dashboard live at 1 Hz:

| Metric | EP-001 (static shell) | EP-002 first measurement | EP-002 after the fix | Budget |
|---|---|---|---|---|
| Idle CPU, 5-minute average | 1.10% | **1.60%** | **1.10%** | <=1.5% Month 1, <=1.2% Month 6 |
| `RssAnon` | 81.3 MiB | 84.6 MiB | **83.4 MiB** | <=110 MiB Month 1, <=100 MiB Month 6 |
| Total `VmRSS` | 253.2 MiB | 261.3 MiB | **259.6 MiB** | <=320 MiB ceiling |
| `RssFile` | 172.0 MiB | 176.7 MiB | 176.2 MiB | driver-dependent |
| Threads | 30 | 31 | 31 | |

The first measurement failed the Month-1 CPU budget, and the failure was real
rather than noise. A shorter 60-second window measured 1.10% while the full
300-second window measured 1.60%: the cost was growing with the history. Each
of the four charts was building a path with one vertex per sample, once per
second, and a fifteen-minute window holds nine hundred samples for a chart a few
hundred pixels wide.

Bounding the plot to `MAX_PLOTTED_POINTS` returns the figure to 1.10%, which is
EP-001's baseline: a live dashboard at one repaint per second now costs the same
as the static shell did. `RssAnon` rises by 2.1 MiB over EP-001, which is the
four rolling windows plus the client's tracked metrics, and stays inside the
Month-6 target as well as the Month-1 one.

Cold start, five launches with `NZXT_STARTUP_TRACE=1` on Wayland: 374.8, 289.4,
326.8, 299.2, 344.0 ms. Median **326.8 ms** against a 700 ms budget. Startup is
unchanged from EP-001 because the daemon connection moved off the launch path:
the worker thread connects while the window is already opening.

The extra thread is the client's worker. It exists so a stalled daemon ages a
reading out instead of freezing the window for the length of the client timeout.

## Deliberate decisions

1. **NVML rather than sysfs for the GPU.** The discrete GPU on this machine is
   driven by the NVIDIA proprietary stack, which publishes no `hwmon` instance
   and no `gpu_busy_percent`: `/sys/class/drm/card2` carries neither load nor
   temperature. The integrated AMD GPU does publish both, but reporting an idle
   integrated GPU as "the GPU" while the RTX 4070 Ti SUPER drives the display
   would be worse than reporting nothing. `nvml-wrapper` loads the library at
   runtime, so a machine without the driver still builds and simply reports the
   GPU as unavailable. It is MIT OR Apache-2.0, compatible with the project's
   GPL-3.0-or-later, as are its five transitive additions.

2. **A fixed list of CPU temperature drivers.** `CPU_TEMPERATURE_DRIVERS`
   contains exactly `k10temp`, which is what the certified platform exposes.
   Another machine resolves to no driver and reports the CPU temperature as
   unavailable rather than borrowing a reading from an unrelated sensor.

3. **Polling rather than a push subscription.** The daemon samples at 1 Hz and
   the client polls at 1 Hz, which puts the worst-case age at one interval plus
   a sub-millisecond round trip. The measured P95 is 296 ms against a 1500 ms
   budget, so a second framing mode would have bought nothing.

4. **Nodes on whole ABI points.** The ten editor nodes sit on point indices
   0, 4, 9, 13, 17, 22, 26, 30, 35 and 39, which are whole degrees: 20, 24, 29,
   33, 37, 42, 46, 50, 55 and 59 C. That makes `CurveNodes::from_curve` the
   exact inverse of `interpolate`. An earlier version spaced the nodes evenly in
   real numbers and drifted by one PWM step per load-and-save cycle;
   `nodes_round_trip_through_a_stored_curve` fails if that returns.

5. **Charts plot at most 180 points.** A fifteen-minute window holds nine
   hundred samples and the chart is a few hundred pixels wide. Plotting every
   sample cost measurable idle CPU for nothing visible: see the budget section.
   Downsampling marks a bucket as a gap when *any* sample in it is missing,
   which overstates a hole rather than hiding one.

6. **Activating a profile persists the selection before it writes.** A write
   that fails leaves the profile selected and its hardware state reported
   honestly, rather than silently reverting a choice the operator made. The
   refusals that can be judged without touching the device, an invalid value or
   an unwritable capability, still happen before anything is persisted.

### Re-observation after the review fixes

The corrected client was rebuilt and run against the live daemon and both real
devices: first frame at **411.9 ms** on Wayland, both allowlisted devices listed
with their firmware and kernel binding, the read-only banner correct for this
machine, and CPU load and temperature refreshing at 1 Hz. No panic, and the
whole suite passes after the last change.

The UI gate then had to be re-run on the Cooling screen, and this session runs
GNOME on Wayland, where `xdotool` reaches no window even through XWayland. It
was re-run inside a dedicated **Xephyr** server instead, where XTEST works, at
`920x640`, `1280x720` and at 192 dpi for the 200% case. Two states were needed,
because this machine has no udev rule and can only show the refused half of
every criterion:

- **Read-only**, against the real daemon and the real devices: both channel rows
  carry RPM, PWM, mode `100% failsafe`, the liquid temperature source and the
  readback age; both fixed-duty controls are disabled and state their accepted
  ranges, `51-255` for the pump and `0-255` for the fan; and the four Program
  actions sit on one row without horizontal scroll at both window sizes.
- **Writable**, against a fake sysfs tree mirroring `testing.rs` with the
  control nodes granted, which is the state an installed udev rule produces. It
  is the only way to exercise the enabled half before US-020 installs that rule,
  and it drives the same code the real device does.

What the writable run proved, entering through the window and nothing else:

| Step | Observation |
|---|---|
| Save as profile | `Profile Onboard (device default) 1 saved.` and the profile lands in `config.toml` |
| Activate it | The selector switches, `active_profile` is persisted, and the note reports that nothing was written because the program is onboard |
| Delete, first activation | The label becomes `Confirm deletion`; the file is unchanged and the profile still active |
| Delete, second activation | `Onboard safe activated before Onboard (device default) 1 was deleted.` and the file holds `active_profile = "Onboard safe"`, `profiles = []` |
| Keyboard only | 15 tabs from the shell land the focus ring on Delete, five rail entries then ten screen controls in visual order; `Space` arms it and a second `Space` deletes, with no pointer involved |
| 200% scale | The window is 1840x1280 physical for 920x640 logical, and no label, readout or selector is clipped |

That last row is what the per-channel gate needed too: with the fan writable the
control is enabled, and with it read-only the control is refused and names
`pwm2`, which is the criterion US-009 states and which no state of this machine
alone can show.

- [`ep-002-cooling-200-scale.png`](./screenshots/ep-002-cooling-200-scale.png)
- [`ep-002-cooling-delete-armed.png`](./screenshots/ep-002-cooling-delete-armed.png)
- [`ep-002-cooling-delete-confirmed.png`](./screenshots/ep-002-cooling-delete-confirmed.png)

## Outstanding

**No write has yet reached the physical Kraken.** This machine has no udev rule
installed, so every control attribute is root-owned and the daemon is correctly
in read-only mode. Installing that rule is US-020's story. The write path is
proven against a fixture that mirrors the driver faithfully, including the
write-only curve points, the real permission model and the readback semantics,
and the live machine produces the correct typed refusals. A live write requires
temporarily granting the user ownership of the four control nodes and the eighty
curve points, which needs `sudo` and is recorded here as pending rather than
claimed.
