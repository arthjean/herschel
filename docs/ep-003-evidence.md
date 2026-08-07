# EP-003 evidence map

Every acceptance criterion of Validated RGB Controller, with the code that
implements it and the check that proves it. Automated checks are test names
inside `cargo test --workspace`; live checks name the observation.

Machine under test: Fedora 44, Linux `7.1.6-201.fc44.x86_64`, NZXT RGB
Controller `1e71:2021` (bcdDevice `0105`) on `/sys/bus/usb/devices/1-12`, HID
interface 0 bound to `usbhid`, node `/dev/hidraw12`.

## What the controller turned out to be

Recorded from the device itself on 2026-08-06, not from a product page:

| Fact | Value | Source |
|---|---|---|
| Firmware | `1.5.0` | report `0x11 0x01`, bytes 17-19 |
| Channels | 3 | report `0x21 0x03`, byte 14 |
| Accessories | one `0x17` F140 RGB Core per channel | report `0x21 0x03`, from offset 15 |
| Channel 1 | top fan | write probe, operator observation |
| Channel 2 | middle fan | write probe, operator observation |
| Channel 3 | bottom fan | write probe, operator observation |
| LED count | **unknown** | the controller reports accessory identifiers and never a count |

`rgb::VALIDATED_FIRMWARE` therefore contains exactly `1.5.0`, and nothing else
can be written by this build.

Access needs one narrow udev rule, numbered below 73 so `73-seat-late.rules`
still consumes the `uaccess` tag. Without it every `/dev/hidraw*` is
`crw------- root root`, the daemon refuses to escalate, and Lighting stays
read-only naming the missing permission. That refusal path was captured before
the rule was installed and is kept below as evidence of US-013 AC-4.

```
# /etc/udev/rules.d/70-nzxt-control.rules
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1e71", ATTRS{idProduct}=="2021", TAG+="uaccess"
```

## The protocol this epic writes against

The report identifiers, the query sequence and the color packet layout are
adapted from liquidctl's `Nzxt2023RgbController`
([`liquidctl/driver/smart_device.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/smart_device.py))
and its `Hue2Accessory` table
([`liquidctl/util.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/util.py)).
Both carry `SPDX-License-Identifier: GPL-3.0-or-later`, so adapting them into
this project is license-compatible. OpenRGB was deliberately **not** used as a
source: it is GPL-2.0-only, which cannot be combined into a GPL-3.0-or-later
work.

Adapted from a working implementation is not evidence that a command is safe on
*this* firmware, which is exactly why the firmware gate exists.

The identifiers were cross-checked against the HID report descriptor this
controller publishes (401 bytes, world-readable at
`/sys/bus/hid/devices/0003:1E71:2021.000D/report_descriptor`). Every identifier
the product sends appears there as an output or feature report with a 63-byte
payload, which plus the identifier byte is the 64-byte report the interrupt OUT
endpoint carries:

| Report | Direction | Meaning |
|---|---|---|
| `0x10 0x01` | OUT | Firmware request |
| `0x11 0x01` | IN | Firmware answer, revision at offsets 17-19 |
| `0x20 0x03` | OUT | Lighting topology request |
| `0x21 0x03` | IN | Topology answer: channel count at 14, six accessory slots per channel from 15 |
| `0x2a 0x04` | OUT | Color command |

Endpoints, read from sysfs and recorded in `docs/capability-record.json`:
`0x02` interrupt OUT, 64 bytes, `bInterval` 1; `0x81` interrupt IN, 64 bytes.

Three facts shaped the implementation.

**The controller exposes no way to read a channel back.** It does emit
unsolicited status reports on `0xff` while commands are in flight, and the write
probe captured them verbatim, but none of them carries a channel's current
color. There is no "get current color" report. The daemon's record of what it
committed is therefore the only evidence of what a channel is showing, which is
sound precisely because the daemon is the sole writer. This is the same argument
the write-only curve attributes already forced in EP-002. `HardwareState::Confirmed`
on a lighting outcome means the controller accepted the report and claims
nothing stronger, and the client renders it in those words.

**There is no brightness field.** The fixed-color packet carries one GRB triplet
and nothing else. `Rgb::scaled` applies brightness to the triplet before
encoding, which is the only place it can be applied without inventing a field
the protocol does not have. A mode carrying no color therefore shows no
brightness control, because there would be nothing for it to dim.

