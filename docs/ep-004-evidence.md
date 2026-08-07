# EP-004 evidence map

Every acceptance criterion of Native Kraken LCD, with the code that implements
it and the check that proves it. Automated checks are test names inside
`cargo test --workspace`; live checks name the observation.

Machine under test: Fedora 44, Linux `7.1.6-201.fc44.x86_64`, NZXT Kraken Base
`1e71:300e` (bcdDevice `0200`) on `/sys/bus/usb/devices/1-9`.

## What the Kraken turned out to be

Read from the device and from the glass on 2026-08-07, not from a product page:

| Fact | Value | Source |
|---|---|---|
| Interface 0 | vendor class `0xff`, **no kernel driver**, one bulk OUT `0x02` at 512 bytes | `/sys/bus/usb/devices/1-9/1-9:1.0` |
| Interface 1 | HID, bound to `usbhid`, interrupt pair `0x01`/`0x81` at 64 bytes | `/sys/bus/usb/devices/1-9/1-9:1.1` |
| `hidraw` node | `/dev/hidraw10`, published beside `kraken2023` | the HID device under interface 1 |
| `usbfs` node | `/dev/bus/usb/001/004`, interface 0 claimed | `busnum` and `devnum` |
| Display reports | `0x30`, `0x32`, `0x36`, `0x38` out; `0x31`, `0x33`, `0x37`, `0x39` in, all 63 payload bytes | the device's own 401-byte HID report descriptor |
| Firmware | **`2.0.0`** | report `0x11 0x01`, bytes 17-19 |
| Panel present | **yes**: it answers `0x31 0x01` with brightness 100 and orientation 0 | report `0x31 0x01` |
| Resolution | 240x240, RGB565 big-endian, 115 200 bytes per frame | candidate from liquidctl's product table; the device reports none |
| Panel shape | **square**, corners lit | a white frame at 50%, observed by the operator |
| Buffer behavior | double-buffered; the first transfer on a link fills without swapping | the first of five frames never appeared, every later one did |

`lcd::VALIDATED_FIRMWARE` therefore contains exactly `2.0.0`, and nothing else
can be written by this build.

### The shape was wrong before it was observed

This product recorded the panel as circular, from the reasonable assumption that
a round cooler head carries round glass. A full white frame is what settles it,
because a solid fill makes the lit area's outline the panel's own outline, and
the operator saw a sharp square with its corners lit.

The consequence is not cosmetic. `PANEL_SHAPE` feeds the capability record, the
preview's clipping and the renderer's boundary ring. Recorded as circular, the
client would have clipped away corners the operator can actually see. The first
write-probe record, [`ep-004-write-probe-us016.json`](./ep-004-write-probe-us016.json),
was generated before the correction and still says `circular` in its `panel`
line: it is left as the run produced it, and this paragraph is the correction.

## The transport, and why it is allowed to exist

The Kraken's two interfaces are what makes this safe at all, and the split is
the whole argument:

