// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The frame write probe: the only code that sends an unvalidated frame.
//!
//! It mirrors [`crate::rgb_probe`] and exists for the same reason. The transfer
//! sequence is adapted from a working implementation, which is evidence that it
//! works on *some* Kraken and no evidence at all about this one. So the frames
//! go out one at a time, under a typed confirmation, with an operator watching
//! the glass, and every byte that left the process is recorded.
//!
//! Three things separate this from a production write.
//!
//! * It runs against a firmware [`kori_hardware_linux::lcd::VALIDATED_FIRMWARE`]
//!   does not list. That list is what this probe exists to fill.
//! * It asks the operator what the panel was showing first and what it shows
//!   after, because no report on this device reads the panel back. A frame is
//!   confirmed by a person or it is not confirmed.
//! * It restores what it can and says plainly whether the restoration was seen.

use kori_core::capability::LcdTopology;
use kori_core::display::{DisplayPreset, MetricSample, Orientation};
use kori_core::lighting::{Brightness, Rgb};
use kori_hardware_linux::lcd::LcdLink;
use serde::{Deserialize, Serialize};

use crate::probe::{CONFIRMATION_PHRASE, Operator, authorized};

/// Colors the probe fills the panel with, in order.
///
/// Three primaries and a neutral. Together they answer the two questions a
/// solid frame can answer: whether the panel lit at all, and whether the pixel
/// format puts the channels where this product thinks it does. A panel showing
/// blue when the probe sent red is a byte-order fault, and it is visible
/// without an instrument.
pub const PROBE_COLORS: [(&str, Rgb); 4] = [
    ("red", Rgb::new(0xff, 0x00, 0x00)),
    ("green", Rgb::new(0x00, 0xff, 0x00)),
    ("blue", Rgb::new(0x00, 0x00, 0xff)),
    ("white", Rgb::new(0xff, 0xff, 0xff)),
];

/// Brightness every probe frame is sent at, as a percentage.
///
/// Half, so a panel that has never been driven by this product is not asked to
/// go to full output on its first frame. Deliberately not the lighting probe's
/// level: a strip at 10% is still unmistakable in a case, and a panel at 10% is
/// not something an operator can read across a desk, which is what this run
/// asks them to do.
const PROBE_PERCENT: u8 = 50;

/// The same level, checked by the build.
///
/// The record names the percentage every frame was sent at, so a constant that
/// silently fell back to off would produce evidence for a frame nothing ever
/// displayed.
const PROBE_LEVEL: Brightness = Brightness::from_percent(PROBE_PERCENT);

/// One frame the probe sent, with everything observable about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeStep {
    /// What this step was trying to show.
    pub description: String,
    /// The HID report that opened the transfer.
    pub transfer_start: Vec<u8>,
    /// The bulk header, magic and length.
    pub bulk_header: Vec<u8>,
    /// Size of the pixel payload, in bytes. The pixels themselves are not
    /// recorded: a record is meant to be read, and a length proves as much.
    pub payload_bytes: usize,
    /// The HID report that closed the transfer.
    pub transfer_end: Vec<u8>,
    /// How many times the sequence went out: twice per frame, for the panel's
    /// buffer swap.
    pub sequences: u8,
    /// How many of the `0x36` commands the panel answered, two per sequence.
    ///
    /// Recorded because a transfer that reports success proves only that the
    /// `ioctl` returned. A frame the panel never acknowledged is the shape of
    /// the picture photographed half painted on 2026-08-08, so the count is
    /// evidence the record would otherwise be missing.
    pub acknowledged: u8,
    /// How long the whole transfer took.
    pub elapsed_us: u128,
    /// What the operator saw, verbatim. Empty when nobody was asked.
    pub observed: String,
    /// Set when the transfer failed.
    pub error: Option<String>,
}

/// Whether the panel was put back the way it was found.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Restoration {
    /// The operator saw the prior state return.
    Confirmed { observed: String },
    /// Restoration ran but nobody confirmed the result.
    Unconfirmed { reason: String },
    /// Restoration could not be attempted.
    Failed { reason: String },
}