**Off is the fixed mode carrying one black step, not an empty command.** A color
count of zero leaves the previous animation running. `off_sends_one_black_step_rather_than_no_step`
pins that byte.

## US-013: Validate the RGB Controller protocol and topology

| Criterion | Implementation | Proof |
|---|---|---|
| Read-only probe records firmware, interfaces, endpoints, channel count and readable LED metadata | `hardware-linux/src/usb.rs` (`endpoints`, `hidraw_node`), `hardware-linux/src/rgb.rs` (`inspect`, `connect`), `daemon/src/main.rs` (`--rgb-probe`) | `endpoints_are_read_in_address_order_with_the_kernels_own_decoding`, `the_controller_resolves_to_the_node_the_kernel_created`, `the_topology_answer_lists_accessories_per_channel`, `a_controller_that_answers_everything_is_recorded_in_full`, `a_controller_that_answers_only_its_firmware_keeps_that_evidence`; live: `docs/capability-record.json` carries firmware `1.5.0`, three channels and one `0x17` accessory each |
| With operator confirmation and a captured prior state, only an allowlisted low-brightness fixed color and off are tested, one channel at a time | `daemon/src/rgb_probe.rs` (`ProbeScope::FixedAndOff`, `CONFIRMATION_PHRASE`, `run`) | `the_us_013_scope_sends_a_dim_fixed_color_and_off_and_nothing_else` (asserts the mode byte of every report is `0x00` and the channel order is 1,1,2,2), `nothing_is_sent_without_the_typed_confirmation`, `an_unknown_topology_refuses_before_the_confirmation_is_even_asked`; live run: [`docs/ep-003-write-probe-us013.json`](./ep-003-write-probe-us013.json), six steps, zero errors |
| Packet bytes, response, channel, observed result and maximum stable cadence recorded, without publishing a serial | `rgb_probe::ProbeStep`, `CadenceObservation`, `measure_cadence` | `an_authorized_probe_records_every_report_it_sent` (full 64-byte frames), `the_cadence_measurement_reports_the_fastest_interval_that_held`; live: both records carry every 64-byte frame, the `0xff` status reports that came back, per-write timing and the operator's words, and no serial number appears in either |
| Unknown channel count, LED count or firmware keeps Lighting read-only and reports the missing evidence | `hardware-linux/src/probe.rs` (`rgb_state`), `app/src/link.rs` (`control_state`), `app/src/shell.rs` (`lighting`) | `an_unanswered_controller_leaves_rgb_unvalidated_and_names_the_reason`, `an_unvalidated_firmware_is_named_and_stays_read_only`, `a_controller_reporting_no_channel_is_never_writable`, `a_capability_with_its_own_reason_is_not_shadowed_by_a_machine_wide_conflict`, `an_unvalidated_firmware_refuses_every_write_and_names_the_missing_evidence`; live capture below |
| A failed probe or a disconnect attempts to restore the captured prior state and reports whether restoration was confirmed | `rgb_probe::restore`, `Restoration` | `an_unobserved_restoration_is_never_reported_as_confirmed`, `a_confirmed_restoration_carries_what_the_operator_saw`, `a_controller_that_refuses_every_write_is_reported_rather_than_retried`; live: both runs end `"restoration": {"state": "confirmed"}` with the operator's description |

### What "captured prior state" can mean here

The controller answers no report that reads a channel back, so no instrument in
this product can capture what the lighting was showing before the first write.
The probe therefore asks the operator and records the answer verbatim, and asks
again after restoring. An observation nobody made stays empty and the
restoration is reported as `Unconfirmed` with that reason: `an_unobserved_restoration_is_never_reported_as_confirmed`
pins it. That is the honest reading of the criterion, not a workaround.

Both live runs show what that costs. The first captured `trois ventilateurs en
violet fixe` and ended with `les trois ventilateurs sont eteints comme demande,
le violet d'origine n'est pas revenu`. The restoration did what it was asked to
do and did not reproduce the prior state, because nothing in this protocol can.
The record says both, which is the point.

### Live results

