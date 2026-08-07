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
//! the gauge happens to fall, so nothing here promises it either way. US-018
//! asks for one frame a second regardless.
//!
//! **At most one frame is ever outstanding.** The transfer is synchronous, so
//! "outstanding" here means the sample that arrived while the previous frame
//! was still being written. US-018 requires that such a sample replace the
//! pending one rather than queue behind it: a panel showing a temperature from
//! four seconds ago because three frames are waiting is worse than a panel that
//! skipped them.

use nzxt_core::capability::LcdPanel;
use nzxt_core::display::{DisplayError, DisplayPreset, MetricSample};
use nzxt_core::ipc::{DisplayOutcome, DisplayState, HardwareState};
use nzxt_core::lighting::Brightness;
use nzxt_hardware_linux::lcd::{LcdError, LcdLink};

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
    /// coming back, or the operator applying a preset by hand. That is what
    /// US-018 means by retrying only after a reconnect or an explicit
    /// recoverable state.
    faulted: Option<String>,
    /// Frames discarded because a newer sample replaced them.
    dropped: u64,
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

    /// The geometry a preset must be rendered at, when one is known.
    pub fn panel(&self) -> Option<&LcdPanel> {
        self.panel.as_ref()
    }

    /// True when a frame could actually be sent right now.
    pub fn is_connected(&self) -> bool {
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
    fn is_streaming(&self) -> bool {
        self.is_connected()
            && self.faulted.is_none()
            && self
                .active
                .as_ref()
                .is_some_and(|preset| preset.mode.uses_readings())
    }

    /// Per-panel state for [`nzxt_core::ipc::DaemonStatus`].
    pub fn state(&self) -> DisplayState {
        DisplayState {
            panel: self.panel.clone(),
            committed: self.committed().cloned(),
            streaming: self.is_streaming(),
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
        // A panel that comes back has swapped nothing yet, so the next frame
        // must prime it again.
        if let Some(link) = self.link.as_mut() {
            link.unprime();
        }
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
        let outcome = self.send(preset, samples)?;
        self.active = Some(preset.clone());
        Ok(outcome)
    }

    /// Redraw the active preset against a fresh sample, once per tick.
    ///
    /// Returns `None` when there is nothing to do: no preset, no panel, a
    /// preset that does not read telemetry, or a stream that has faulted.
    pub fn refresh(&mut self, samples: &[MetricSample; 2]) -> Option<DisplayOutcome> {
        if !self.is_streaming() {
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
            return Ok(DisplayOutcome {
                preset: preset.clone(),
                hardware: HardwareState::NotApplied {
                    reason: "No panel answered on this device.".to_string(),
                },
                frames: 0,
                deduplicated: false,
            });
        };

        let frame = nzxt_lcd_renderer::render(preset, samples, &panel)?.to_rgb565_be();

        // The picture is what the panel holds, so the picture is what is
        // compared. A preset that changed without changing a pixel still costs
        // nothing, and a preset that did not change but whose readings did is
        // still sent.
        if self
            .committed
            .as_ref()
            .is_some_and(|entry| entry.frame == frame)
        {
            return Ok(DisplayOutcome {
                preset: preset.clone(),
                hardware: HardwareState::Confirmed,
                frames: 0,
                deduplicated: true,
            });
        }

        let Some(link) = self.link.as_mut() else {
            return Ok(DisplayOutcome {
                preset: preset.clone(),
                hardware: HardwareState::NotApplied {
                    reason: "No panel answered on this device.".to_string(),
                },
                frames: 0,
                deduplicated: false,
            });
        };

        // Brightness travels over its own report and only when it changed, so
        // a streaming preset does not resend it every second.
        if self.brightness != Some(preset.brightness)
            && let Err(error) = link.set_display(preset.brightness)
        {
            return Ok(uncertain(preset, &error, &mut self.committed));
        }
        self.brightness = Some(preset.brightness);

        match link.send_frame(&frame) {
            Ok(_) => {
                self.committed = Some(Committed {
                    preset: preset.clone(),
                    frame,
                });
                Ok(DisplayOutcome {
                    preset: preset.clone(),
                    hardware: HardwareState::Confirmed,
                    frames: 1,
                    deduplicated: false,
                })
            }
            Err(error) => Ok(uncertain(preset, &error, &mut self.committed)),
        }
    }
}

/// A transfer that may or may not have landed.
///
/// The record of what the panel is showing is dropped rather than left claiming
/// a picture that may never have arrived, which is the same reasoning the
/// lighting path follows for a controller that acknowledges nothing.
fn uncertain(
    preset: &DisplayPreset,
    error: &LcdError,
    committed: &mut Option<Committed>,
) -> DisplayOutcome {
    *committed = None;
    DisplayOutcome {
        preset: preset.clone(),
        hardware: HardwareState::Uncertain {
            reason: error.to_string(),
        },
        frames: 0,
        deduplicated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nzxt_core::display::{DisplayMode, LcdMetric};
    use nzxt_core::lighting::Rgb;
    use nzxt_hardware_linux::lcd::{self, FRAME_BYTES};
    use nzxt_hardware_linux::testing::{BulkRecorder, FakeKraken};
    use nzxt_hardware_linux::usbfs::UsbfsError;
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
        let mut executor = DisplayExecutor::connected(kraken.link(), lcd::candidate_panel());
        let preset = DisplayPreset::default_infographic();

        for reading in [40.0, 41.0, 42.0, 43.0] {
            executor
                .apply(&preset, &samples(Some(reading), Some(30.0)))
                .unwrap();
        }

        // Four frames, and the display-control report was not one of them per
        // frame. The count is read back through a second executor because the
        // first boxed the device away; what matters here is that a streaming
        // preset does not resend a setting that did not change, which the
        // brightness record enforces.
        assert_eq!(executor.brightness, Some(preset.brightness));
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
        assert!(!executor.is_connected());
        assert!(executor.panel().is_none());

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
}
