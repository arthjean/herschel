// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Named cooling profiles and the validation every value passes before it can
//! reach the daemon.
//!
//! Validation lives here, next to the types, so the client can disable Apply
//! for the same reason the daemon would reject the command. The daemon still
//! revalidates: the client is not a trusted input.

use serde::{Deserialize, Serialize};

use crate::DeviceId;
use crate::capability::{CapabilityId, DeviceRecord};
use crate::display::DisplayPreset;
use crate::lighting::LightingCommand;

mod curve;

pub use curve::{CURVE_NODE_COUNT, CURVE_NODE_STEP_C, CurveNodes};

/// Bumped whenever the on-disk configuration shape changes.
///
/// What each version changed, and which direction the incompatibility runs in,
/// is recorded in `docs/schema-history.md`.
pub const CONFIG_SCHEMA_VERSION: u32 = 4;

/// Name of the built-in profile that is always available and always safe.
pub const SAFE_PROFILE_NAME: &str = "Onboard safe";

pub const PROFILE_NAME_MIN_LEN: usize = 1;
pub const PROFILE_NAME_MAX_LEN: usize = 48;

/// Lowest duty the product will command on a liquid cooler pump.
///
/// The kernel accepts 0, which stops the pump. Nothing in this product has a
/// reason to command it, so the floor is enforced before the value is written.
pub const MIN_PUMP_DUTY: u8 = 51; // 20% of 255
/// Lowest duty commanded on a fan channel.
pub const MIN_FAN_DUTY: u8 = 0;
pub const MAX_DUTY: u8 = 255;

/// Highest duty the device actually distinguishes, as a percentage.
///
/// The hwmon ABI takes 0-255, but the driver stores a percentage and converts
/// in both directions, so the device has 101 distinct settings and not 256.
/// Anything finer than a percent is a value the hardware cannot hold.
pub const MAX_DUTY_PERCENT: u8 = 100;

/// The 0-255 duty that means `percent`.
///
/// Deliberately the driver's own `kraken3_percent_to_pwm`, rounding the same
/// way, so a value produced here survives the round trip through the device and
/// reads back as the percent it was asked for.
pub fn duty_from_percent(percent: u8) -> u8 {
    let percent = percent.min(MAX_DUTY_PERCENT) as u16;
    ((percent * MAX_DUTY as u16 + 50) / 100) as u8
}

/// The percentage a 0-255 duty becomes on the device.
///
/// The driver's own conversion, `DIV_ROUND_CLOSEST(val * 100, 255)`.
pub fn duty_to_percent(duty: u8) -> u8 {
    ((duty as u16 * 100 + (MAX_DUTY as u16 / 2)) / MAX_DUTY as u16) as u8
}

/// Kernel curve points exposed by `kraken2023`, one per degree Celsius.
pub const CURVE_POINT_COUNT: usize = 40;
/// Temperature of the first curve point, in degrees Celsius.
pub const CURVE_FIRST_TEMP_C: u8 = 20;
/// Temperature of the last curve point, in degrees Celsius.
pub const CURVE_LAST_TEMP_C: u8 = CURVE_FIRST_TEMP_C + CURVE_POINT_COUNT as u8 - 1;

/// Shortest interval the daemon leaves between two writes to the thermal path.
///
/// Same reasoning as [`crate::lighting::MIN_COMMAND_INTERVAL_MS`], and the
/// kernel documents the failure it prevents: these devices "can lock up or
/// discard the changes if they are too numerous at once"
/// (`Documentation/hwmon/nzxt-kraken3.rst`). liquidctl reaches the same
/// conclusion from the other side, spacing its own hwmon writes on the comment
/// that "the device can get confused when hammered with HID reports".
///
/// Deduplication is not backpressure: a client alternating between two
/// different programs is never deduplicated, and nothing else in the write path
/// bounds how fast it can arrive. Set well above the per-write cost, and above
/// the lighting floor because one curve transaction is forty-three attributes
/// rather than one report.
pub const MIN_PROGRAM_INTERVAL_MS: u64 = 200;