```
firmware 1.5.0, 3 channels, one F140 RGB Core each
scope fixed_and_off   6 steps, 0 errors, writes 184-1075 us
scope with_effects   15 steps, 0 errors
cadence  5 commands landed at every interval down to 10 ms, no failure
```

The cadence result is the one that feeds a constant.
`MIN_COMMAND_INTERVAL_MS` stays at 50 ms, five times slower than the fastest
interval that held. The floor exists because the controller acknowledges
nothing, so it is set from what is safe rather than from what is achievable.

Channel bits map one to one onto physical fans, confirmed by watching them:
`0x01` top, `0x02` middle, `0x04` bottom. At every step the operator recorded
that the other two channels were unchanged, which is what rules out a mask
touching more than one fan.

Packet bytes, identical in both runs:

| Program | mode | speed | first triplet | trailer |
|---|---|---|---|---|
| fixed `#FFFFFF` at 10% | `00` | `3200` | `191919` | `00 01 00 08 03` |
| off | `00` | `0000` | `000000` | `00 01 00 08 03` |
| Breathing, normal | `07` | `1400` | `191919` | `00 01 08 08 03` |
| Spectrum wave, normal, forward | `02` | `fa00` | `000000` | `00 01 00 08 03` |

`19` is 25, exactly 10% of 255, so brightness reaches the wire through the
triplet as designed. `0x14` and `0xfa` are the third entry of each effect's
speed table, which is `normal`.

## US-014: Control fixed RGB color and off state

| Criterion | Implementation | Proof |
|---|---|---|
| A validated channel shows its name, LED count when known, confirmed mode, brightness and color | `app/src/shell.rs` (`lighting`, `channel_row_lighting`, `channel_headline`, `accessory_summary`), `core/src/ipc.rs` (`ChannelState`) | `a_channel_names_what_the_controller_detected_and_never_an_led_count`, `a_controller_that_answered_is_reported_with_its_channels_and_accessories`, `the_reported_state_carries_the_accessories_the_controller_named` |
| A valid six-digit hex and 0-100% brightness update the preview immediately, then one rate-limited command follows | `app/src/lighting.rs` (`ChannelEditor::program`), `daemon/src/lighting.rs` (`LightingExecutor::apply`) | `a_command_sends_exactly_one_report_and_becomes_the_committed_state`, `brightness_dims_the_triplet_that_reaches_the_controller`, `a_fixed_color_is_encoded_green_red_blue_at_full_brightness` |
| Off emits zero light and the prior fixed color stays available | `core/src/lighting.rs` (`LightingProgram::Off`), `app/src/lighting.rs` | `off_sends_one_black_step_rather_than_no_step`, `switching_to_off_and_back_keeps_the_color_the_operator_chose` |
| Invalid hex, an unsupported channel or a cadence above the limit produce no write and identify the field | `core/src/lighting.rs` (`validate_command`), `daemon/src/lighting.rs` (cadence), `daemon/src/state.rs` (`illuminate`) | `an_invalid_color_names_the_exact_problem`, `a_channel_outside_the_reported_topology_is_refused_before_any_write`, `a_command_faster_than_the_floor_is_refused_before_the_write`, `an_out_of_range_brightness_cannot_even_be_decoded`, `a_fixed_color_becomes_a_program_and_an_invalid_one_does_not` |
| A daemon start restores the active profile's channels; a physical unplug is US-019's | `daemon/src/state.rs` (`apply_profile_lighting`, called from `restore_active_profile` and from `activate_profile`) | `a_saved_effect_round_trips_without_protocol_bytes_reaching_the_file` starts a second daemon over the same configuration directory and reads the same channel parameters back. Restoration runs before the socket is bound, so no client can observe the gap. The physical-unplug half moved to US-019 in PRD 1.3; what this epic leaves it is recorded below |

## US-015: Add only validated RGB effects