/// Everything one run recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteProbeReport {
    /// Firmware the Kraken reported, which is what the validated list keys on.
    pub firmware: String,
    /// Geometry every frame was laid out at.
    pub panel: String,
    /// Where the reports and the frames went.
    pub transport: String,
    /// What the operator said the panel was showing before the first frame.
    pub prior_state: String,
    pub steps: Vec<ProbeStep>,
    pub restoration: Restoration,
}

impl WriteProbeReport {
    /// True when every frame the probe sent left the process without error.
    pub fn every_step_landed(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|step| step.error.is_none())
    }

    /// True when the operator confirmed every frame they were asked about.
    pub fn every_step_observed(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|step| !step.observed.is_empty())
    }
}

/// Why a run refused to start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeRefusal {
    #[error("the device did not answer its display report: {reason}")]
    NoPanel { reason: String },
    #[error("the panel geometry is not recorded, so no frame can be laid out")]
    NoGeometry,
    #[error(
        "the write probe was not authorized. Re-run it and type {CONFIRMATION_PHRASE} when asked."
    )]
    NotAuthorized,
}

/// Run the write probe against a link the caller already opened.
///
/// The caller owns opening the nodes and holding the device lock, so this is
/// the same code whether it drives real hardware or a Kraken that answers the
/// same reports.
pub fn run<O: Operator>(
    link: &mut LcdLink,
    operator: &mut O,
    topology: &LcdTopology,
) -> Result<WriteProbeReport, ProbeRefusal> {
    let Some(panel) = topology.panel.value().cloned() else {
        return Err(ProbeRefusal::NoGeometry);
    };
    if !topology.answered() {
        return Err(ProbeRefusal::NoPanel {
            reason: topology
                .display
                .reason()
                .unwrap_or("no reason was recorded")
                .to_string(),
        });
    }
    let firmware = topology
        .firmware
        .value()
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());

    let frames = PROBE_COLORS.len();
    let (width, height, format) = (panel.width, panel.height, &panel.pixel_format);
    operator.note(&format!(
        "About to send {frames} unvalidated frames to a panel running firmware \
         {firmware}.\n\
         Each is one solid color at {PROBE_PERCENT}% brightness, laid out at \
         {width}x{height} in {format}.\n\
         Nothing else is sent: no orientation change, no image, no animation.\n\
         Watch the panel while this runs.",
    ));

    // The prior state is captured before authorization, so a run that is
    // refused still leaves a record of what the panel was showing.
    let prior_state = operator.ask("What is the panel showing right now? (free text)");

    if !authorized(operator) {
        return Err(ProbeRefusal::NotAuthorized);
    }

    let mut steps = Vec::new();

    for (name, color) in PROBE_COLORS {
        let mut preset = DisplayPreset::solid(color);
        preset.brightness = PROBE_LEVEL;
        // The panel is left on its own orientation, so nothing this run sends
        // can leave a rotation behind.
        preset.orientation = Orientation::Deg0;

        steps.push(send_step(
            link,
            operator,
            &preset,
            &panel,
            &format!("solid {name} at {PROBE_PERCENT}%"),
            &format!("What does the panel show now? (expected: a {name} field)"),
        ));
    }

    let restoration = restore(link, operator, &panel, &prior_state);

    Ok(WriteProbeReport {
        firmware,
        panel: format!(
            "{}x{} {} {}",
            panel.width,
            panel.height,
            panel.pixel_format,
            match panel.shape {
                kori_core::capability::LcdPanelShape::Circular => "circular",
                kori_core::capability::LcdPanelShape::Square => "square",
            }
        ),
        transport: link.source(),
        prior_state,
        steps,
        restoration,
    })
}

