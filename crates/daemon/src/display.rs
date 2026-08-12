// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The only caller that sends a frame to the Kraken's panel.
//!
//! Mirrors [`crate::lighting`]: one owner, one record of what was committed,
//! one place a frame leaves the process. Two rules live here rather than at the
//! screen, because the screen is not a trusted input.
//!
//! **A frame identical to the committed one is not sent.** The comparison is on
//! the rendered picture, not on the preset, so a solid field or a static image
//! costs one transfer however often Apply is activated, and an infographic
//! whose readings did not move costs none. Whether a fraction of a degree
//! survives into a different picture depends on where the antialiased end of
//! the gauge happens to fall, so nothing here promises it either way. The
//! stream produces one frame a second regardless.
//!
//! **The brightness is not part of that comparison.** It is a panel setting
//! carried by its own report, so it is written before the picture is compared
//! and a deduplicated frame never swallows it. Sending it afterwards was a
//! defect: a preset whose only edit was the brightness renders the same pixels,
//! so the glass stayed at the old level until some reading happened to move the
//! picture, and the client said "nothing was sent" the whole time.
//!
//! **At most one frame is ever outstanding.** The transfer is synchronous, so
//! "outstanding" here means the sample that arrived while the previous frame
//! was still being written. Such a sample replaces the pending one rather than
//! queueing behind it: a panel showing a temperature from four seconds ago
//! because three frames are waiting is worse than a panel that skipped them.
//!
//! **A refusal the panel imposes is a [`HardwareState`], never an error.** This
//! is the one of the three executors that still returns a `Result`, and what its
//! error channel carries is the single thing the other two cannot meet: a preset
//! that has no bytes to send, because the picture it names could not be
//! rendered. Nothing about the panel's own behavior travels that way, so a
//! caller reads "what the hardware did" in one place for all three paths.
//!
//! **An animation is decoded once and then only copied.** A GIF the operator
//! picks is compiled to a table of finished framebuffers when it is applied, so
//! what the tick loop does per frame is a copy and a transfer. It plays on its
//! own clock, taken from the file rather than from the telemetry cadence, and
//! the same rule about late ticks applies to it: the cursor walks the wall
//! clock, so a tick that ran late skips frames instead of playing the animation
//! in slow motion.

use std::time::{Duration, Instant};

use kori_core::capability::LcdPanel;
use kori_core::display::{DisplayError, DisplayPreset, MetricSample};
use kori_core::ipc::{DisplayOutcome, DisplayState, HardwareState};
use kori_core::lighting::Brightness;
use kori_hardware_linux::lcd::{LcdError, LcdLink};

/// Owns the panel handle and the record of what it was told to show.
pub struct DisplayExecutor {
    link: Option<LcdLink>,
    /// Geometry a frame must match, from the topology the probe recorded.
    panel: Option<LcdPanel>,
    /// What the operator asked for. Survives a failed transfer, because a
    /// failure changes what the panel holds, not what was requested.
    active: Option<DisplayPreset>,
    /// The last preset committed, and the exact bytes it produced.
    committed: Option<Committed>,
    /// Brightness the panel was last told to use.
    brightness: Option<Brightness>,
    /// Why streaming stopped, once a transfer failed.
    ///
    /// A stream that kept retrying every second would hammer an endpoint that
    /// has already refused, so it stops until something changes: the device
    /// coming back, or the operator applying a preset by hand. A faulted
    /// stream retries only after a reconnect or an explicit recoverable state.
    faulted: Option<String>,
    /// Frames discarded because a newer sample replaced them.
    dropped: u64,
    /// The animation playing, when the active preset names one.
    animation: Option<Animation>,
}

/// What the panel is showing, and the frame that put it there.
///
/// The bytes are kept because the deduplication has to compare the *picture*,
/// not the preset: the infographic renders a new picture whenever a reading
/// changes, from a preset that never changed.
struct Committed {
    preset: DisplayPreset,
    frame: Vec<u8>,
}

/// A compiled animation and where the panel is in it.
///
/// The frames are held as the bytes the transport takes rather than as
/// framebuffers: nothing downstream of the compile step needs the pixels back,
/// and two bytes per pixel rather than three is a third of the table.
struct Animation {
    frames: Vec<AnimationFrame>,
    cursor: usize,
    /// When the frame after the current one is due.
    due: Instant,
}