/// A controllable cooling channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Pump,
    Fan,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pump => "Pump",
            Self::Fan => "Fan",
        }
    }

    /// Lowest duty accepted on this channel.
    pub fn min_duty(self) -> u8 {
        match self {
            Self::Pump => MIN_PUMP_DUTY,
            Self::Fan => MIN_FAN_DUTY,
        }
    }

    /// Capability required to set a fixed duty on this channel.
    pub fn duty_capability(self) -> CapabilityId {
        match self {
            Self::Pump => CapabilityId::PumpDuty,
            Self::Fan => CapabilityId::FanDuty,
        }
    }

    /// Capability required to write an onboard curve on this channel.
    pub fn curve_capability(self) -> CapabilityId {
        match self {
            Self::Pump => CapabilityId::PumpCurve,
            Self::Fan => CapabilityId::FanCurve,
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// What a profile asks the hardware to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CoolingProgram {
    /// Leave the device on its own firmware program.
    ///
    /// This is the safe default: it writes nothing and preserves the firmware
    /// failsafe unconditionally.
    Onboard,
    /// A constant duty per channel.
    Fixed { pump: u8, fan: u8 },
    /// A liquid-temperature curve per channel.
    Curve {
        pump: TemperatureCurve,
        fan: TemperatureCurve,
    },
}

impl CoolingProgram {
    /// Capabilities that must be writable before this program can be applied.
    pub fn required_capabilities(&self) -> Vec<CapabilityId> {
        match self {
            Self::Onboard => Vec::new(),
            Self::Fixed { .. } => vec![CapabilityId::PumpDuty, CapabilityId::FanDuty],
            Self::Curve { .. } => vec![CapabilityId::PumpCurve, CapabilityId::FanCurve],
        }
    }
}

/// The wire and on-disk shape of a curve, which is a list of unknown length.
///
/// Kept separate from [`TemperatureCurve`] so the length is checked exactly
/// once, at the boundary where a list of unknown length actually arrives, and
/// so the object written to a config file or a socket frame stays the
/// `{"points": [..]}` it has always been.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurveWire {
    points: Vec<u8>,
}

/// Exactly one PWM value per kernel curve point.
///
/// The point count is a property of the type rather than something each caller
/// rechecks. It was the latter, and the four separate checks that resulted did
/// not cover the same ground: the divergence window in the daemon indexes this
/// array directly, and it stayed in bounds only because a length check in a
/// different crate happened to run two lines earlier. An invariant that holds
/// by construction is what makes that indexing safe to read.
///
/// A curve arriving from a socket frame or a config file is a list of unknown
/// length, so it enters through [`TemperatureCurve::new`] and is refused there
/// when it does not carry the ABI's count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CurveWire", into = "CurveWire")]
pub struct TemperatureCurve {
    points: [u8; CURVE_POINT_COUNT],
}

impl TryFrom<CurveWire> for TemperatureCurve {
    type Error = ValidationError;

    fn try_from(wire: CurveWire) -> Result<Self, Self::Error> {
        Self::new(wire.points)
    }
}

impl From<TemperatureCurve> for CurveWire {
    fn from(curve: TemperatureCurve) -> Self {
        Self {
            points: curve.points.to_vec(),
        }
    }
}

impl TemperatureCurve {
    /// A curve from a list whose length is not known yet.
    ///
    /// The one door into the type from outside, and the only place the ABI's
    /// point count is compared against anything.
    pub fn new(points: Vec<u8>) -> Result<Self, ValidationError> {
        let points: [u8; CURVE_POINT_COUNT] =
            points
                .as_slice()
                .try_into()
                .map_err(|_| ValidationError::CurvePointCount {
                    expected: CURVE_POINT_COUNT,
                    actual: points.len(),
                })?;
        Ok(Self { points })
    }

    /// A curve from a list that already carries the ABI's count.
    ///
    /// For a caller that built the array from [`CURVE_POINT_COUNT`] itself, so
    /// it does not have to handle a refusal that cannot happen. Without it such
    /// a caller would reach for an unwrap on a `Result` it can prove is `Ok`,
    /// which is the failure mode this crate refuses everywhere else.
    pub fn from_points(points: [u8; CURVE_POINT_COUNT]) -> Self {
        Self { points }
    }

    /// The duty commanded at each ABI point, in ascending temperature order.
    pub fn points(&self) -> &[u8; CURVE_POINT_COUNT] {
        &self.points
    }

    /// The same, for a caller editing a point in place.
    ///
    /// A `&mut [u8; N]` lets every existing edit through and lets no caller
    /// change how many points there are, which is the whole invariant.
    pub fn points_mut(&mut self) -> &mut [u8; CURVE_POINT_COUNT] {
        &mut self.points
    }