/// Render one preset, send it, and record what left the process.
fn send_step<O: Operator>(
    link: &mut LcdLink,
    operator: &mut O,
    preset: &DisplayPreset,
    panel: &kori_core::capability::LcdPanel,
    description: &str,
    question: &str,
) -> ProbeStep {
    operator.note(&format!("Sending {description}."));

    // A solid field reads no telemetry, so the samples are unavailable ones:
    // nothing this probe sends can depend on a collector.
    let samples = [
        MetricSample::unavailable(kori_core::display::LcdMetric::CpuTemperature),
        MetricSample::unavailable(kori_core::display::LcdMetric::GpuTemperature),
    ];

    let frame = match kori_lcd_renderer::render(preset, &samples, panel) {
        Ok(frame) => frame.to_rgb565_be(),
        Err(error) => {
            return ProbeStep {
                description: description.to_string(),
                transfer_start: Vec::new(),
                bulk_header: Vec::new(),
                payload_bytes: 0,
                transfer_end: Vec::new(),
                sequences: 0,
                acknowledged: 0,
                elapsed_us: 0,
                observed: String::new(),
                error: Some(error.to_string()),
            };
        }
    };

    let started = std::time::Instant::now();
    let sent = link
        .set_display(preset.brightness)
        .and_then(|_| link.send_frame(&frame));
    let elapsed_us = started.elapsed().as_micros();

    match sent {
        Ok(report) => ProbeStep {
            description: description.to_string(),
            transfer_start: report.start.to_vec(),
            bulk_header: report.header.to_vec(),
            payload_bytes: report.payload_bytes,
            transfer_end: report.end.to_vec(),
            sequences: report.sequences,
            acknowledged: report.acknowledged,
            elapsed_us,
            observed: operator.ask(question),
            error: None,
        },
        Err(error) => ProbeStep {
            description: description.to_string(),
            transfer_start: Vec::new(),
            bulk_header: Vec::new(),
            payload_bytes: frame.len(),
            transfer_end: Vec::new(),
            sequences: 0,
            acknowledged: 0,
            elapsed_us,
            observed: String::new(),
            error: Some(error.to_string()),
        },
    }
}