struct AnimationFrame {
    bytes: Vec<u8>,
    delay: Duration,
}

impl DisplayExecutor {
    /// An executor with no panel behind it.
    ///
    /// Every command is refused with the reason the caller recorded in the
    /// capability record, so an absent panel is a disabled control rather than
    /// a special case scattered through the daemon.
    pub fn absent() -> Self {
        Self {
            link: None,
            panel: None,
            active: None,
            committed: None,
            brightness: None,
            faulted: None,
            dropped: 0,
            animation: None,
        }
    }

    /// An executor bound to a panel that answered its display report.
    pub fn connected(link: LcdLink, panel: LcdPanel) -> Self {
        Self {
            link: Some(link),
            panel: Some(panel),
            ..Self::absent()
        }
    }

    /// True when a frame could actually be sent right now.
    fn is_connected(&self) -> bool {
        self.link.is_some() && self.panel.is_some()
    }

    /// The preset the panel is showing, as far as the daemon knows.
    pub fn committed(&self) -> Option<&DisplayPreset> {
        self.committed.as_ref().map(|entry| &entry.preset)
    }

    /// The preset the operator asked for, whatever the panel then did with it.
    pub fn active(&self) -> Option<&DisplayPreset> {
        self.active.as_ref()
    }

    /// Whether a tick would currently produce anything.
    ///
    /// An animation counts: it is the panel moving on its own, which is what
    /// this flag means to the screen, and a fault has to be able to stop it the
    /// same way it stops a stream of readings.
    fn is_streaming(&self) -> bool {
        if !self.is_connected() || self.faulted.is_some() {
            return false;
        }
        self.animation.is_some()
            || self
                .active
                .as_ref()
                .is_some_and(|preset| preset.mode.uses_readings())
    }

    /// Per-panel state for [`kori_core::ipc::DaemonStatus`].
    pub fn state(&self) -> DisplayState {
        DisplayState {
            panel: self.panel.clone(),
            committed: self.committed().cloned(),
            streaming: self.is_streaming(),
            faulted: self.faulted.clone(),
            dropped_frames: self.dropped,
        }
    }

    /// Count one sample that arrived too late to become a frame.
    pub fn drop_frame(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }

    /// Forget what the panel is showing.
    ///
    /// Called when the device goes away: without this, an Apply after a
    /// reconnect could deduplicate against a picture the panel no longer holds
    /// and silently send nothing.
    pub fn forget(&mut self) {
        self.committed = None;
        self.brightness = None;
        self.faulted = None;
    }

    /// Render `preset` against `samples` and send the result.
    ///
    /// The rendering happens here rather than at the client so the panel keeps
    /// updating with the window closed, and from the same crate the client
    /// previews with so the two cannot disagree.
    pub fn apply(
        &mut self,
        preset: &DisplayPreset,
        samples: &[MetricSample; 2],
    ) -> Result<DisplayOutcome, DisplayError> {
        // An Apply the operator activated is the explicit recoverable state a
        // faulted stream waits for.
        self.faulted = None;
        // Whatever was playing belongs to the preset being replaced. Dropped
        // before the new one is compiled rather than after, so a file that
        // fails to decode leaves the panel with no animation instead of the
        // previous operator's.
        self.animation = None;
        let outcome = if preset.mode.uses_image() {
            self.start_image(preset)?
        } else {
            self.send(preset, samples)?
        };
        self.active = Some(preset.clone());
        Ok(outcome)
    }

    /// Compile the picture a preset names and put its first frame on the glass.
    ///
    /// The whole decode happens here, once. A file that turns out to carry more
    /// than one frame is installed as an animation and the tick loop takes it
    /// from there; one that does not is an ordinary frame and nothing is
    /// installed, so a still picture costs no clock at all.
    fn start_image(&mut self, preset: &DisplayPreset) -> Result<DisplayOutcome, DisplayError> {
        let Some(panel) = self.panel.clone() else {
            return Ok(absent(preset));
        };
        let frames: Vec<AnimationFrame> = kori_lcd_renderer::render_image_frames(preset, &panel)?
            .into_iter()
            .map(|frame| AnimationFrame {
                bytes: frame.frame.to_rgb565_be(),
                delay: frame.delay,
            })
            .collect();

        let Some(first) = frames.first() else {
            return Ok(absent(preset));
        };
        let bytes = first.bytes.clone();
        let delay = first.delay;
        let outcome = self.send_bytes(preset, bytes);

        if frames.len() > 1 {
            self.animation = Some(Animation {
                frames,
                cursor: 0,
                due: Instant::now() + delay,
            });
        }
        Ok(outcome)
    }