    /// Temperature in degrees Celsius of the point at `index`.
    pub fn temperature_at(index: usize) -> u8 {
        CURVE_FIRST_TEMP_C + index as u8
    }

    /// A flat curve at `duty`, used as a starting point in the editor.
    pub fn flat(duty: u8) -> Self {
        Self {
            points: [duty; CURVE_POINT_COUNT],
        }
    }

    /// Index of the point that governs `temperature_c`.
    ///
    /// Clamped at both ends: the ABI starts at 20 C and the firmware runs the
    /// last point above 59 C, so a reading outside the range still names the
    /// point that is in force.
    ///
    /// `None` for a temperature that is not a number. Clamping propagates a
    /// NaN and the cast that follows turns it into index 0, which would answer
    /// "the duty commanded at 20 C" for a reading nothing measured: the same
    /// fabricated default [`crate::telemetry::clamp_percent`] refuses, in the
    /// path that decides whether the device is running the committed curve.
    pub fn point_index_for(temperature_c: f32) -> Option<usize> {
        if !temperature_c.is_finite() {
            return None;
        }
        let offset = (temperature_c - CURVE_FIRST_TEMP_C as f32).round();
        Some(offset.clamp(0.0, (CURVE_POINT_COUNT - 1) as f32) as usize)
    }

    /// The duty this curve commands at `temperature_c`.
    ///
    /// `None` only when the temperature names no point, which is a temperature
    /// that is not a number. Every curve carries every point.
    pub fn duty_at(&self, temperature_c: f32) -> Option<u8> {
        Some(self.points[Self::point_index_for(temperature_c)?])
    }

    /// The highest duty this curve ever commands.
    ///
    /// The last point, because [`validate_curve`] refuses a curve that
    /// decreases. Named here so the callers that need a safe fallback duty do
    /// not each restate that reasoning next to an indexing expression.
    pub fn highest_duty(&self) -> u8 {
        self.points[CURVE_POINT_COUNT - 1]
    }
}

/// A named local profile.
///
/// Unknown fields are refused: an unrecognized key means the file or frame came
/// from a version this build cannot interpret, which is a recovery case rather
/// than something to silently ignore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub program: CoolingProgram,
    /// Device the profile was created for, when it is device-specific.
    pub device: Option<DeviceId>,
    /// What each lighting channel should show, when the profile sets any.
    ///
    /// Stored as named colors, effects and speeds. No packet, report
    /// identifier or byte sequence enters the configuration file, so a stored
    /// profile never pins the product to one reverse-engineered encoding.
    ///
    /// Defaulted so a profile written before lighting existed still loads.
    #[serde(default)]
    pub lighting: Vec<LightingCommand>,
    /// What the panel should show, when the profile sets anything.
    ///
    /// A description only: no resolution, no pixel and no protocol byte, for
    /// the same reason the lighting field stores named colors rather than
    /// packets. Defaulted so a profile written before the panel existed loads.
    #[serde(default)]
    pub display: Option<DisplayPreset>,
}

impl Profile {
    /// The built-in profile selected whenever configuration cannot be trusted.
    pub fn safe() -> Self {
        Self {
            name: SAFE_PROFILE_NAME.to_string(),
            program: CoolingProgram::Onboard,
            device: None,
            lighting: Vec::new(),
            display: None,
        }
    }

    pub fn is_safe_builtin(&self) -> bool {
        self.name == SAFE_PROFILE_NAME
            && self.program == CoolingProgram::Onboard
            && self.lighting.is_empty()
            && self.display.is_none()
    }
}

/// Why a value or profile was refused.
///
/// Each variant carries the accepted range so a control can name it without
/// duplicating the bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidationError {
    #[error("Profile name must be {min}-{max} characters.")]
    NameLength { min: usize, max: usize },
    #[error("Profile name must not be blank or padded with whitespace.")]
    NameBlank,
    #[error("{channel} duty {value} is outside the accepted range {min}-{max}.")]
    DutyOutOfRange {
        channel: Channel,
        value: u8,
        min: u8,
        max: u8,
    },
    #[error("A curve must contain exactly {expected} points, got {actual}.")]
    CurvePointCount { expected: usize, actual: usize },
    #[error(
        "{channel} curve duty at {temperature_c} C is {value}, outside the accepted range {min}-{max}."
    )]
    CurveDutyOutOfRange {
        channel: Channel,
        temperature_c: u8,
        value: u8,
        min: u8,
        max: u8,
    },
    #[error("Lighting on channel {channel} is invalid: {detail}")]
    Lighting { channel: u8, detail: String },
    #[error("The panel preset is invalid: {detail}")]
    Display { detail: String },
    #[error(
        "{channel} curve duty must never decrease as temperature rises: {previous} at {previous_temperature_c} C then {value} at {temperature_c} C."
    )]
    CurveNotMonotonic {
        channel: Channel,
        previous: u8,
        previous_temperature_c: u8,
        value: u8,
        temperature_c: u8,
    },
}