/// Put the panel back, and say honestly whether anyone saw it happen.
///
/// No report on this device reads the panel back, so the prior picture cannot
/// be reproduced from anything this product recorded. What restoration means
/// here is the product's own default view, which is a state the operator can
/// recognize, followed by the question of whether that is what they see.
fn restore<O: Operator>(
    link: &mut LcdLink,
    operator: &mut O,
    panel: &kori_core::capability::LcdPanel,
    prior_state: &str,
) -> Restoration {
    operator.note("Restoring the panel to this product's default view.");

    let preset = DisplayPreset::default_infographic();
    let samples = [
        MetricSample::unavailable(kori_core::display::LcdMetric::CpuTemperature),
        MetricSample::unavailable(kori_core::display::LcdMetric::GpuTemperature),
    ];

    let frame = match kori_lcd_renderer::render(&preset, &samples, panel) {
        Ok(frame) => frame.to_rgb565_be(),
        Err(error) => {
            return Restoration::Failed {
                reason: error.to_string(),
            };
        }
    };
    if let Err(error) = link
        .set_display(preset.brightness)
        .and_then(|_| link.send_frame(&frame))
    {
        return Restoration::Failed {
            reason: error.to_string(),
        };
    }

    let observed = operator.ask(&format!(
        "The panel was showing {prior_state:?} before this run. What does it show now?"
    ));
    if observed.is_empty() {
        return Restoration::Unconfirmed {
            reason: "nobody described the panel after restoration".to_string(),
        };
    }
    Restoration::Confirmed { observed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::testing::Script;
    use kori_core::capability::Evidenced;
    use kori_hardware_linux::lcd;
    use kori_hardware_linux::testing::{BulkRecorder, FakeKraken};
    use kori_hardware_linux::usbfs::UsbfsError;
    use std::sync::Arc;

    fn topology(firmware: &str) -> LcdTopology {
        LcdTopology {
            hid_node: Evidenced::known("/dev/hidraw10".into(), "sysfs"),
            bulk_node: Evidenced::known("/dev/bus/usb/001/004".into(), "sysfs"),
            firmware: Evidenced::known(firmware.into(), "report 0x11 0x01"),
            panel: Evidenced::known(lcd::candidate_panel(), "candidate"),
            display: Evidenced::known(
                kori_core::capability::LcdDisplaySettings {
                    brightness_percent: 60,
                    quarter_turns: 0,
                },
                "report 0x31 0x01",
            ),
        }
    }

    fn probe(
        answers: &[&str],
    ) -> (
        Result<WriteProbeReport, ProbeRefusal>,
        Script,
        Arc<BulkRecorder>,
    ) {
        let kraken = FakeKraken::new("2.0.4");
        let bulk = kraken.bulk_recorder();
        let mut link = kraken.link();
        let mut operator = Script::new(answers);
        let report = run(&mut link, &mut operator, &topology("2.0.4"));
        (report, operator, bulk)
    }

    /// The answers a full authorized run consumes.
    fn full_run() -> Vec<&'static str> {
        let mut answers = vec!["the default NZXT logo", CONFIRMATION_PHRASE];
        answers.extend([
            "a red field",
            "a green field",
            "a blue field",
            "a white field",
        ]);
        answers.push("the product's own dial, with dashes");
        answers
    }

    #[test]
    fn nothing_is_sent_without_the_typed_confirmation() {
        let (report, operator, bulk) = probe(&["a logo", "yes please"]);
        assert_eq!(report.unwrap_err(), ProbeRefusal::NotAuthorized);
        assert!(
            bulk.transfers().is_empty(),
            "an unauthorized run must not put a byte on the endpoint"
        );
        // The prior state was still asked for, before the refusal, so an
        // aborted run leaves a record of what the panel was showing.
        assert!(
            operator.asked[0].contains("showing right now"),
            "{:?}",
            operator.asked
        );
    }

    #[test]
    fn a_device_that_never_answered_its_panel_refuses_before_the_question() {
        let kraken = FakeKraken::without_panel("2.0.4");
        let bulk = kraken.bulk_recorder();
        let mut link = kraken.link();
        let mut operator = Script::new(&full_run());

        let mut silent = topology("2.0.4");
        silent.display = Evidenced::unknown("the device sent no display answer", "report 0x31");
        match run(&mut link, &mut operator, &silent) {
            Err(ProbeRefusal::NoPanel { reason }) => {
                assert!(reason.contains("no display"), "{reason}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(
            operator.asked.is_empty(),
            "a device with no panel is refused before the operator is troubled"
        );
        assert!(bulk.transfers().is_empty());
    }

    #[test]
    fn an_unknown_geometry_refuses_rather_than_guessing_one() {
        let kraken = FakeKraken::new("2.0.4");
        let mut link = kraken.link();
        let mut operator = Script::new(&full_run());

        let mut unknown = topology("2.0.4");
        unknown.panel = Evidenced::unknown("no resolution was recorded", "candidate");
        assert_eq!(
            run(&mut link, &mut operator, &unknown).unwrap_err(),
            ProbeRefusal::NoGeometry
        );
    }

    #[test]
    fn an_authorized_run_sends_one_frame_per_color_and_records_every_byte() {
        let (report, _operator, bulk) = probe(&full_run());
        let report = report.unwrap();

        assert_eq!(report.steps.len(), PROBE_COLORS.len());
        assert!(report.every_step_landed());
        assert!(report.every_step_observed());
        assert_eq!(report.firmware, "2.0.4");
        assert!(report.panel.contains("240x240"), "{}", report.panel);

        for step in &report.steps {
            assert_eq!(&step.transfer_start[0..2], &[0x36, 0x01]);
            assert_eq!(&step.transfer_end[0..2], &[0x36, 0x02]);
            assert_eq!(step.bulk_header.len(), 20);
            assert_eq!(&step.bulk_header[..12], &lcd::packet::BULK_MAGIC);
            assert_eq!(step.payload_bytes, lcd::FRAME_BYTES);
        }

        // Four probe frames plus the restoration, each sent twice for the
        // panel's buffer swap, and each sequence a header and a payload.
        assert!(report.steps.iter().all(|step| step.sequences == 2));
        assert_eq!(bulk.transfers().len(), (PROBE_COLORS.len() + 1) * 2 * 2);

        // A panel that answers every transfer command acknowledges four of
        // them per frame, and the record carries that count: a step showing
        // fewer is a panel the pixels may have outrun.
        assert!(report.steps.iter().all(|step| step.acknowledged == 4));
    }

    #[test]
    fn every_color_the_probe_sends_reaches_the_panel_as_a_different_frame() {
        let (report, _operator, bulk) = probe(&full_run());
        report.unwrap();

        // Payloads only, so the identical headers do not mask a color that
        // never changed.
        let frames: Vec<Vec<u8>> = bulk
            .transfers()
            .into_iter()
            .filter(|transfer| transfer.len() == lcd::FRAME_BYTES)
            .collect();
        // Four colors plus the restoration, every one of them sent twice for
        // the panel's buffer swap, so the pairs are what carry the colors.
        assert_eq!(frames.len(), (PROBE_COLORS.len() + 1) * 2);
        let pictures: Vec<&Vec<u8>> = frames
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair[0], pair[1], "a picture goes out twice, identically");
                &pair[0]
            })
            .collect();

        // Every distinct picture is distinct: a step that re-sent the previous
        // one would prove nothing about the color it claims to test.
        let unique: std::collections::HashSet<&&Vec<u8>> = pictures.iter().collect();
        assert_eq!(
            unique.len(),
            pictures.len(),
            "two probe steps sent the same picture, so one of them proves nothing"
        );

        // Red is the top five bits of the format.
        assert_eq!(&pictures[0][0..2], &[0xf8, 0x00]);
        assert_eq!(&pictures[1][0..2], &[0x07, 0xe0], "green");
        assert_eq!(&pictures[2][0..2], &[0x00, 0x1f], "blue");
    }

    #[test]
    fn the_operator_authorizes_the_scope_the_run_actually_sends() {
        let (report, operator, _bulk) = probe(&full_run());
        report.unwrap();

        let prompt = operator.noted.first().cloned().unwrap_or_default();
        assert!(prompt.contains(&PROBE_COLORS.len().to_string()), "{prompt}");
        assert!(prompt.contains("240x240"), "{prompt}");
        assert!(prompt.contains("no image"), "{prompt}");
        assert!(prompt.contains("no animation"), "{prompt}");
    }

    #[test]
    fn a_restoration_nobody_described_is_never_reported_as_confirmed() {
        let mut answers = vec!["a logo", CONFIRMATION_PHRASE];
        answers.extend(["red", "green", "blue", "white"]);
        // Nothing for the restoration question.
        let (report, _operator, _bulk) = probe(&answers);
        let report = report.unwrap();
        match report.restoration {
            Restoration::Unconfirmed { reason } => assert!(reason.contains("nobody"), "{reason}"),
            other => panic!("expected an unconfirmed restoration, got {other:?}"),
        }
    }

    #[test]
    fn a_confirmed_restoration_carries_what_the_operator_saw() {
        let (report, _operator, _bulk) = probe(&full_run());
        match report.unwrap().restoration {
            Restoration::Confirmed { observed } => {
                assert_eq!(observed, "the product's own dial, with dashes")
            }
            other => panic!("expected a confirmed restoration, got {other:?}"),
        }
    }

    #[test]
    fn a_panel_that_refuses_every_frame_is_reported_rather_than_retried() {
        let kraken = FakeKraken::new("2.0.4");
        let bulk = kraken.bulk_recorder();
        bulk.fail_with(UsbfsError::PermissionDenied {
            path: "/dev/bus/usb/001/004".to_string(),
        });
        let mut link = kraken.link();
        let mut operator = Script::new(&full_run());

        let report = run(&mut link, &mut operator, &topology("2.0.4")).unwrap();
        assert_eq!(report.steps.len(), PROBE_COLORS.len());
        assert!(!report.every_step_landed());
        for step in &report.steps {
            assert!(
                step.error.as_deref().is_some_and(|e| e.contains("udev")),
                "{step:?}"
            );
            assert!(
                step.observed.is_empty(),
                "a frame that never landed must not carry an observation"
            );
        }
        assert!(matches!(report.restoration, Restoration::Failed { .. }));
    }

    #[test]
    fn every_field_the_probe_sends_is_one_the_operator_can_tell_apart() {
        // The frames are what the operator watches for, and they now carry
        // nothing but their own color, so the colors themselves have to be
        // distinct and none of them may be the background of another.
        let mut seen = Vec::new();
        for (name, color) in PROBE_COLORS {
            assert!(
                !seen.contains(&color),
                "{name} repeats a color already sent"
            );
            seen.push(color);
        }
        assert!(seen.len() > 1, "one field proves nothing changed");
    }
}