    /// Put the next frame of the animation on the glass, if one is due.
    ///
    /// Returns when the frame after that one is due, so the caller can sleep to
    /// it rather than poll for it. `None` means nothing is playing, which is
    /// also what a faulted or disconnected panel reports: a stopped stream must
    /// not keep a clock running against a link that is refusing.
    pub fn advance_animation(&mut self, now: Instant) -> Option<Instant> {
        if !self.is_streaming() {
            return None;
        }
        let animation = self.animation.as_mut()?;
        if now < animation.due {
            return Some(animation.due);
        }

        // The cursor walks the wall clock rather than the tick count. A tick
        // that ran late skips the frames it slept through, which is the same
        // rule a late telemetry frame follows and for the same reason:
        // a backlog of old pictures has nothing to offer a panel. The loop is
        // bounded because every delay is at least MIN_FRAME_DELAY_MS.
        let mut skipped = 0u32;
        loop {
            animation.cursor = (animation.cursor + 1) % animation.frames.len();
            let frame = animation.frames.get(animation.cursor)?;
            animation.due += frame.delay;
            if now < animation.due {
                break;
            }
            skipped += 1;
        }

        let due = animation.due;
        let bytes = animation.frames.get(animation.cursor)?.bytes.clone();
        let preset = self.active.clone()?;
        for _ in 0..skipped {
            self.drop_frame();
        }

        let outcome = self.send_bytes(&preset, bytes);
        if let HardwareState::Uncertain { reason } = &outcome.hardware {
            self.faulted = Some(reason.clone());
            return None;
        }
        Some(due)
    }

    /// Redraw the active preset against a fresh sample, once per tick.
    ///
    /// Returns `None` when there is nothing to do: no preset, no panel, a
    /// preset that does not read telemetry, an animation running on its own
    /// clock, or a stream that has faulted.
    pub fn refresh(&mut self, samples: &[MetricSample; 2]) -> Option<DisplayOutcome> {
        // With no animation installed, [`Self::is_streaming`] is exactly "the
        // active preset reads telemetry", so the preset is not asked the same
        // question again below.
        if !self.is_streaming() || self.animation.is_some() {
            return None;
        }
        let preset = self.active.clone()?;
        match self.send(&preset, samples) {
            Ok(outcome) => {
                if let HardwareState::Uncertain { reason } = &outcome.hardware {
                    self.faulted = Some(reason.clone());
                }
                Some(outcome)
            }
            // A preset that stopped rendering (an image the operator deleted,
            // say) stops the stream rather than failing every second.
            Err(error) => {
                self.faulted = Some(error.to_string());
                None
            }
        }
    }

    fn send(
        &mut self,
        preset: &DisplayPreset,
        samples: &[MetricSample; 2],
    ) -> Result<DisplayOutcome, DisplayError> {
        let Some(panel) = self.panel.clone() else {
            return Ok(absent(preset));
        };

        let frame = kori_lcd_renderer::render(preset, samples, &panel)?.to_rgb565_be();
        Ok(self.send_bytes(preset, frame))
    }