impl ValidationError {
    /// True when the error points at a specific input field.
    pub fn channel(&self) -> Option<Channel> {
        match self {
            Self::DutyOutOfRange { channel, .. }
            | Self::CurveDutyOutOfRange { channel, .. }
            | Self::CurveNotMonotonic { channel, .. } => Some(*channel),
            Self::NameLength { .. }
            | Self::NameBlank
            | Self::CurvePointCount { .. }
            | Self::Lighting { .. }
            | Self::Display { .. } => None,
        }
    }
}

/// Validate a fixed duty for one channel.
pub fn validate_duty(channel: Channel, value: u8) -> Result<(), ValidationError> {
    let min = channel.min_duty();
    if value < min {
        return Err(ValidationError::DutyOutOfRange {
            channel,
            value,
            min,
            max: MAX_DUTY,
        });
    }
    Ok(())
}

/// Validate a complete curve before any point is written.
///
/// Every point is checked. A curve is never written partially, so a single
/// invalid point rejects the whole transaction.
///
/// The point count is not among the checks: a [`TemperatureCurve`] that exists
/// carries it, and [`TemperatureCurve::new`] is where a list that does not is
/// turned away.
pub fn validate_curve(channel: Channel, curve: &TemperatureCurve) -> Result<(), ValidationError> {
    let min = channel.min_duty();
    let mut previous: Option<(u8, u8)> = None;
    for (index, &value) in curve.points().iter().enumerate() {
        let temperature_c = TemperatureCurve::temperature_at(index);
        if value < min {
            return Err(ValidationError::CurveDutyOutOfRange {
                channel,
                temperature_c,
                value,
                min,
                max: MAX_DUTY,
            });
        }
        if let Some((previous_value, previous_temperature_c)) = previous
            && value < previous_value
        {
            return Err(ValidationError::CurveNotMonotonic {
                channel,
                previous: previous_value,
                previous_temperature_c,
                value,
                temperature_c,
            });
        }
        previous = Some((value, temperature_c));
    }
    Ok(())
}

/// Validate a profile name.
pub fn validate_name(name: &str) -> Result<(), ValidationError> {
    if name.trim() != name || name.trim().is_empty() {
        return Err(ValidationError::NameBlank);
    }
    let len = name.chars().count();
    if !(PROFILE_NAME_MIN_LEN..=PROFILE_NAME_MAX_LEN).contains(&len) {
        return Err(ValidationError::NameLength {
            min: PROFILE_NAME_MIN_LEN,
            max: PROFILE_NAME_MAX_LEN,
        });
    }
    Ok(())
}

/// Validate every value a program carries, before any of it can be written.
///
/// A program is validated as a whole: one bad point rejects the transaction,
/// so the hardware never receives a partially checked curve.
pub fn validate_program(program: &CoolingProgram) -> Result<(), ValidationError> {
    match program {
        CoolingProgram::Onboard => Ok(()),
        CoolingProgram::Fixed { pump, fan } => {
            validate_duty(Channel::Pump, *pump)?;
            validate_duty(Channel::Fan, *fan)
        }
        CoolingProgram::Curve { pump, fan } => {
            validate_curve(Channel::Pump, pump)?;
            validate_curve(Channel::Fan, fan)
        }
    }
}

/// Validate a whole profile: name, then every value it carries.
///
/// Lighting and the panel preset are validated for shape only. Whether a
/// channel exists, and whether a panel is there to draw on, are facts about the
/// connected hardware, so they are checked at activation against what the
/// devices reported rather than against anything stored in a file.
///
/// The preset is checked here and not only on the write path so a broken one is
/// refused at the Save that introduces it, naming the field, instead of being
/// stored and failing later at every activation with nothing to point at.
pub fn validate_profile(profile: &Profile) -> Result<(), ValidationError> {
    validate_name(&profile.name)?;
    validate_program(&profile.program)?;
    for command in &profile.lighting {
        crate::lighting::validate_program(&command.program).map_err(|source| {
            ValidationError::Lighting {
                channel: command.channel,
                detail: source.to_string(),
            }
        })?;
    }
    if let Some(preset) = &profile.display {
        preset
            .validate()
            .map_err(|source| ValidationError::Display {
                detail: source.to_string(),
            })?;
    }
    Ok(())
}

