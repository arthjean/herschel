// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the Kraken's panel is asked to show, as a typed description.
//!
//! A [`DisplayPreset`] is a description, not pixels. It carries no resolution,
//! no pixel format and no protocol byte, so the same value can be rendered into
//! the GPUI preview and into the framebuffer the daemon sends, so the editor
//! previews the exact bytes the panel receives. The rendering itself lives in
//! `kori-lcd-renderer`, and both processes call it.
//!
//! Nothing here reads a file or touches a device. A preset naming a static
//! image carries its path and nothing more: the file is opened by the renderer,
//! at the moment a frame is produced, so an unreadable file is a typed render
//! failure rather than a preset that silently means something else.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::lighting::{Brightness, LightingError, Rgb};
use crate::telemetry::TelemetrySnapshot;

/// How often the daemon redraws a preset that reads telemetry.
///
/// Exactly one frame per second, which is the rate the collectors sample at:
/// a faster panel would repeat readings, and a slower one would show stale
/// ones.
pub const FRAME_INTERVAL_MS: u64 = 1_000;

/// Widest image this product will decode, per side.
///
/// The ceiling is applied to the dimensions the file declares, before any pixel
/// buffer is allocated, so an image claiming an enormous size is refused rather
/// than resized.
pub const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Longest path a preset may carry, so a peer cannot grow a frame without end.
const MAX_IMAGE_PATH_BYTES: usize = 4096;

/// Most frames this product will hold in memory for one animation.
///
/// A GIF header declares neither a frame count nor a duration, so the ceiling
/// cannot be checked before decoding the way [`MAX_IMAGE_DIMENSION`] is: the
/// decoder is stopped at this frame instead, and the file is refused rather than
/// truncated into an animation that silently drops its tail.
///
/// The number is a memory budget. Each frame occupies the panel's own
/// framebuffer twice over while it is being compiled: 240x240 at three bytes per
/// pixel inside the renderer, 240x240 at two bytes per pixel in the daemon's
/// table. At 120 frames that is 20.7 MiB transient and 13.8 MiB resident, which
/// is what a few seconds of animation costs.
pub const MAX_ANIMATION_FRAMES: usize = 120;

/// Shortest a frame may stay on the glass, in milliseconds.
///
/// Measured, not chosen: one picture costs two transfer sequences and four
/// acknowledgments, timed at 79 to 80 ms end to end on the owned panel. A GIF asking
/// for less would not be played faster, it would be played late and forever
/// further behind, so the delay is raised to what the transport can hold.
pub const MIN_FRAME_DELAY_MS: u64 = 80;

/// What a frame declaring no delay at all is given.
///
/// GIF writers emit a zero delay to mean "as fast as possible", which every
/// viewer resolves to a tenth of a second rather than to a busy loop. This
/// product does the same, one step above [`MIN_FRAME_DELAY_MS`] so an
/// unspecified cadence is not also the fastest one the hardware allows.
pub const DEFAULT_FRAME_DELAY_MS: u64 = 100;

/// What kind of picture the panel shows.
///
/// Four entries, each of which something in this product actually produces: the
/// infographic the daemon streams, the single reading that gives one metric the
/// whole dial, the solid field the transport probe sends, and a static image
/// the operator picks. Nothing is listed that the renderer cannot draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    /// Two metrics, each with an arc, a value and a label.
    DualInfographic,
    /// One metric, on one arc, at the largest size the panel can hold.
    SingleReading,
    /// The background color across the whole panel, and nothing else.
    Solid,
    /// A picture the operator chose, scaled to the panel.
    ///
    /// One mode rather than two. A GIF carrying more than one frame is played,
    /// a GIF carrying one and a PNG or JPEG are held still, and the difference
    /// is a property of the file rather than a choice the operator has to
    /// restate in a select.
    Image,
}

impl DisplayMode {
    pub const ALL: [Self; 4] = [
        Self::DualInfographic,
        Self::SingleReading,
        Self::Solid,
        Self::Image,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::DualInfographic => "Dual infographic",
            Self::SingleReading => "Single reading",
            Self::Solid => "Solid color",
            Self::Image => "Image or GIF",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::DualInfographic => "dual_infographic",
            Self::SingleReading => "single_reading",
            Self::Solid => "solid",
            Self::Image => "image",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }

    /// How many of the two reading slots this mode actually draws.
    ///
    /// The count rather than a flag: the editor hides the slots a mode never
    /// draws, and a mode that draws one of the two would otherwise have to be
    /// special-cased at every place that asks.
    pub fn reading_slots(self) -> usize {
        match self {
            Self::DualInfographic => 2,
            Self::SingleReading => 1,
            Self::Solid | Self::Image => 0,
        }
    }

    /// Whether this mode draws the reading slots at all.
    pub fn uses_readings(self) -> bool {
        self.reading_slots() > 0
    }

    /// Whether this mode needs an image path before it can render.
    pub fn uses_image(self) -> bool {
        matches!(self, Self::Image)
    }

    /// Whether a band shades between its slot's two colors.
    ///
    /// Only the layout that gives one metric the whole ring: a band long enough
    /// for a shade to be a shade rather than a smear. The paired layout draws
    /// each of its two bands solid, which is also what the reference screens
    /// do, and it keeps the editor from asking for four colors where two will
    /// never be told apart.
    pub fn gradates_band(self) -> bool {
        matches!(self, Self::SingleReading)
    }
}

/// A telemetry value a reading slot can be pointed at.
///
/// Every entry resolves to a field of [`TelemetrySnapshot`] that an existing
/// collector fills. A metric the daemon does not sample has no variant here,
/// because a select entry that can only ever render `--` is a fabricated
/// capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcdMetric {
    CpuTemperature,
    GpuTemperature,
    LiquidTemperature,
    CpuLoad,
    GpuLoad,
}

impl LcdMetric {
    pub const ALL: [Self; 5] = [
        Self::CpuTemperature,
        Self::GpuTemperature,
        Self::LiquidTemperature,
        Self::CpuLoad,
        Self::GpuLoad,
    ];

    /// Full name, for the editor's select.
    pub fn label(self) -> &'static str {
        match self {
            Self::CpuTemperature => "CPU temperature",
            Self::GpuTemperature => "GPU temperature",
            Self::LiquidTemperature => "Liquid temperature",
            Self::CpuLoad => "CPU load",
            Self::GpuLoad => "GPU load",
        }
    }

    /// The short caption drawn on the panel itself.
    ///
    /// The panel is 240 pixels across and carries two of these, so the caption
    /// is what fits there rather than an abbreviation of [`Self::label`].
    pub fn caption(self) -> &'static str {
        match self {
            Self::CpuTemperature | Self::CpuLoad => "CPU",
            Self::GpuTemperature | Self::GpuLoad => "GPU",
            Self::LiquidTemperature => "LIQUID",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::CpuTemperature | Self::GpuTemperature | Self::LiquidTemperature => "\u{b0}",
            Self::CpuLoad | Self::GpuLoad => "%",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::CpuTemperature => "cpu_temperature",
            Self::GpuTemperature => "gpu_temperature",
            Self::LiquidTemperature => "liquid_temperature",
            Self::CpuLoad => "cpu_load",
            Self::GpuLoad => "gpu_load",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|metric| metric.key() == key)
    }

    /// Value at which this metric's arc reaches a full sweep.
    ///
    /// Temperatures use 100 degrees rather than the liquid failsafe at 60, so
    /// a CPU and a coolant reading drawn side by side share one scale.
    pub fn full_scale(self) -> f32 {
        100.0
    }

    /// Read this metric out of a snapshot.
    ///
    /// A metric whose collector reported nothing produces a sample with no
    /// value. The renderer turns that into `--` and a neutral arc, rather than
    /// a zero that looks like a reading.
    pub fn sample(self, telemetry: &TelemetrySnapshot) -> MetricSample {
        let value = match self {
            Self::CpuTemperature => telemetry.system.cpu_temperature_c.copied(),
            Self::GpuTemperature => telemetry.gpu.temperature_c.copied(),
            Self::LiquidTemperature => telemetry.kraken.liquid_temperature_c.copied(),
            Self::CpuLoad => telemetry.system.cpu_load_percent.copied(),
            Self::GpuLoad => telemetry.gpu.load_percent.copied(),
        };
        MetricSample {
            metric: self,
            value: value.filter(|reading| reading.is_finite()),
        }
    }
}