    /// Everything a frame goes through once it exists: the brightness, the
    /// comparison against what the panel holds, and the transfer.
    ///
    /// Split out so an animation frame, which was rendered when the file was
    /// picked rather than on this tick, takes exactly the same path as one the
    /// renderer just produced. Both are then deduplicated the same way, which
    /// is what keeps a GIF whose frames repeat from costing a transfer per
    /// repeat.
    fn send_bytes(&mut self, preset: &DisplayPreset, frame: Vec<u8>) -> DisplayOutcome {
        let Some(link) = self.link.as_mut() else {
            return absent(preset);
        };

        // Brightness travels over its own report and only when it changed, so
        // a streaming preset does not resend it every second. It goes out ahead
        // of the frame comparison because it is a panel setting rather than a
        // picture: an unchanged picture must not be able to discard it.
        let mut brightness_sent = false;
        if self.brightness != Some(preset.brightness) {
            if let Err(error) = link.set_display(preset.brightness) {
                return self.uncertain(preset, &error, false);
            }
            self.brightness = Some(preset.brightness);
            brightness_sent = true;
        }

        // The picture is what the panel holds, so the picture is what is
        // compared. A preset that changed without changing a pixel still costs
        // nothing, and a preset that did not change but whose readings did is
        // still sent.
        if self
            .committed
            .as_ref()
            .is_some_and(|entry| entry.frame == frame)
        {
            return DisplayOutcome {
                preset: preset.clone(),
                hardware: HardwareState::Confirmed,
                frames: 0,
                deduplicated: true,
                brightness_sent,
            };
        }

        match link.send_frame(&frame) {
            Ok(_) => {
                self.committed = Some(Committed {
                    preset: preset.clone(),
                    frame,
                });
                DisplayOutcome {
                    preset: preset.clone(),
                    hardware: HardwareState::Confirmed,
                    frames: 1,
                    deduplicated: false,
                    brightness_sent,
                }
            }
            Err(error) => self.uncertain(preset, &error, brightness_sent),
        }
    }

    /// A transfer that may or may not have landed.
    ///
    /// The record of what the panel is showing is dropped rather than left
    /// claiming a picture that may never have arrived, which is the same
    /// reasoning the lighting path follows for a controller that acknowledges
    /// nothing. Dropping it is the point of this being a method: the outcome
    /// and the forgetting are one act, and a caller that built the outcome
    /// without forgetting would leave the executor claiming a frame it has just
    /// reported as uncertain.
    fn uncertain(
        &mut self,
        preset: &DisplayPreset,
        error: &LcdError,
        brightness_sent: bool,
    ) -> DisplayOutcome {
        self.committed = None;
        DisplayOutcome {
            preset: preset.clone(),
            hardware: HardwareState::Uncertain {
                reason: error.to_string(),
            },
            frames: 0,
            deduplicated: false,
            brightness_sent,
        }
    }
}