| Criterion | Implementation | Proof |
|---|---|---|
| Breathing and Spectrum Wave are the only additional candidates | `core/src/lighting.rs` (`LightingEffect::ALL`) | `only_the_two_validated_effects_exist` |
| Only values the capability record validates can be selected | `core/src/lighting.rs` (`color_range`, `accepts_direction`), `hardware-linux/src/rgb.rs` (`speed_values`) | Partial. The *shape* is pinned: `the_speed_step_moves_with_the_selected_speed` (the five firmware steps, asserted strictly decreasing), `a_direction_the_effect_ignores_is_refused_rather_than_silently_dropped`, `an_effect_refuses_a_color_count_it_does_not_accept`. `the_sweep_covers_every_value_the_screen_can_select` builds its expectation from the same `ALL` arrays the screen builds its selects from, so a value that becomes selectable without being swept fails the test. Live: [`docs/ep-003-write-probe-us015-sweep.json`](./ep-003-write-probe-us015-sweep.json), the fifteen selectable combinations plus off on channel 1, zero errors, every step observed. See "Every selectable parameter, on the hardware" |
| An unproven effect or parameter is absent, not disabled or emulated | `app/src/lighting.rs` (`LightingMode::all`, `uses_*`), `app/src/shell.rs` (`when(...)` guards) | `effects_are_absent_until_they_are_available`, `each_mode_exposes_only_the_controls_it_uses` |
| A failed effect write keeps the last confirmed mode and discards the pending one | `daemon/src/lighting.rs` (uncertain path clears the committed record) | `a_failed_write_reports_uncertain_and_forgets_the_channel` |
| A saved effect round-trips without raw protocol data in the configuration file | `core/src/profile.rs` (`Profile::lighting`), `daemon/src/config.rs` | `a_saved_effect_round_trips_without_protocol_bytes_reaching_the_file` (reads the file back and asserts no packet byte appears), `a_program_round_trips_without_carrying_protocol_bytes` |

## Every selectable parameter, on the hardware

`--rgb-write-probe --sweep-effects`, 2026-08-06, firmware `1.5.0`, channel 1
(top fan). Sixteen programs, zero write errors, every step observed, cadence
stable to 10 ms again, restoration confirmed:
[`docs/ep-003-write-probe-us015-sweep.json`](./ep-003-write-probe-us015-sweep.json).

The bytes that left the process match `speed_values` exactly, and the trailer
direction flag appears on the five backward steps and nowhere else:

| Program | mode | speed | direction flag |
|---|---|---|---|
| Breathing, slowest to fastest | `07` | `2800` `1e00` `1400` `0a00` `0400` | `00` |
| Spectrum wave forward, slowest to fastest | `02` | `5e01` `2c01` `fa00` `9600` `5000` | `00` |
| Spectrum wave backward, slowest to fastest | `02` | `5e01` `2c01` `fa00` `9600` `5000` | `02` |
| off | `00` | `0000` | `00` |

What the operator saw, which is the half no byte can prove. Both effects sped
up monotonically across their five steps, so the table's ordering is a fact
about this firmware rather than an assumption carried over from liquidctl. The
reversal was described as counter-clockwise from the second backward step
onward, and the first was recorded as *"m'a l'air plus lent"* without a
direction: the sense reverses, and the record keeps the hesitation rather than
smoothing it. At all sixteen steps channels 2 and 3 stayed on their prior fixed
violet, which is what rules out a mask reaching past the addressed channel.

The sweep stays on the first channel by design. Speed and direction are
firmware behavior, not channel behavior, and the channel bits were already
mapped one to one onto physical fans by the US-013 run, so sweeping all three
would triple an operator's watching time to re-observe a recorded fact.
Restoration still covers every reported channel.

This run left the three fans off rather than on their prior violet, because it
was started without `--restore`. The record says so, as the US-013 run did.

## Boundaries this epic did not cross

- **No kernel driver is detached.** The controller stays bound to `usbhid` and
  every transfer goes through the node the kernel already created, which is the
  same rule the thermal path follows for `kraken2023`.
- **Nothing runs as root.** `--rgb-write-probe` refuses to start as root and
  points at the udev rule instead.
- **One writer.** `--rgb-write-probe` takes the same per-device lock the daemon
  takes, so the probe and the service can never address the controller at once.
- **The command floor is enforced in the daemon**, not at the screen:
  `MIN_COMMAND_INTERVAL_MS` is 50 ms per channel, tracked per channel
  (`cadence_is_tracked_per_channel_rather_than_globally`). The probe measured
  10 ms as stable, so the floor keeps a factor of five.

## Interface

Captured from the running release build at 920x640 under X11, against the real
machine and the real controller.