/// One metric as the panel is about to draw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricSample {
    pub metric: LcdMetric,
    /// `None` when the collector reported the metric unavailable.
    pub value: Option<f32>,
}

impl MetricSample {
    /// A sample nothing could be read for.
    pub fn unavailable(metric: LcdMetric) -> Self {
        Self {
            metric,
            value: None,
        }
    }

    /// The digits drawn on the panel, or the unavailable marker.
    ///
    /// Temperatures and loads are both drawn as whole numbers: at this size a
    /// decimal costs a glyph the panel cannot spare and changes every frame.
    pub fn text(&self) -> String {
        match self.value {
            Some(value) => format!("{}", value.round() as i32),
            None => "--".to_string(),
        }
    }

    /// How far around its arc this sample sits, in `0.0..=1.0`.
    ///
    /// `None` keeps the arc in its neutral state rather than collapsing it to
    /// zero, which would be indistinguishable from a real reading of zero.
    pub fn fraction(&self) -> Option<f32> {
        self.value
            .map(|value| (value / self.metric.full_scale()).clamp(0.0, 1.0))
    }
}

/// How the picture is turned before it reaches the panel.
///
/// The renderer applies this to the framebuffer, so the preview and the panel
/// show the same thing. The device is left on its own orientation zero, which
/// is what keeps the two in step: a rotation applied in two places at once
/// would cancel or double.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Orientation {
    pub const ALL: [Self; 4] = [Self::Deg0, Self::Deg90, Self::Deg180, Self::Deg270];

    /// Smallest turn the panel accepts, in degrees.
    pub const INCREMENT_DEGREES: u16 = 90;

    pub fn degrees(self) -> u16 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }

    /// The value the display-control report carries, `0` through `3`.
    pub fn quarter_turns(self) -> u8 {
        (self.degrees() / Self::INCREMENT_DEGREES) as u8
    }

    pub fn label(self) -> String {
        format!("{}\u{b0}", self.degrees())
    }

    /// The next orientation clockwise, which is what Rotate Display activates.
    pub fn rotated(self) -> Self {
        match self {
            Self::Deg0 => Self::Deg90,
            Self::Deg90 => Self::Deg180,
            Self::Deg180 => Self::Deg270,
            Self::Deg270 => Self::Deg0,
        }
    }
}

/// One metric slot of the infographic: what it shows and in which colors.
///
/// The band and the text are colored separately, which is the split the
/// reference screens make: the band carries the color, and the value reads in
/// the same color as its caption. A band may shade between two colors in the
/// layouts that draw it long enough for a shade to register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingSlot {
    pub metric: LcdMetric,
    /// Color the band starts on, at the foot of its sweep.
    pub reading: Rgb,
    /// Color the band reaches at the head of its sweep.
    ///
    /// `None` is a solid band. It is also what a preset written before this
    /// field existed deserializes to, which is why it is optional rather than
    /// defaulted to a color nobody chose.
    #[serde(default)]
    pub reading_end: Option<Rgb>,
    /// Color of the value and of the caption under it.
    pub text: Rgb,
}

impl ReadingSlot {
    /// The color the band ends on, which is where it starts when it is solid.
    pub fn band_end(&self) -> Rgb {
        self.reading_end.unwrap_or(self.reading)
    }
}

/// Everything the panel needs, with no pixel and no protocol byte in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPreset {
    pub mode: DisplayMode,
    /// Reading 1 and Reading 2, in the order they are drawn.
    pub readings: [ReadingSlot; 2],
    pub background: Rgb,
    /// Accepted, never drawn, never written back.
    ///
    /// The panel carried the product's name until it was taken off the glass.
    /// The field stays so a profile written before that still loads, since a
    /// preset refuses unknown fields; `skip_serializing` keeps it out of every
    /// file written from now on. The condition that retires it is recorded
    /// under "Deprecated fields awaiting removal" in `docs/schema-history.md`,
    /// because a compatibility shim with no stated end is permanent.
    #[serde(default, skip_serializing)]
    pub logo: Option<Rgb>,
    pub orientation: Orientation,
    pub brightness: Brightness,
    /// Set only when [`DisplayMode::Image`] is selected.
    pub image: Option<PathBuf>,
}