/// A capability a profile needs that the device cannot provide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incompatibility {
    pub capability: CapabilityId,
    pub reason: String,
}

/// List everything that prevents `profile` from being applied to `device`.
///
/// An empty result means activation may proceed. A non-empty result must stop
/// activation before any write.
pub fn incompatibilities(profile: &Profile, device: &DeviceRecord) -> Vec<Incompatibility> {
    let mut found = Vec::new();

    if let Some(expected) = profile.device
        && expected != device.id()
    {
        found.push(Incompatibility {
            capability: CapabilityId::LiquidTemperature,
            reason: format!(
                "Profile targets {expected} but the connected device is {}.",
                device.id()
            ),
        });
    }

    found.extend(program_incompatibilities(&profile.program, device));
    found
}

/// Everything that prevents `program` from being written to `device`.
///
/// Split from [`incompatibilities`] because the Cooling screen applies a
/// program that is not yet a profile, and it has to pass the same gate.
pub fn program_incompatibilities(
    program: &CoolingProgram,
    device: &DeviceRecord,
) -> Vec<Incompatibility> {
    let mut found = Vec::new();
    for capability in program.required_capabilities() {
        match device.capability(capability) {
            Some(entry) if entry.state.is_writable() => {}
            Some(entry) => found.push(Incompatibility {
                capability,
                reason: entry
                    .state
                    .blocked_reason()
                    .unwrap_or_else(|| format!("{} is not writable.", capability.label())),
            }),
            None => found.push(Incompatibility {
                capability,
                reason: format!(
                    "{} is absent from the capability record.",
                    capability.label()
                ),
            }),
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KRAKEN_BASE;
    use crate::capability::{Capability, CapabilityState, Evidenced, SupportState, UsbIdentity};

    /// The device holds a percentage, so a duty produced from one has to read
    /// back as that same percentage. Without this the editor could not offer a
    /// value and have the hardware agree it is running it.
    #[test]
    fn every_percentage_survives_the_trip_through_a_duty() {
        for percent in 0..=MAX_DUTY_PERCENT {
            let duty = duty_from_percent(percent);
            assert_eq!(
                duty_to_percent(duty),
                percent,
                "{percent}% became duty {duty}, which reads back as {}%",
                duty_to_percent(duty)
            );
        }
        assert_eq!(duty_from_percent(0), 0);
        assert_eq!(duty_from_percent(MAX_DUTY_PERCENT), MAX_DUTY);
    }

    /// The point of snapping: one step of the control is one step the hardware
    /// can tell apart. Two percentages sharing a duty would be a control with
    /// dead positions in it.
    #[test]
    fn no_two_percentages_share_a_duty() {
        let duties: Vec<u8> = (0..=MAX_DUTY_PERCENT).map(duty_from_percent).collect();
        assert!(
            duties.windows(2).all(|pair| pair[0] < pair[1]),
            "{duties:?}"
        );
    }

    /// The floor is the driver's own `PUMP_DUTY_MIN`, which is a percentage. It
    /// has to sit exactly on the grid, or clamping to it would produce a value
    /// no edit could ever land on.
    #[test]
    fn the_pump_floor_is_a_whole_percentage() {
        assert_eq!(duty_to_percent(MIN_PUMP_DUTY), 20);
        assert_eq!(duty_from_percent(20), MIN_PUMP_DUTY);
    }

    fn device_with(capabilities: Vec<Capability>) -> DeviceRecord {
        DeviceRecord {
            support: SupportState::Supported,
            usb: UsbIdentity {
                id: KRAKEN_BASE,
                manufacturer: Evidenced::unknown("absent", "sysfs"),
                product: Evidenced::unknown("absent", "sysfs"),
                serial: Evidenced::unknown("absent", "sysfs"),
                firmware: Evidenced::unknown("absent", "sysfs"),
                sysfs_path: "/sys/bus/usb/devices/1-9".into(),
            },
            interfaces: vec![],
            hwmon: None,
            rgb: None,
            lcd: None,
            capabilities,
        }
    }

    #[test]
    fn safe_profile_writes_nothing() {
        let safe = Profile::safe();
        assert!(safe.is_safe_builtin());
        assert!(safe.program.required_capabilities().is_empty());
        assert!(validate_profile(&safe).is_ok());
    }

    #[test]
    fn pump_duty_below_floor_is_rejected_with_its_range() {
        let error = validate_duty(Channel::Pump, 10).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("51-255"), "{message}");
        assert_eq!(error.channel(), Some(Channel::Pump));
    }

    #[test]
    fn fan_duty_accepts_zero_but_pump_does_not() {
        assert!(validate_duty(Channel::Fan, 0).is_ok());
        assert!(validate_duty(Channel::Pump, 0).is_err());
    }

    #[test]
    fn each_channel_names_its_own_capability() {
        // A control surface gates one channel's control on this, so the two
        // channels must never resolve to the same capability: a rule granting
        // pwm1 alone leaves pwm2 read-only, and the fan control has to know.
        assert_eq!(Channel::Pump.duty_capability(), CapabilityId::PumpDuty);
        assert_eq!(Channel::Fan.duty_capability(), CapabilityId::FanDuty);
        assert_eq!(Channel::Pump.curve_capability(), CapabilityId::PumpCurve);
        assert_eq!(Channel::Fan.curve_capability(), CapabilityId::FanCurve);

        // And every capability a program requires is one a channel names, so a
        // control cannot be gated on a capability the daemon never checks.
        let fixed = CoolingProgram::Fixed {
            pump: 128,
            fan: 128,
        }
        .required_capabilities();
        assert_eq!(
            fixed,
            vec![
                Channel::Pump.duty_capability(),
                Channel::Fan.duty_capability()
            ]
        );
        let curve = CoolingProgram::Curve {
            pump: TemperatureCurve::flat(120),
            fan: TemperatureCurve::flat(120),
        }
        .required_capabilities();
        assert_eq!(
            curve,
            vec![
                Channel::Pump.curve_capability(),
                Channel::Fan.curve_capability()
            ]
        );
    }

    /// The count is refused at the one door a list of unknown length comes
    /// through, rather than by each caller that later reads the curve.
    #[test]
    fn a_list_that_is_not_forty_points_never_becomes_a_curve() {
        for actual in [0usize, 39, 41] {
            let error = TemperatureCurve::new(vec![100; actual]).unwrap_err();
            assert_eq!(
                error,
                ValidationError::CurvePointCount {
                    expected: CURVE_POINT_COUNT,
                    actual
                }
            );
        }
        assert!(TemperatureCurve::new(vec![100; CURVE_POINT_COUNT]).is_ok());
    }

    /// The same door, reached from the wire. A stored profile or a socket frame
    /// carrying a short curve is refused while it is still a frame, so nothing
    /// downstream has to ask again.
    #[test]
    fn a_curve_of_the_wrong_length_is_refused_at_deserialization() {
        let short = r#"{"points":[100,100,100]}"#;
        let error = serde_json::from_str::<TemperatureCurve>(short).unwrap_err();
        assert!(error.to_string().contains("exactly 40"), "{error}");

        // And the accepted form is byte for byte what earlier builds wrote, so
        // no profile on disk needs rewriting and no schema version moves.
        let curve = TemperatureCurve::flat(120);
        let json = serde_json::to_string(&curve).unwrap();
        assert!(json.starts_with(r#"{"points":["#), "{json}");
        assert_eq!(
            serde_json::from_str::<TemperatureCurve>(&json).unwrap(),
            curve
        );
    }

    /// A temperature that is not a number names no point. The clamped form
    /// answered index 0, which on a validated curve is its lowest duty, so an
    /// unreadable temperature would have read as the coolest one there is.
    #[test]
    fn a_temperature_that_is_not_a_number_names_no_point_and_no_duty() {
        let curve = TemperatureCurve::flat(200);
        for impossible in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                TemperatureCurve::point_index_for(impossible),
                None,
                "{impossible}"
            );
            assert_eq!(curve.duty_at(impossible), None, "{impossible}");
        }

        // A reading outside the ABI's range still names the point in force.
        assert_eq!(TemperatureCurve::point_index_for(-40.0), Some(0));
        assert_eq!(
            TemperatureCurve::point_index_for(200.0),
            Some(CURVE_POINT_COUNT - 1)
        );
        assert_eq!(curve.duty_at(31.2), Some(200));
    }

    /// The fallback duty callers reach for when nothing can be measured.
    #[test]
    fn the_highest_duty_is_the_last_point_of_a_non_decreasing_curve() {
        let curve = CurveNodes::starting_ramp().interpolate();
        assert!(validate_curve(Channel::Pump, &curve).is_ok());
        assert_eq!(curve.highest_duty(), MAX_DUTY);
        assert_eq!(
            curve.highest_duty(),
            *curve.points().iter().max().unwrap(),
            "a validated curve never decreases, so its last point is its highest"
        );
    }

    #[test]
    fn curve_temperature_range_matches_the_kernel_abi() {
        assert_eq!(TemperatureCurve::temperature_at(0), CURVE_FIRST_TEMP_C);
        assert_eq!(
            TemperatureCurve::temperature_at(CURVE_POINT_COUNT - 1),
            CURVE_LAST_TEMP_C
        );
        assert_eq!(CURVE_LAST_TEMP_C, 59);
    }

    #[test]
    fn curve_must_not_decrease() {
        let mut curve = TemperatureCurve::flat(120);
        curve.points_mut()[20] = 119;
        let error = validate_curve(Channel::Pump, &curve).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("40 C"), "{message}");
    }

    #[test]
    fn profile_name_bounds_are_enforced() {
        assert!(validate_name("Silent").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("  ").is_err());
        assert!(validate_name(" padded").is_err());
        assert!(validate_name(&"x".repeat(48)).is_ok());
        assert!(validate_name(&"x".repeat(49)).is_err());
    }

    #[test]
    fn incompatible_profile_lists_every_missing_capability() {
        let device = device_with(vec![Capability {
            id: CapabilityId::PumpDuty,
            state: CapabilityState::Available {
                writable: false,
                source: "/sys/class/hwmon/hwmon4/pwm1".into(),
            },
        }]);
        let profile = Profile {
            name: "Fixed".into(),
            program: CoolingProgram::Fixed {
                pump: 128,
                fan: 128,
            },
            device: None,
            lighting: Vec::new(),
            display: None,
        };

        let found = incompatibilities(&profile, &device);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|i| i.capability == CapabilityId::PumpDuty));
        assert!(found.iter().any(|i| i.capability == CapabilityId::FanDuty));
    }

    #[test]
    fn profile_for_another_device_is_incompatible() {
        let device = device_with(vec![]);
        let profile = Profile {
            name: "Other".into(),
            program: CoolingProgram::Onboard,
            device: Some(crate::RGB_CONTROLLER),
            lighting: Vec::new(),
            display: None,
        };
        let found = incompatibilities(&profile, &device);
        assert_eq!(found.len(), 1);
        assert!(found[0].reason.contains("1e71:2021"), "{}", found[0].reason);
    }

    #[test]
    fn safe_profile_is_compatible_with_a_device_exposing_nothing() {
        let device = device_with(vec![]);
        assert!(incompatibilities(&Profile::safe(), &device).is_empty());
    }

    /// A preset that can never render is refused at the Save that introduces
    /// it, where the editor can point at the field, rather than at every later
    /// activation with nothing to point at. Lighting was already checked here;
    /// the panel is its sibling and had been left out.
    #[test]
    fn a_profile_carrying_an_unrenderable_preset_is_refused_at_save() {
        let mut preset = DisplayPreset::default_infographic();
        preset.mode = crate::display::DisplayMode::Image;
        preset.image = None;

        let profile = Profile {
            name: "Panel".into(),
            program: CoolingProgram::Onboard,
            device: None,
            lighting: Vec::new(),
            display: Some(preset.clone()),
        };
        let error = validate_profile(&profile).unwrap_err();
        assert!(
            matches!(error, ValidationError::Display { .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("needs a file"), "{error}");
        assert_eq!(error.channel(), None);

        preset.image = Some(std::path::PathBuf::from("/home/a/wallpaper.png"));
        let renderable = Profile {
            display: Some(preset),
            ..profile
        };
        assert!(validate_profile(&renderable).is_ok());
    }

    /// The safe profile carries no preset at all, so the new check cannot make
    /// the one profile that must always load start failing.
    #[test]
    fn the_safe_profile_still_validates_with_the_preset_check_in_place() {
        assert!(Profile::safe().display.is_none());
        assert!(validate_profile(&Profile::safe()).is_ok());
    }
}