- [`ep-003-lighting-missing-evidence.png`](./screenshots/ep-003-lighting-missing-evidence.png):
  Lighting before the udev rule existed. No fabricated channel, no disabled
  color picker, no placeholder effect list. It states the node, the permission,
  the remediation and the story that would produce the evidence: *"The channel
  topology is not readable. permission denied on /dev/hidraw12. Check the
  installed udev rule. Requires US-013."*
- [`ep-003-lighting-live.png`](./screenshots/ep-003-lighting-live.png): the same
  screen against the answering controller. Channel selector naming the detected
  F140 RGB Core, the LED count stated as not reported, the confirmed program,
  and the write controls enabled. Apply was activated through the interface and
  the confirmed row moved to *fixed #6F4EF2 at 60%*.
- [`ep-003-lighting-deduplicated.png`](./screenshots/ep-003-lighting-deduplicated.png):
  Apply activated a second time on the same program. *"Channel 1 already shows
  this. Nothing was sent."* The deduplication US-014 requires, observed from the
  interface rather than from a unit test.

Three defects were found through these captures and fixed rather than accepted.

**A cooling fact explained a lighting refusal.** The machine-wide read-only
conflict the daemon synthesises is about `hwmon`, and it shadowed the RGB
capability's own reason. `LinkState::control_state` now prefers the capability's
own reason and falls through to the ownership conflict only when the record is
clean; ownership still outranks a clean record.
`a_capability_with_its_own_reason_is_not_shadowed_by_a_machine_wide_conflict`
and `ownership_still_outranks_a_capability_record_that_looks_fine` pin both
directions.

**A brightness control that could not act.** The first Lighting screen rendered
a `Slider`, which at the time painted a value and received no input, so an
enabled-looking control did nothing. It became the same stepper the Cooling
screen uses for fixed duty, and stepping it from 100% to 60% and applying is
what produced the live capture above. `Slider` has since been given the input it
lacked, and the first attempt at that was wrong in a way worth recording.

The track publishes the rectangle it was painted at, a press captures that
rectangle, and every later move is converted against it
(`a_pointer_on_the_track_selects_the_value_that_position_marks`). What did not
work was hanging the press on the control itself: every GPUI mouse listener is
gated on `hitbox.is_hovered`, and the slider's own interactive surface sits
between the press and a listener on the field around it, so the handler never
ran and the drag did nothing at all. The press is decided in the window's
capture handler instead, from a map of where each operable track was painted
(`a_press_finds_the_track_it_landed_on_and_no_other`,
`a_row_that_went_away_takes_its_track_with_it`) That handler is the one this
shell already relied on for dropping the focus ring, so it is the one path
whose delivery was not in question.

Measured end to end in a nested X server, where synthetic input actually
reaches the process: a press at the middle of channel 1's track reported
`Some(Channel(1))` and set 50%, a drag to the left and back set 20%, 2% and
75%, including a move that left the track vertically; releasing stopped it, and
further moves changed nothing. From the keyboard, Left stepped 95% then 90% and
Home reached 0%. The row carries that slider now; the stepper is gone.

**A query window set below the measured latency.** See the section below.

## The measurement that changed a constant

The first live `--rgb-probe` reported a silent controller. It was not silent:
the firmware answer lands in 2 ms and the topology answer takes 518 to 699 ms
over five runs, and `ANSWER_TIMEOUT` was 500 ms. Enumerating three channels is
simply slow on this firmware.

Two fixes, not one. The window moved to 2 s, well above the slowest observation
rather than at it (`the_answer_window_clears_the_measured_topology_latency`).
And `query` stopped discarding evidence: it had reported both fields unknown
when only one was late, so a firmware that had actually been read was recorded
as unread. `RgbInventory` now carries each field separately and `inspect` maps
each to its own `Evidenced`
(`a_controller_that_answers_only_its_firmware_keeps_that_evidence`).

## Incidental finding, for EP-004

`--rgb-probe` also recorded the Kraken's interface 0: a **bulk OUT endpoint
`0x02` with a 512-byte max packet**, unbound to any kernel driver, alongside the
interrupt pair on interface 1 that `kraken2023` owns. That is the shape a
framebuffer transport has, and it sits on a different interface from the thermal
one. US-016 should start there. It is recorded, not acted on.