impl DisplayPreset {
    /// The preset a panel starts on: the dual infographic.
    pub fn default_infographic() -> Self {
        Self {
            mode: DisplayMode::DualInfographic,
            readings: [
                ReadingSlot {
                    metric: LcdMetric::CpuTemperature,
                    reading: Rgb::new(0x6B, 0x00, 0xDE),
                    reading_end: Some(Rgb::new(0xD6, 0x00, 0xBF)),
                    text: Rgb::new(0xFF, 0xFF, 0xFF),
                },
                ReadingSlot {
                    metric: LcdMetric::GpuTemperature,
                    reading: Rgb::new(0xD6, 0x00, 0xBF),
                    reading_end: Some(Rgb::new(0x6B, 0x00, 0xDE)),
                    text: Rgb::new(0xFF, 0xFF, 0xFF),
                },
            ],
            background: Rgb::BLACK,
            logo: None,
            orientation: Orientation::Deg0,
            brightness: Brightness::FULL,
            image: None,
        }
    }

    /// A single-color field, which is the shape the transport probe sends.
    pub fn solid(color: Rgb) -> Self {
        Self {
            mode: DisplayMode::Solid,
            background: color,
            ..Self::default_infographic()
        }
    }

    /// Refuse a preset the renderer could not honor.
    ///
    /// The colors and the brightness are already typed, so they cannot be out
    /// of range here. What remains is the agreement between the mode and the
    /// image path, and the length of that path. The daemon runs this on every
    /// received preset: the client is not a trusted input.
    pub fn validate(&self) -> Result<(), DisplayError> {
        match (self.mode, &self.image) {
            (DisplayMode::Image, None) => Err(DisplayError::ImagePathMissing),
            (DisplayMode::Image, Some(path)) => {
                if path.as_os_str().is_empty() {
                    return Err(DisplayError::ImagePathMissing);
                }
                if path.as_os_str().len() > MAX_IMAGE_PATH_BYTES {
                    return Err(DisplayError::ImagePathTooLong {
                        bytes: path.as_os_str().len(),
                        max_bytes: MAX_IMAGE_PATH_BYTES,
                    });
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Both reading slots resolved against one telemetry pass.
    pub fn samples(&self, telemetry: &TelemetrySnapshot) -> [MetricSample; 2] {
        [
            self.readings[0].metric.sample(telemetry),
            self.readings[1].metric.sample(telemetry),
        ]
    }
}

/// Every way a preset or a frame can be refused, naming the field at fault.
///
/// Tagged `kind`, like the other validation errors, because [`crate::ipc::IpcError`]
/// wraps this one and is itself tagged `error`: two `error` tags in one object
/// is a frame neither side can decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DisplayError {
    #[error("{field}: {source}")]
    Color {
        /// The editor field the operator was typing in.
        field: String,
        #[source]
        source: LightingError,
    },
    #[error("Image mode needs a file to display.")]
    ImagePathMissing,
    #[error("Image path is {bytes} bytes, above the {max_bytes} byte limit.")]
    ImagePathTooLong { bytes: usize, max_bytes: usize },
    #[error("{path} could not be decoded as an image: {detail}")]
    ImageUndecodable { path: String, detail: String },
    #[error("Image is {width}x{height}, above the {max}x{max} limit.")]
    ImageTooLarge { width: u32, height: u32, max: u32 },
    #[error("The animation has more than {max} frames, which is more than this panel holds.")]
    AnimationTooLong { max: usize },
    #[error("The panel geometry is not known, so no frame can be laid out.")]
    PanelUnknown,
}

impl DisplayError {
    /// The editor field this error belongs to, when it belongs to one.
    pub fn field(&self) -> Option<&str> {
        match self {
            Self::Color { field, .. } => Some(field),
            Self::ImagePathMissing
            | Self::ImagePathTooLong { .. }
            | Self::ImageUndecodable { .. }
            | Self::ImageTooLarge { .. }
            | Self::AnimationTooLong { .. } => Some("image"),
            Self::PanelUnknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{Reading, Unavailable};

    fn snapshot() -> TelemetrySnapshot {
        let mut snapshot = TelemetrySnapshot::unavailable(0, Unavailable::absent("fixture"));
        snapshot.system.cpu_temperature_c = Reading::valid(61.4);
        snapshot.system.cpu_load_percent = Reading::valid(12.0);
        snapshot.gpu.temperature_c = Reading::valid(48.6);
        snapshot.kraken.liquid_temperature_c = Reading::valid(31.2);
        snapshot
    }

    #[test]
    fn every_metric_resolves_to_a_collector_this_daemon_actually_samples() {
        let telemetry = snapshot();
        // GPU load is the one field the fixture leaves unavailable, which is
        // exactly the case the panel has to render as `--`.
        for metric in LcdMetric::ALL {
            let sample = metric.sample(&telemetry);
            assert_eq!(sample.metric, metric);
            if metric == LcdMetric::GpuLoad {
                assert_eq!(sample.value, None);
            } else {
                assert!(sample.value.is_some(), "{metric:?} resolved to nothing");
            }
        }
    }

    #[test]
    fn an_unavailable_metric_is_never_drawn_as_zero() {
        let sample = MetricSample::unavailable(LcdMetric::GpuTemperature);
        assert_eq!(sample.text(), "--");
        assert_eq!(
            sample.fraction(),
            None,
            "a missing reading must not collapse the arc onto zero, which is a \
             value the metric could genuinely have"
        );

        let zero = MetricSample {
            metric: LcdMetric::GpuTemperature,
            value: Some(0.0),
        };
        assert_eq!(zero.text(), "0");
        assert_eq!(zero.fraction(), Some(0.0));
    }

    #[test]
    fn a_reading_beyond_full_scale_stays_inside_its_arc() {
        let hot = MetricSample {
            metric: LcdMetric::CpuTemperature,
            value: Some(180.0),
        };
        assert_eq!(hot.fraction(), Some(1.0));
        assert_eq!(hot.text(), "180", "the number is not clamped, only the arc");

        let cold = MetricSample {
            metric: LcdMetric::CpuTemperature,
            value: Some(-40.0),
        };
        assert_eq!(cold.fraction(), Some(0.0));
    }

    #[test]
    fn a_non_finite_reading_is_treated_as_unavailable() {
        let mut telemetry = snapshot();
        telemetry.system.cpu_temperature_c = Reading::valid(f32::NAN);
        assert_eq!(LcdMetric::CpuTemperature.sample(&telemetry).value, None);

        telemetry.system.cpu_temperature_c = Reading::valid(f32::INFINITY);
        let sample = LcdMetric::CpuTemperature.sample(&telemetry);
        assert_eq!(sample.value, None);
        assert_eq!(sample.text(), "--");
    }

    #[test]
    fn rotation_walks_the_validated_increment_and_returns_to_zero() {
        let mut orientation = Orientation::Deg0;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(orientation.degrees());
            orientation = orientation.rotated();
        }
        assert_eq!(seen, vec![0, 90, 180, 270]);
        assert_eq!(orientation, Orientation::Deg0);
        assert!(
            Orientation::ALL
                .iter()
                .all(|o| o.degrees() % Orientation::INCREMENT_DEGREES == 0)
        );
    }

    #[test]
    fn quarter_turns_match_what_the_display_control_report_carries() {
        assert_eq!(Orientation::Deg0.quarter_turns(), 0);
        assert_eq!(Orientation::Deg90.quarter_turns(), 1);
        assert_eq!(Orientation::Deg180.quarter_turns(), 2);
        assert_eq!(Orientation::Deg270.quarter_turns(), 3);
    }

    #[test]
    fn image_mode_without_a_file_is_refused_and_names_the_field() {
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        let error = preset.validate().unwrap_err();
        assert_eq!(error, DisplayError::ImagePathMissing);
        assert_eq!(error.field(), Some("image"));

        preset.image = Some(PathBuf::from(""));
        assert_eq!(preset.validate(), Err(DisplayError::ImagePathMissing));

        preset.image = Some(PathBuf::from("/home/a/wallpaper.png"));
        assert!(preset.validate().is_ok());
    }

    #[test]
    fn an_unbounded_image_path_is_refused_before_it_becomes_a_frame() {
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = DisplayMode::Image;
        preset.image = Some(PathBuf::from("/".repeat(MAX_IMAGE_PATH_BYTES + 1)));
        match preset.validate().unwrap_err() {
            DisplayError::ImagePathTooLong { max_bytes, .. } => {
                assert_eq!(max_bytes, MAX_IMAGE_PATH_BYTES)
            }
            other => panic!("expected a length refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_path_left_over_from_image_mode_does_not_block_another_mode() {
        let mut preset = DisplayPreset::default_infographic();
        preset.image = Some(PathBuf::from("/home/a/wallpaper.png"));
        assert!(
            preset.validate().is_ok(),
            "switching back to the infographic must not be refused by a path \
             the operator picked earlier"
        );
    }

    #[test]
    fn a_preset_round_trips_through_json_without_carrying_pixels() {
        let preset = DisplayPreset::default_infographic();
        let json = serde_json::to_string(&preset).unwrap();
        assert_eq!(
            serde_json::from_str::<DisplayPreset>(&json).unwrap(),
            preset
        );
        assert!(!json.contains("pixel"), "{json}");
        assert!(json.contains("dual_infographic"), "{json}");
    }

    #[test]
    fn a_preset_written_before_the_band_had_two_colors_still_loads() {
        // The field was added after profiles were already on disk. Absent means
        // a solid band, which is what those profiles drew, rather than a color
        // nobody chose being invented for them.
        let json = r#"{"mode":"dual_infographic","readings":[
            {"metric":"cpu_temperature","reading":{"r":111,"g":78,"b":242},
             "text":{"r":230,"g":232,"b":239}},
            {"metric":"gpu_temperature","reading":{"r":55,"g":194,"b":166},
             "text":{"r":230,"g":232,"b":239}}],
            "background":{"r":11,"g":12,"b":16},"logo":{"r":138,"g":144,"b":162},
            "orientation":"deg0","brightness":100,"image":null}"#;
        let preset: DisplayPreset =
            serde_json::from_str(json).expect("an older preset still loads");
        assert_eq!(preset.readings[0].reading_end, None);
        assert_eq!(
            preset.readings[0].band_end(),
            preset.readings[0].reading,
            "a band with no second color is solid"
        );
        assert!(preset.validate().is_ok());

        // And a preset that carries one round-trips with it.
        let two_colored = DisplayPreset::default_infographic();
        assert!(two_colored.readings[0].reading_end.is_some());
        let encoded = serde_json::to_string(&two_colored).unwrap();
        assert_eq!(
            serde_json::from_str::<DisplayPreset>(&encoded).unwrap(),
            two_colored
        );
    }

    #[test]
    fn an_unknown_preset_field_is_rejected_rather_than_ignored() {
        let json = r#"{"mode":"solid","readings":[],"background":{"r":0,"g":0,"b":0},
            "logo":{"r":0,"g":0,"b":0},"orientation":"deg0","brightness":100,
            "image":null,"force":true}"#;
        assert!(serde_json::from_str::<DisplayPreset>(json).is_err());
    }

    #[test]
    fn every_mode_and_metric_key_round_trips() {
        for mode in DisplayMode::ALL {
            assert_eq!(DisplayMode::from_key(mode.key()), Some(mode));
        }
        for metric in LcdMetric::ALL {
            assert_eq!(LcdMetric::from_key(metric.key()), Some(metric));
        }
        assert_eq!(DisplayMode::from_key("gif"), None);
        assert_eq!(LcdMetric::from_key("fan_rpm"), None);
    }

    #[test]
    fn each_mode_declares_how_many_readings_it_draws() {
        assert_eq!(DisplayMode::DualInfographic.reading_slots(), 2);
        assert_eq!(DisplayMode::SingleReading.reading_slots(), 1);
        assert_eq!(DisplayMode::Solid.reading_slots(), 0);
        assert_eq!(DisplayMode::Image.reading_slots(), 0);

        // The count is what `uses_readings` is derived from, so a mode can
        // never claim to draw readings and then have no slot to draw.
        for mode in DisplayMode::ALL {
            assert_eq!(mode.uses_readings(), mode.reading_slots() > 0);
            assert!(
                mode.reading_slots() <= 2,
                "{mode:?} asks for more slots than a preset carries"
            );
        }
        assert!(DisplayMode::Image.uses_image());
        assert!(!DisplayMode::Solid.uses_image());
        assert!(!DisplayMode::SingleReading.uses_image());
    }
}