* **Interface 1 is the thermal one.** `kraken2023` owns it. The display
  commands travel over the `hidraw` node *the driver itself publishes*: the
  kernel driver calls `hid_hw_start(hdev, HID_CONNECT_HIDRAW)` expressly so
  user space can share that interface
  ([`drivers/hwmon/nzxt-kraken3.c`](https://github.com/torvalds/linux/blob/master/drivers/hwmon/nzxt-kraken3.c)).
  The identifiers the driver uses (`0x10`, `0x70`, `0x72`, `0x74` out, `0x11`,
  `0x75` in) and the ones this product sends are disjoint, which
  `the_display_reports_never_collide_with_the_thermal_drivers_own` pins.
* **Interface 0 is nobody's.** It is vendor class with no driver bound, so
  `usbfs` can claim it without detaching anything. `Usbfs::claim` refuses any
  interface a driver is bound to, before it opens the node
  (`an_interface_a_driver_owns_is_never_claimed`). The one mistake that would
  break cooling cannot be made from here.

No kernel driver is detached on either side.

`usbfs` rather than libusb: the product already reads every identity field from
sysfs, so enumeration, matching and endpoint discovery were solved. What was
missing was three `ioctl` calls. The opcodes are derived rather than
hard-coded, and pinned against the numbers the kernel headers expand to
(`the_opcodes_match_the_kernel_headers`), because a silently changed derivation
would send a valid request to the wrong handler.

## The protocol this epic writes against

Adapted from liquidctl's `KrakenZ3` driver
([`liquidctl/driver/kraken3.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/kraken3.py)),
`SPDX-License-Identifier: GPL-3.0-or-later` and therefore compatible with this
project. Every identifier was cross-checked against the report descriptor this
machine's Kraken publishes.

| Report | Direction | Meaning |
|---|---|---|
| `0x10 0x01` | OUT | Firmware request, shared with the RGB controller |
| `0x11 0x01` | IN | Firmware answer, revision at offsets 17-19 |
| `0x30 0x01` | OUT | Display info request |
| `0x31 0x01` | IN | Brightness at `0x18`, orientation at `0x1a` |
| `0x30 0x02` | OUT | Set brightness and orientation together |
| `0x36 0x01 0x00 0x01 0x06` | OUT | Open a framebuffer transfer |
| `0x36 0x02` | OUT | Close one |

Then, on the bulk endpoint: a 20-byte header (twelve magic bytes, the transfer
kind, the little-endian length) as its own short-packet transfer, followed by
115 200 bytes of RGB565.

Three decisions shaped the implementation.

**A device with no panel answers `0x31 0x01` not at all.** That is the one
observation separating a Kraken with a screen from a Kraken without one, and it
is what the capability gate turns on. A unit that stays silent gets no
resolution written beside it
(`a_device_that_answers_no_display_report_gets_no_resolution_written_beside_it`).

**liquidctl carries two transfer sequences for this product id.** The 1.x one
negotiates memory buckets; the 2.x one is a plain start, header, payload, end.
Only 2.x is implemented, because it is the generation this unit's `bcdDevice`
suggests. A Kraken reporting any other major version is refused *by name*
rather than sent a sequence written for a firmware it is not
(`a_firmware_from_another_generation_is_named_and_refused`).

**Rotation happens in exactly one place.** The renderer turns the framebuffer;
the device is left on its own orientation zero. Turning it in both places would
double or cancel, and the preview would stop agreeing with the glass.

## US-016: Validate the LCD transport on `1e71:300e`

| Criterion | Implementation | Proof |
|---|---|---|
| Resolution, shape, orientation/brightness commands, endpoint and pixel format recorded or explicitly marked unknown | `hardware-linux/src/lcd.rs` (`candidate_panel`, `inspect`, `topology_from`), `core/src/capability.rs` (`LcdTopology`, `LcdPanel`), `daemon/src/main.rs` (`--probe`) | **Proven.** Every field is in [`capability-record.json`](./capability-record.json): firmware `2.0.0` from the device, brightness and orientation from the device, endpoint and interface from sysfs, resolution as a labeled candidate, shape from the white frame. `endpoints_are_recorded_for_the_interfaces_that_publish_them`, `a_panel_that_answered_is_recorded_with_its_geometry_and_its_settings`, `a_device_that_answers_no_display_report_gets_no_resolution_written_beside_it` |
| A solid test frame displays in the expected orientation and color, without detaching `kraken2023` | `lcd::LcdLink::send_frame`, `daemon/src/lcd_probe.rs` (`run`, `PROBE_COLORS`) | **Proven on the glass.** Two runs, [`ep-004-write-probe-us016.json`](./ep-004-write-probe-us016.json) is the second. Four frames, zero transfer errors, and the color sent is the color seen in all four cases: *"J'obtiens un écran rouge / vert / bleu / blanc avec écrit KRAKEN CONTROL"*. `kraken2023` stayed bound and `usbhid` stayed on interface 1 throughout; `hwmon5` was read immediately before and after each run and did not move |
| 1 frame per second for 30 minutes with zero lost `hwmon` samples above 2 s and zero unhandled USB errors | `daemon/src/server.rs` (`spawn_display_ticker`), `daemon/src/display.rs` (`refresh`) | See "Thirty minutes of output" below |
| A model, firmware or endpoint that cannot be proven keeps LCD read-only, and no sequence from another PID is attempted | `hardware-linux/src/probe.rs` (`lcd_state`), `hardware-linux/src/lcd.rs` (`VALIDATED_FIRMWARE`, `SUPPORTED_FIRMWARE_MAJOR`) | **Proven**, including live before the rule existed. `a_kraken_that_never_answered_the_display_report_stays_read_only`, `a_firmware_from_another_generation_is_named_and_refused`, `an_unvalidated_firmware_of_the_right_generation_is_still_refused`, `a_validated_firmware_is_the_only_thing_that_opens_the_frame_path`, `an_unvalidated_firmware_refuses_every_frame_and_names_the_missing_evidence`; capture [`ep-004-lcd-unvalidated.png`](./screenshots/ep-004-lcd-unvalidated.png) |
| A disconnect during a transfer releases the endpoint, keeps the daemon alive and permits a fresh capability probe after reconnect | `usbfs::Usbfs::drop` (releases the claim), `daemon/src/display.rs` (`uncertain`, `faulted`, `forget`), `lcd::LcdLink::unprime` | **Partial, deferred to US-019.** `a_failed_transfer_reports_uncertain_and_forgets_the_picture`, `a_panel_that_refuses_every_frame_is_reported_rather_than_retried` and `a_forgotten_panel_is_written_again_rather_than_deduplicated` prove the software half. A physical unplug mid-transfer has not been performed; hotplug detection is US-019's, and this is what EP-004 hands it |

### The defect the first run found

The first write probe sent red and **nothing changed on the panel**. The
transfer reported no error, 115 200 bytes went out, and the operator saw the
previous picture. Green, blue and white then all landed immediately.

That is not a failed transfer, it is a buffer swap. The panel double-buffers and
swaps on the transfer *after* the one that filled it, so a lone first frame
lands where nothing shows it. liquidctl sends static images twice with the
comment "sending it twice is only required once after initialization", which is
the same observation from the other side.

`LcdLink` now primes: the first frame of a link goes out twice, every later one
once, and `unprime()` rearms after a disconnect because a frame swallowed on
reconnect would be indistinguishable from a transfer that failed. The second run
confirms it, and its record carries the count per step:

```
solid red at 50%     sequences=2   27426 us   "J'obtiens un écran rouge avec écrit KRAKEN CONTROL"
solid green at 50%   sequences=1   13933 us   "un écran vert"
solid blue at 50%    sequences=1   13743 us   "un écran bleu"
solid white at 50%   sequences=1   14074 us   "un écran blanc"
```

27.4 ms for the primed frame is two 13.7 ms sequences, which is what the
doubling costs: once per connection.
`the_first_frame_of_a_link_goes_out_twice_and_no_later_one_does` pins both
halves, and it was written from this measurement rather than from the comment.

### What "restore" means here

No report on this device reads the panel back, so the picture an operator had
before a probe cannot be reproduced from anything this product recorded. The
probe therefore restores to *this product's own default view* and asks what the
operator sees, rather than claiming to have put back what was there. Both runs
ended `"restoration": {"state": "confirmed"}`, and the second run's prior state
was the first run's restoration, described identically both times: *"un
graphique avec un arc de cercle en haut et un en bas en haut CPU et en bas GPU
et pas de valeur juste 2 traits de chaque côté."*

That description is itself evidence: two arcs, the captions in the right places,
and dashes where the readings would be, because the probe renders against
deliberately unavailable samples. The layout, the typefaces and the unavailable
state were all confirmed on the physical glass by that sentence.

### Access this epic needs

Two nodes, one rule file, numbered below 73 so `73-seat-late.rules` still
consumes the `uaccess` tag:

```
# /etc/udev/rules.d/70-nzxt-control.rules
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1e71", ATTRS{idProduct}=="2021", TAG+="uaccess"
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1e71", ATTRS{idProduct}=="300e", TAG+="uaccess"
SUBSYSTEM=="usb",    ATTR{idVendor}=="1e71",  ATTR{idProduct}=="300e",  TAG+="uaccess"
```

The second line is the display commands, the third the framebuffer. Without the
third, brightness would work and frames would not, which is a real state and is
reported as itself rather than collapsed into one refusal
(`a_missing_bulk_node_disables_frames_without_disabling_brightness`).

One trap worth recording: `udevadm trigger --attr-match=idVendor=1e71` selects
the USB device that *carries* that attribute, not its `hidraw` child, so the
second line does not take effect until `udevadm trigger --action=add
/sys/class/hidraw/hidraw10` or a replug. The `usbfs` line applies immediately
and the `hidraw` one silently does not, which looks exactly like a wrong rule.

## US-017: Build the LCD editor, preview and static output

| Criterion | Implementation | Proof |
|---|---|---|
| Display-mode and metric selects, Reading 1/2, Text 1/2, Background and Logo colors, Rotate Display, and a preview matching the panel | `app/src/display.rs` (`DisplayEditor`, `DisplayColorField`), `app/src/shell.rs` (`lcd`, `metric_select`, `color_field`) | `the_editor_exposes_the_six_color_controls_the_story_names`, `every_color_field_is_separately_addressable`, `the_six_fields_are_ordered_so_each_pair_sits_together`, `every_lcd_control_has_its_own_tab_stop_inside_the_screen_range`; captures [`ep-004-lcd-editor.png`](./screenshots/ep-004-lcd-editor.png) and [`ep-004-lcd-writable.png`](./screenshots/ep-004-lcd-writable.png), both against the answering panel |
| Any editor change repaints the preview within 16.7 ms at P95, writing no hardware | `app/src/display.rs` (`DisplayScreen::edit`), `lcd-renderer` (`render`, `Framebuffer::to_png`), `app/src/preview.rs` (`panel_preview`) | `every_editor_change_moves_the_preview_with_it` covers *which* changes reach the preview, and `a_preview_repaint_stays_inside_the_frame_budget` covers how long one takes. Measured on this machine, release build, 300 repaints with a moving reading: render alone **1.12 ms** at P95, render plus PNG encode **1.50 ms** at P95, against a 16.7 ms budget. Nothing in the path opens a device |
| Apply renders one typed `DisplayPreset` into both the preview and the exact-resolution framebuffer | `crates/lcd-renderer` called by `app/src/preview.rs` and by `daemon/src/display.rs` | `the_preview_renders_the_same_frame_the_daemon_would_send`, `a_frame_encodes_to_a_png_the_toolkit_can_decode` (asserts every preview pixel equals the frame pixel), `a_frame_is_exactly_the_panels_size_in_the_panels_format`, `rgb565_packs_five_six_five_most_significant_byte_first` |
| An invalid, incomplete or out-of-gamut hex leaves Apply disabled, the prior valid preview visible, and sends no frame | `app/src/display.rs` (`parsed_color`, `PreviewState`), `app/src/shell.rs` (`apply` state) | `an_incomplete_color_names_its_own_field_and_blocks_apply`, `the_preview_keeps_the_last_valid_picture_while_a_field_is_mid_edit`, `a_preset_the_renderer_refuses_never_reaches_the_endpoint`, `an_invalid_preset_is_refused_before_the_capability_gate_is_even_reached` |
| A static image that fails to decode or exceeds 8192x8192 is rejected without panic, and no partial frame reaches the device | `lcd-renderer/src/lib.rs` (`draw_image`), `core/src/display.rs` (`MAX_IMAGE_DIMENSION`) | `a_file_that_is_not_an_image_is_rejected_without_panicking`, `an_image_larger_than_the_ceiling_is_refused_before_it_is_decoded` (a 45-byte PNG declaring 9000x9000: the refusal comes from the size it claims, not from the file being large), `image_mode_without_a_file_is_refused_before_anything_is_drawn` |
| The default preview uses the project's own wordmark and never NZXT's | `core/src/lib.rs` (`PRODUCT_NAME`), `lcd-renderer` (`draw_wordmark`) | `the_panel_never_carries_a_vendors_wordmark`, `the_face_covers_every_character_the_panel_can_be_asked_to_draw` |

### Two typefaces, both drawn rather than loaded

The panel is 240 pixels across and needed text. Embedding a font file would put
a third party's licensing into the shipped binary for six captions, so both
faces are original work: a five by seven bitmap for the captions and the
wordmark, and a seven-segment face for the readings, where a stroked shape stays
legible at a glance and scales without a staircase. The arcs and the segments
are rasterized with coverage rather than hard pixels, so an edge lands as a
blend.

### What the screen does not offer

`DisplayMode::Image` is absent from the mode select
(`the_screen_offers_only_the_modes_it_can_configure_completely`). The screen has
no control that can name a file, because this codebase has no text-input
primitive: US-004 built nine components and none of them accepts free text. A
mode whose Apply could only ever refuse would be a control that says the feature
is here and then does nothing, which is the same rule that keeps an unproven
lighting effect absent rather than disabled.

The mode stays in the vocabulary, in the renderer and in the daemon, and a saved
profile can select it. US-017 AC-1 lists the controls the screen must contain
and an image picker is not among them; AC-5 asks only that a user-provided image
be rejected safely, which the three tests above prove.

## US-018: Render and stream the dual CPU/GPU infographic

| Criterion | Implementation | Proof |
|---|---|---|
| Two colored arcs, two temperatures, CPU/GPU labels and the wordmark, in the selected colors | `lcd-renderer/src/lib.rs` (`draw_infographic`, `draw_reading`, `draw_wordmark`) | `both_readings_are_drawn_and_each_uses_its_own_colors` (asserts every selected color appears in the frame), `the_two_gauges_occupy_opposite_halves_of_the_dial`, `a_higher_reading_fills_more_of_its_gauge`; capture [`ep-004-lcd-editor.png`](./screenshots/ep-004-lcd-editor.png) shows the live CPU and GPU readings |
| The framebuffer updates once per second and displayed data age stays <=2 s at P95 over 30 minutes | `core/src/display.rs` (`FRAME_INTERVAL_MS`), `daemon/src/server.rs` (`spawn_display_ticker`), `daemon/src/state.rs` (`tick_display`) | See "Thirty minutes of output" under US-016. The render itself costs 1.13 ms at P95 on the device path, so the age is bounded by the tick rather than by the work |
| An unavailable metric renders `--` and a neutral arc rather than zero degrees | `core/src/display.rs` (`MetricSample::text`, `fraction`), `lcd-renderer` (the `None` arm of the arc) | `an_unavailable_reading_shows_dashes_and_never_a_zero_gauge` (asserts the gauge loses the reading's color while the dashes keep it, and that the frame differs from a reading of zero), `an_unavailable_metric_is_never_drawn_as_zero`, `a_reading_and_its_unavailable_marker_occupy_the_same_room` |
| Rotate Display turns the preview and the physical output together by the validated increment, preserving text alignment | `core/src/display.rs` (`Orientation`), `lcd-renderer/src/lib.rs` (`rotate`) | `a_quarter_turn_moves_the_picture_and_keeps_every_pixel`, `four_quarter_turns_return_the_picture_unchanged`, `rotation_walks_the_validated_increment_and_returns_to_zero`; capture [`ep-004-lcd-rotated-180.png`](./screenshots/ep-004-lcd-rotated-180.png). The two turn together by construction rather than by coincidence: one framebuffer is both, the device is left on its own orientation zero, and the frames already proven to reach the glass verbatim carry the rotation as pixel content. A rotated frame has not been photographed on the panel, which is the one part of this row that rests on that inference rather than on an observation, and which is deferred to the polish iteration |
| 30 minutes of output adds <=0.5 percentage points of average CPU and queues no more than one unsent frame | `daemon/src/display.rs` (synchronous send, no queue), `daemon/src/server.rs` (missed ticks are dropped, not caught up) | See "Thirty minutes of output". There is no queue that *can* exceed one: the transfer is synchronous, and a tick that runs late skips the intervals it missed and counts them rather than catching up (`dropped_frames_are_counted_rather_than_queued`) |
| Backpressure or a transfer failure drops stale frames and retries only after a reconnect or an explicit recoverable state | `daemon/src/display.rs` (`faulted`, `refresh`, `apply`, `forget`) | `a_failed_transfer_reports_uncertain_and_forgets_the_picture`, `a_forgotten_panel_is_written_again_rather_than_deduplicated`, `dropped_frames_are_counted_rather_than_queued` |

### Deduplication is on the picture, not the preset

The infographic renders a new picture whenever a reading moves, from a preset
that never changed, so comparing presets would send a frame every second forever
and comparing readings would miss a color change. The executor keeps the bytes
it last committed and compares those
(`a_reading_that_moved_produces_a_frame_and_one_that_did_not_does_not`). Whether
a fraction of a degree survives into a different picture depends on where the
antialiased end of the gauge falls, so nothing promises it either way; US-018
asks for one frame a second regardless.

## Boundaries this epic did not cross

- **No kernel driver is detached.** The display commands go through the node
  `kraken2023` itself publishes, and the framebuffer through an interface no
  driver claimed. `Usbfs::claim` refuses a bound interface outright.
- **Nothing runs as root.** `--lcd-write-probe` refuses to start as root and
  points at the udev rule instead.
- **One writer.** `--lcd-write-probe` takes the same per-device lock the daemon
  takes, so the probe and the service can never address the Kraken at once.
- **The frame size is checked before the first byte leaves.** A payload that is
  not exactly 115 200 bytes never reaches the endpoint
  (`a_frame_of_the_wrong_size_never_reaches_the_endpoint`).
- **Only OUT endpoints are written**, asserted per transfer rather than trusted
  (`an_in_endpoint_is_refused_before_any_ioctl_happens`).

## Corrections made while building this

Six defects were found and fixed rather than accepted. Three came from the
hardware, which is what the epic existed to ask.

- **The first frame never reached the glass.** The panel's buffer swap. See
  "The defect the first run found".
- **The panel is square, not round.** Recorded as circular from an assumption
  about the cooler's shape until a white frame showed its corners. See "The
  shape was wrong before it was observed".
- **A record blamed a path it never opened.** When the `hidraw` open failed,
  every field of the topology carried that permission error, including the bulk
  node, which had not been attempted. It now says "not attempted", because a
  record that names the wrong file sends an operator to the wrong rule
  (`a_node_that_cannot_be_opened_never_blames_the_half_it_did_not_reach`).
- **No command produced a complete capability record.** `--rgb-probe` attached
  the controller's topology and `--lcd-probe` the panel's, so re-recording
  `docs/capability-record.json` with either dropped the other device's evidence.
  `--probe` asks both and is now what regenerates the artifact.
- **The editor's buttons were clipped at 920x640.** The preview column sized
  itself to the sentence under the preview, squeezing the editor until the
  brightness `+` and Apply ran off the panel. The column is now fixed width and
  the sentence wraps.
- **One contrast threshold was applied to five elements.** The default violet
  reading is 3.77:1 on the default background, which fails a 4.5:1 text bar and
  passes the 3:1 non-text one. The readings are forty pixels of digit and their
  gauges are not text at all, so each element is now measured against the
  threshold that applies to it, which is the split the project's accessibility
  budget already makes.

- **The display-mode select moved the editor and left the preview behind.** The
  editor and the preview are deliberately two values, so that a half-typed color
  keeps the last whole picture on screen. Four of the five controls re-derived
  the preview after mutating; the mode select did not, so choosing Solid color
  redrew the controls while the preview kept painting the infographic and Apply
  would have sent a picture the window never showed. The pair is now one
  `DisplayScreen` whose only mutator re-derives the preview, so the omission is
  not available to make (`every_editor_change_moves_the_preview_with_it`).

Two more were caught by the tooling rather than by the hardware:

- **`DisplayError` and `IpcError` both serialized a field called `error`.** Two
  tags of the same name in one object is a frame neither side can decode. Found
  by an integration test, not by review; `DisplayError` is now tagged `kind`
  like every other validation error.
- **`LcdEditor` was a placeholder with four colors and one metric.** US-004
  built it before a preset type existed. It is gone, replaced rather than
  extended, and `docs/ep-001-evidence.md` was corrected to name tests that still
  exist.

## Shared code this epic moved

The HID transport (`HidTransport`, `Hidraw`, the 64-byte report shape, the
firmware report) moved from `rgb.rs` into `hardware-linux/src/hid.rs`, because
both devices speak it. `rgb::RgbError` is now that module's error under the name
every lighting caller already used, so nothing outside changed. A test in each
module pins the shared identifiers so the two cannot drift onto different bytes
(`the_firmware_report_is_the_same_one_the_controller_answers`).

## Schema versions

- `CAPABILITY_SCHEMA_VERSION` 2 to 3, for `LcdTopology`.
- `CONFIG_SCHEMA_VERSION` 2 to 3, for `Profile::display`. The field is optional,
  so a schema-2 file parses as it stands and the next save rewrites it.
- `PROTOCOL_VERSION` 2 to 3, for `ApplyDisplay` and `DisplayState`.

`DisplayError` is tagged `kind` rather than `error`, because `IpcError` wraps it
and is itself tagged `error`: two `error` tags in one object is a frame neither
side can decode. That was found by an integration test, not by review.

## Validation

```
cargo fmt --all -- --check                              pass
cargo check --workspace --all-targets                   pass
cargo clippy --workspace --all-targets -- -D warnings   pass
cargo test --workspace                                  pass, 467 tests
```

Stop `nzxt-controld` first. The fixtures mirror this machine down to the
`hidraw` numbers and `usb::hidraw_node` maps a fixture node onto the real `/dev`
path, so a running daemon is detected as a competing writer and the ownership
assertions in `crates/daemon/tests/ipc.rs` fail. That is the conflict detector
doing its job on a real conflict, and it is recorded here because it looks like
a broken suite the first time it happens.

Live runs, all on 2026-08-07 against `1e71:300e` firmware `2.0.0`:

| Run | Result |
|---|---|
| `--lcd-probe` before the udev rule | every field `unknown` with the permission reason, both capabilities refused naming US-016 |
| `--lcd-probe` after the rule | firmware, brightness, orientation and both nodes recorded |
| `--lcd-write-probe`, first | 4 frames, 0 errors, **first frame invisible**: the buffer-swap defect |
| `--lcd-write-probe`, second | 4 frames, 0 errors, every color seen as sent, restoration confirmed |
| `--probe` | `docs/capability-record.json` re-recorded at schema 3 with both topologies |
| 30-minute stream | below |

### Thirty minutes of output

`--probe` firmware `2.0.0`, the dual infographic streaming at one frame per
second, `hwmon5` read once per second by an independent sampler for the whole
run, 2026-08-07:

| Measure | Result | Budget |
|---|---|---|
| Elapsed | 1800.3 s | 1800 s |
| `hwmon` samples taken | 1800 | one per second |
| `hwmon` gaps above 2 s | **0** | 0 |
| `hwmon` read errors | **0** | 0 |
| Frames dropped for backpressure | **0** | at most one pending |
| Daemon CPU, 30-minute average | **0.221%** | LCD adds <=0.5 pp |
| Streaming still active at the end | yes | |

Three criteria close on this run.

**US-016 AC-3.** Eighteen hundred frames went out while eighteen hundred
`hwmon` samples came back, and not one sample was more than two seconds after
its predecessor. The two interfaces do not interfere: that is the coexistence
the whole transport design rests on, measured rather than argued.

**US-018 AC-5.** The daemon's *total* CPU over the run was 0.221%, which
includes the 1 Hz collectors, the socket and the rendering. The LCD's own
contribution cannot exceed the total, so it is under the 0.5 point budget
without needing a separate baseline. Zero dropped frames means no tick ever ran
late enough to skip an interval.

**US-018 AC-2.** The cadence held for the full run. The displayed age is
bounded by construction rather than sampled: the collectors run at 1 Hz and the
ticker at 1 Hz independently, so a frame carries a reading at most one second
old and is drawn at most one second after that. The render itself is 1.13 ms at
P95, three orders of magnitude below the interval, so it contributes nothing to
the age. Zero dropped frames is what rules out the case that would break the
bound, which is a tick that ran late.

## Deferred, not proven

EP-004 is closed as `DONE` on 2026-08-07 with three observations deliberately
deferred rather than performed. They are recorded here and in
`tasks/prd-native-nzxt-hardware-control-status.json` under `deferred`, because a
criterion nobody observed is a criterion nobody observed, whatever the tracker
says next to it. Two of the three are folded into the display polish iteration
of the PRD, and the third belongs to US-019.

**A physical disconnect during a transfer (US-016 AC-5), deferred to US-019.**
The software half is
proven: a failed transfer reports `Uncertain`, drops the committed record rather
than claiming a picture the panel may not hold, stops the stream instead of
retrying every second, releases the `usbfs` claim on drop, and rearms the buffer
priming so a frame swallowed on reconnect is not mistaken for a failure. What
has not happened is an actual unplug mid-frame. Unplugging the Kraken's USB
carries no thermal risk, because the pump keeps running its onboard program,
which is the property this architecture exists to guarantee. EP-003 met the same
wall for lighting and PRD 1.3 moved the physical-unplug half to US-019, which
owns hotplug detection for both devices. The same move fits here, and it is a
scoping decision for the PRD rather than one this document should make quietly.

**Rotation on the glass (US-018 AC-4), deferred to the polish iteration.** The
preview rotates, the framebuffer
rotates, and the frames this epic sent were seen on the panel exactly as
rendered, so a rotated frame follows. It has not been observed. One run applying
the four orientations in turn would replace that inference with an observation,
and the panel is left upright either way.

**The UI gate matrix for the LCD screen (US-017), deferred to the polish
iteration.** The PRD asks every UI story for the
build under Wayland *and* X11, at 920x640 *and* 1280x720, at 100% *and* 200%
scale, completed by keyboard alone. Four captures are committed and the 920x640
clipping defect above was found by running at that size, so the screen has been
exercised; what this document does not record is which display server, which
scale and whether the keyboard walkthrough was performed end to end.
`every_lcd_control_has_its_own_tab_stop_inside_the_screen_range` proves the tab
stops are distinct and inside the screen's range, which is a necessary
condition and not the walkthrough itself. EP-003 recorded "920x640 under X11"
for the Lighting screen; the same line is missing here, and it is a gap in the
record rather than a known failure.

Nothing else in the epic rests on an untested assumption.