## Configuration compatibility

`CONFIG_SCHEMA_VERSION` moved from 1 to 2 for `Profile::lighting`. The field is
optional, so a schema-1 file parses exactly as it stands: `read_document` now
accepts any version at or below the current one and the next save rewrites it,
rather than sending an operator's profiles to recovery over an added field. A
version this build has never seen is still a recovery case.
`a_file_from_an_earlier_schema_is_migrated_rather_than_orphaned` and
`a_future_schema_version_is_a_recovery_case_not_a_guess` pin both directions.

## Validation

```
cargo fmt --all -- --check                              pass
cargo check --workspace --all-targets                   pass
cargo clippy --workspace --all-targets -- -D warnings   pass
cargo test --workspace                                  pass
```

Three live write-probe runs, `docs/ep-003-write-probe-us013.json`,
`docs/ep-003-write-probe-us015.json` and
`docs/ep-003-write-probe-us015-sweep.json`: thirty-seven reports, zero write
errors, every restoration confirmed by the operator.

## What this epic hands to US-019

Restoration works in one direction only, and the review that found it is
recorded here rather than argued away.

At every daemon start `apply_profile_lighting` puts the active profile's
channels back, so a service restart restores lighting. A *device* reconnect
does not: nothing observes the controller leaving, so `LightingExecutor::forget`
is never called, the committed record survives a device that no longer holds it,
and the `hidraw` handle opened at start keeps pointing at a device that is gone.
Every write then fails as uncertain until the service restarts.

`LightingExecutor::restore` and `forget` are written and unit-proven
(`a_reconnect_replays_every_committed_channel_exactly_once`,
`forgetting_the_device_makes_the_next_command_write_again`) but no caller
reaches them. They are deliberately left in place: what is missing is not the
replay, it is the hotplug detection that would trigger it, which means
observing the device leave, re-probing it, re-reading the topology,
re-resolving the capability state and re-opening the node.

That is US-019's own criterion for both devices, so PRD 1.3 scoped US-014 to
the daemon start it actually delivers and made US-019 name the lighting side
of the recovery, including these two entry points. The alternative was to build
half of US-019 here and rebuild it there.

## Corrections from the epic review

Three defects were found by reviewing the final tree against the criteria, and
fixed here.

- **Two lighting controls shared a tab stop.** The brightness stepper renders
  two buttons from one index, so the speed select counted after it landed on
  the stepper's second button. Every Lighting stop is now a named offset inside
  a per-row block, and
  `every_lighting_control_keeps_traversal_order_equal_to_visual_order` asserts
  the values the screen actually passes instead of a hand-written range.
- **`--rgb-write-probe --with-effects` understated itself.** The prompt that
  asks for authorization named only the dim white and off, then sent Breathing
  and Spectrum Wave as well. The description now follows the scope, and
  `the_operator_authorizes_the_scope_the_run_actually_sends` pins both.
- **No instrument evidenced the selectable effect parameters.** `--with-effects`
  sends each effect at one speed, so four speeds and one direction reached the
  Lighting screen unproven. `ProbeScope::EffectSweep` now sends every
  combination the screen can select, on the first channel only because speed
  and direction are firmware behavior rather than channel behavior. Restoration
  still covers every reported channel. The run that closes the criterion is
  recorded under "Every selectable parameter, on the hardware".
- **A controller that opens and then answers nothing had no test.** That is the
  live refusal path US-013 requires, and it was only reachable in production.
  `a_controller_that_answers_nothing_records_absence_rather_than_an_error`
  covers it. `Rgb::is_black` and two unused `FakeController` helpers were
  removed in the same pass.

## Left for later stories
- **LED counts.** The controller reports accessory identifiers and never a
  count, so `led_count` stays `Unknown` on every channel. Turning `0x17` into a
  number would take counting LEDs on the physical fan, which is a fact about a
  fan model rather than about this controller.
- **`--rgb-write-probe` is not covered by an automated end-to-end run.** Its
  logic is unit-proven against a controller that answers the real reports, and
  its hardware behaviour is recorded in the two JSON records. An automated run
  would have to write to the device, which is the one thing the design forbids
  without an operator.