/// A command that reached no panel at all.
fn absent(preset: &DisplayPreset) -> DisplayOutcome {
    DisplayOutcome {
        preset: preset.clone(),
        hardware: HardwareState::NotApplied {
            reason: "No panel answered on this device.".to_string(),
        },
        frames: 0,
        deduplicated: false,
        brightness_sent: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kori_core::display::{DisplayMode, LcdMetric};
    use kori_core::lighting::{Brightness, Rgb};
    use kori_hardware_linux::lcd::{self, FRAME_BYTES};
    use kori_hardware_linux::testing::{BulkRecorder, FakeKraken};
    use kori_hardware_linux::usbfs::UsbfsError;
    use std::sync::Arc;

    fn samples(first: Option<f32>, second: Option<f32>) -> [MetricSample; 2] {
        [
            MetricSample {
                metric: LcdMetric::CpuTemperature,
                value: first,
            },
            MetricSample {
                metric: LcdMetric::GpuTemperature,
                value: second,
            },
        ]
    }

    fn executor() -> (DisplayExecutor, Arc<BulkRecorder>) {
        let kraken = FakeKraken::new("2.0.4");
        let bulk = kraken.bulk_recorder();
        (
            DisplayExecutor::connected(kraken.link(), lcd::candidate_panel()),
            bulk,
        )
    }

    /// An image preset pointing at a GIF of `frames` solid colors, each
    /// declaring `delay` hundredths of a second.
    fn animated(name: &str, frames: usize, delay: u16) -> DisplayPreset {
        let directory = kori_lcd_renderer::testing::scratch(name).unwrap();
        let path = directory.join("animation.gif");
        let pictures: Vec<(Rgb, u16)> = (0..frames)
            // Distinct after the panel's five bits of red, so no two frames
            // in a row are deduplicated as the same picture.
            .map(|index| (Rgb::new((index % 32) as u8 * 8, 0x40, 0x80), delay))
            .collect();
        kori_lcd_renderer::testing::write_gif(&path, 16, &pictures).unwrap();

        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        preset.image = Some(path);
        preset
    }

    /// How many whole pictures the recorder has seen.
    ///
    /// Every frame is two sequences and every sequence is a header plus a
    /// payload, so four transfers is one picture on the glass.
    fn pictures(bulk: &BulkRecorder) -> usize {
        bulk.transfers().len() / 4
    }

    #[test]
    fn a_preset_becomes_exactly_one_frame_of_the_panels_size() {
        let (mut executor, bulk) = executor();
        let outcome = executor
            .apply(
                &DisplayPreset::default_infographic(),
                &samples(Some(50.0), Some(40.0)),
            )
            .unwrap();

        assert_eq!(outcome.frames, 1);
        assert!(!outcome.deduplicated);
        assert_eq!(outcome.hardware, HardwareState::Confirmed);
        // The first frame of a link goes out twice for the panel's buffer
        // swap, so this is two headers and two payloads, each exactly the
        // panel's frame. `frames` counts pictures, not transfers.
        let transfers = bulk.transfers();
        assert_eq!(transfers.len(), 4);
        assert_eq!(transfers[1].len(), FRAME_BYTES);
        assert_eq!(transfers[3].len(), FRAME_BYTES);
        assert_eq!(transfers[1], transfers[3], "the same picture, sent twice");
    }

    #[test]
    fn a_repeated_apply_sends_no_second_frame() {
        let (mut executor, bulk) = executor();
        let preset = DisplayPreset::default_infographic();

        executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        let bytes = bulk.bytes();

        let repeat = executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        assert!(repeat.deduplicated);
        assert_eq!(repeat.frames, 0);
        assert_eq!(repeat.hardware, HardwareState::Confirmed);
        assert_eq!(bulk.bytes(), bytes, "nothing more reached the endpoint");
    }

    #[test]
    fn a_reading_that_moved_produces_a_frame_and_one_that_did_not_does_not() {
        let (mut executor, bulk) = executor();
        let preset = DisplayPreset::default_infographic();

        executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        let bytes = bulk.bytes();

        // The same sample renders the same picture, whatever produced it.
        let unchanged = executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        assert!(unchanged.deduplicated);
        assert_eq!(bulk.bytes(), bytes);

        // A reading that moved by a degree is a different picture, and is sent.
        let moved = executor
            .apply(&preset, &samples(Some(51.0), Some(40.0)))
            .unwrap();
        assert_eq!(moved.frames, 1);
        assert!(!moved.deduplicated);
        assert!(bulk.bytes() > bytes);
    }

    #[test]
    fn a_changed_preset_is_sent_even_when_the_readings_did_not_move() {
        let (mut executor, _bulk) = executor();
        let mut preset = DisplayPreset::default_infographic();
        executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();

        preset.background = Rgb::new(0x40, 0x00, 0x00);
        let outcome = executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        assert_eq!(outcome.frames, 1);
        assert!(!outcome.deduplicated);
    }

    #[test]
    fn brightness_is_sent_once_rather_than_with_every_frame() {
        let kraken = FakeKraken::new("2.0.4");
        let reports = kraken.report_recorder();
        let mut executor = DisplayExecutor::connected(kraken.link(), lcd::candidate_panel());
        let preset = DisplayPreset::default_infographic();

        for reading in [40.0, 41.0, 42.0, 43.0] {
            executor
                .apply(&preset, &samples(Some(reading), Some(30.0)))
                .unwrap();
        }

        // Four different pictures, and exactly one display-control report: a
        // streaming preset must not resend a setting that did not change.
        assert_eq!(reports.matching(lcd::packet::DISPLAY_CONTROL).len(), 1);
        assert_eq!(executor.brightness, Some(preset.brightness));
    }

    #[test]
    fn a_brightness_only_change_reaches_the_panel_rather_than_being_deduplicated() {
        // The defect this pins: the brightness used to be written *after* the
        // frame comparison, so a preset whose only edit was the brightness
        // rendered the same pixels, deduplicated, and returned "nothing was
        // sent" while the glass stayed at the old level. It is a panel setting,
        // not a picture, and the picture must not be able to discard it.
        let kraken = FakeKraken::new("2.0.4");
        let bulk = kraken.bulk_recorder();
        let reports = kraken.report_recorder();
        let mut executor = DisplayExecutor::connected(kraken.link(), lcd::candidate_panel());

        let mut preset = DisplayPreset::default_infographic();
        let sample = samples(Some(50.0), Some(40.0));
        executor.apply(&preset, &sample).unwrap();
        let bytes = bulk.bytes();

        preset.brightness = Brightness::new(20).unwrap();
        let outcome = executor.apply(&preset, &sample).unwrap();

        assert!(outcome.deduplicated, "the picture did not change");
        assert_eq!(outcome.frames, 0);
        assert!(outcome.brightness_sent, "the panel setting did change");
        assert_eq!(
            bulk.bytes(),
            bytes,
            "an unchanged picture is still not worth a transfer"
        );

        let control = reports.matching(lcd::packet::DISPLAY_CONTROL);
        assert_eq!(control.len(), 2, "one for the first apply, one for the dim");
        assert_eq!(control[1][3], 20, "the report carries the new brightness");
    }

    #[test]
    fn a_failed_transfer_reports_uncertain_and_forgets_the_picture() {
        let (mut executor, bulk) = executor();
        let preset = DisplayPreset::default_infographic();
        executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        assert!(executor.committed().is_some());

        bulk.fail_with(UsbfsError::PermissionDenied {
            path: "/dev/bus/usb/001/004".to_string(),
        });
        let outcome = executor
            .apply(&preset, &samples(Some(60.0), Some(40.0)))
            .unwrap();

        assert_eq!(outcome.frames, 0);
        match &outcome.hardware {
            HardwareState::Uncertain { reason } => assert!(reason.contains("udev"), "{reason}"),
            other => panic!("expected uncertain, got {other:?}"),
        }
        assert_eq!(
            executor.committed(),
            None,
            "a panel that may or may not have taken the frame is not claimed"
        );
    }

    #[test]
    fn a_stopped_stream_names_itself_so_the_screen_can_offer_a_way_back() {
        let (mut executor, bulk) = executor();
        let preset = DisplayPreset::default_infographic();
        executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();
        assert!(executor.state().streaming);
        assert_eq!(executor.state().faulted, None);

        bulk.fail_with(UsbfsError::PermissionDenied {
            path: "/dev/bus/usb/001/004".to_string(),
        });
        executor.refresh(&samples(Some(60.0), Some(40.0)));

        // The reason travels, not just the stopped flag. `streaming: false` is
        // equally what a panel nobody has written to reports, so a screen with
        // only the flag could not tell a fault from an idle panel, and could
        // not know it has a recovery to offer.
        let state = executor.state();
        assert!(!state.streaming);
        let reason = state.faulted.expect("a stopped stream says why it stopped");
        assert!(reason.contains("udev"), "{reason}");

        // Only an explicit apply clears it. Nothing about the preset changed
        // when the transfer failed, so no automatic write has anything to
        // notice, which is why the screen keeps one deliberate control.
        bulk.recover();
        executor.refresh(&samples(Some(61.0), Some(40.0)));
        assert!(
            executor.state().faulted.is_some(),
            "a faulted stream must not restart itself on the next tick"
        );
        executor
            .apply(&preset, &samples(Some(62.0), Some(40.0)))
            .unwrap();
        assert_eq!(executor.state().faulted, None);
        assert!(executor.state().streaming);
    }

    #[test]
    fn a_forgotten_panel_is_written_again_rather_than_deduplicated() {
        let (mut executor, _bulk) = executor();
        let preset = DisplayPreset::default_infographic();
        let sample = samples(Some(50.0), Some(40.0));

        executor.apply(&preset, &sample).unwrap();
        executor.forget();
        let outcome = executor.apply(&preset, &sample).unwrap();
        assert!(
            !outcome.deduplicated,
            "a reconnected panel holds nothing this daemon can deduplicate against"
        );
        assert_eq!(outcome.frames, 1);
    }

    #[test]
    fn an_absent_panel_refuses_without_pretending_to_write() {
        let mut executor = DisplayExecutor::absent();
        assert_eq!(
            executor.state().panel,
            None,
            "a panel nothing answered for must not be given a geometry"
        );

        let outcome = executor
            .apply(&DisplayPreset::default_infographic(), &samples(None, None))
            .unwrap();
        assert_eq!(outcome.frames, 0);
        assert!(matches!(outcome.hardware, HardwareState::NotApplied { .. }));
        assert_eq!(executor.committed(), None);

        // And it never claims to be streaming, whatever the caller asks for.
        assert!(!executor.state().streaming);
    }

    #[test]
    fn a_preset_the_renderer_refuses_never_reaches_the_endpoint() {
        let (mut executor, bulk) = executor();
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;

        assert_eq!(
            executor.apply(&preset, &samples(Some(50.0), Some(40.0))),
            Err(DisplayError::ImagePathMissing)
        );
        assert!(
            bulk.transfers().is_empty(),
            "a refused preset must not put a byte on the endpoint"
        );
        assert_eq!(executor.committed(), None);
    }

    #[test]
    fn dropped_frames_are_counted_rather_than_queued() {
        let (mut executor, _bulk) = executor();
        assert_eq!(executor.state().dropped_frames, 0);
        executor.drop_frame();
        executor.drop_frame();
        assert_eq!(executor.state().dropped_frames, 2);
    }

    #[test]
    fn the_reported_state_carries_the_panel_and_what_it_was_told_to_show() {
        let (mut executor, _bulk) = executor();
        let preset = DisplayPreset::default_infographic();
        executor
            .apply(&preset, &samples(Some(50.0), Some(40.0)))
            .unwrap();

        let state = executor.state();
        assert_eq!(state.panel.map(|panel| panel.width), Some(240));
        assert_eq!(state.committed.as_ref(), Some(&preset));
        assert!(state.streaming);
    }

    #[test]
    fn applying_an_animation_puts_its_first_frame_on_the_glass_and_starts_a_clock() {
        let (mut executor, bulk) = executor();
        let preset = animated("apply", 3, 10);
        let outcome = executor.apply(&preset, &samples(None, None)).unwrap();

        assert_eq!(outcome.frames, 1, "the first picture goes out at once");
        assert_eq!(pictures(&bulk), 1);
        assert!(
            executor.state().streaming,
            "an animation is the panel moving on its own, which is what streaming means"
        );
        assert!(
            executor.advance_animation(Instant::now()).is_some(),
            "the clock is running"
        );
    }

    #[test]
    fn a_still_picture_installs_no_clock_at_all() {
        // One frame is an ordinary frame. Nothing has to wake up for it, which
        // is what keeps a wallpaper from costing what an animation costs.
        let (mut executor, bulk) = executor();
        let preset = animated("still", 1, 10);
        executor.apply(&preset, &samples(None, None)).unwrap();

        assert_eq!(pictures(&bulk), 1);
        assert_eq!(
            executor.advance_animation(Instant::now() + Duration::from_secs(10)),
            None,
            "a still picture has no next frame to be due"
        );
        assert_eq!(pictures(&bulk), 1);
    }

    #[test]
    fn an_animation_frame_is_sent_when_it_is_due_and_not_before() {
        let (mut executor, bulk) = executor();
        // Ten hundredths is 100 ms, above the transport floor, so the delay
        // survives the clamp and this test is about the clock rather than
        // about the clamp.
        let preset = animated("cadence", 4, 10);
        executor.apply(&preset, &samples(None, None)).unwrap();
        assert_eq!(pictures(&bulk), 1);

        // Every instant here comes from the executor's own answer rather than
        // from a clock this test read earlier, so the assertions do not depend
        // on how long the compile took.
        let due = executor
            .advance_animation(Instant::now())
            .expect("the animation is still playing");
        assert_eq!(pictures(&bulk), 1, "nothing was due yet");

        executor.advance_animation(due);
        assert_eq!(pictures(&bulk), 2, "the second picture landed on its due");
        assert_eq!(executor.state().dropped_frames, 0);
    }

    #[test]
    fn a_late_tick_skips_the_frames_it_slept_through_rather_than_playing_them_all() {
        // The same rule a late telemetry frame follows, for the same
        // reason: a backlog of old pictures has nothing to offer a panel. What
        // it does not do is play the animation in slow motion, which is what a
        // cursor advancing one step per tick would produce.
        let (mut executor, bulk) = executor();
        let preset = animated("late", 8, 10);
        executor.apply(&preset, &samples(None, None)).unwrap();
        let due = executor
            .advance_animation(Instant::now())
            .expect("the animation is playing");

        // Four hundred milliseconds past the first due, on a hundred
        // millisecond cadence: five frames came due while nothing was looking.
        executor.advance_animation(due + Duration::from_millis(400));
        assert_eq!(
            pictures(&bulk),
            2,
            "one picture went out, not the five that came due"
        );
        assert_eq!(
            executor.state().dropped_frames,
            4,
            "the frames that were skipped are counted rather than hidden"
        );
    }

    #[test]
    fn an_animation_returns_to_its_first_frame_rather_than_stopping_at_the_last() {
        let (mut executor, bulk) = executor();
        let preset = animated("loop", 2, 10);
        executor.apply(&preset, &samples(None, None)).unwrap();

        // Two frames, so the third picture is the first one again. It is sent
        // rather than deduplicated only because the second one displaced it.
        let mut due = executor
            .advance_animation(Instant::now())
            .expect("the animation is playing");
        for _ in 0..2 {
            due = executor.advance_animation(due).expect("still playing");
        }
        assert_eq!(pictures(&bulk), 3);
        assert!(executor.state().streaming);
    }

    #[test]
    fn a_failed_transfer_stops_the_animation_instead_of_pushing_frames_at_a_refusing_panel() {
        let kraken = FakeKraken::new("2.0.4");
        let bulk = kraken.bulk_recorder();
        let mut executor = DisplayExecutor::connected(kraken.link(), lcd::candidate_panel());
        let preset = animated("faulted", 4, 10);
        executor.apply(&preset, &samples(None, None)).unwrap();
        let due = executor
            .advance_animation(Instant::now())
            .expect("the animation is playing");
        let sent = pictures(&bulk);

        bulk.fail_with(UsbfsError::PermissionDenied {
            path: "/dev/bus/usb/001/004".to_string(),
        });
        assert_eq!(
            executor.advance_animation(due),
            None,
            "a failed transfer stops the clock rather than returning a next due"
        );

        let state = executor.state();
        assert!(!state.streaming);
        assert!(state.faulted.is_some(), "the stop names itself");
        assert_eq!(
            executor.advance_animation(due + Duration::from_millis(400)),
            None,
            "and a later tick does not restart it by itself"
        );
        assert_eq!(
            pictures(&bulk),
            sent,
            "no picture was completed after the refusal"
        );
    }

    #[test]
    fn the_telemetry_tick_leaves_an_animation_to_its_own_clock() {
        // Both cadences run on one thread, and the second one has nothing to
        // contribute to a picture that reads no telemetry. Without this the
        // once-a-second redraw would push frame zero back onto the glass in
        // the middle of the animation.
        let (mut executor, bulk) = executor();
        let preset = animated("tick", 3, 10);
        executor.apply(&preset, &samples(None, None)).unwrap();
        let sent = pictures(&bulk);

        assert_eq!(executor.refresh(&samples(Some(61.0), Some(48.0))), None);
        assert_eq!(pictures(&bulk), sent, "the telemetry tick sent nothing");
    }

    #[test]
    fn a_new_preset_drops_whatever_was_playing() {
        let (mut executor, _bulk) = executor();
        executor
            .apply(&animated("replaced", 4, 10), &samples(None, None))
            .unwrap();
        assert!(executor.advance_animation(Instant::now()).is_some());

        executor
            .apply(
                &DisplayPreset::default_infographic(),
                &samples(Some(50.0), Some(40.0)),
            )
            .unwrap();
        assert_eq!(
            executor.advance_animation(Instant::now() + Duration::from_secs(1)),
            None,
            "the animation belonged to the preset that was replaced"
        );
    }

    #[test]
    fn a_picture_that_cannot_be_decoded_leaves_no_animation_behind() {
        // A file that fails must not leave the previous operator's animation
        // running under a preset that no longer names it.
        let (mut executor, _bulk) = executor();
        executor
            .apply(&animated("kept", 4, 10), &samples(None, None))
            .unwrap();

        let mut broken = DisplayPreset::default_infographic();
        broken.mode = DisplayMode::Image;
        broken.image = Some(std::path::PathBuf::from("/nonexistent/kori/absent.gif"));
        assert!(executor.apply(&broken, &samples(None, None)).is_err());
        assert_eq!(
            executor.advance_animation(Instant::now() + Duration::from_secs(1)),
            None
        );
    }
}
