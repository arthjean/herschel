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

/// Bumped whenever the on-disk configuration shape changes.
///
/// Version 2 added the per-channel lighting a profile carries.
///
/// Version 3 added the panel preset. Both fields are optional, so a file at an
/// earlier version parses exactly as it stands and the next save rewrites it.
///
/// Version 4 added the session table, which is what the daemon last committed
/// rather than what the operator saved under a name. The bump matters in the
/// other direction: an earlier build refuses an unknown key outright and would
/// preserve the file as unreadable, while the version makes it say which build
/// wrote it.
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

/// Exactly one PWM value per kernel curve point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemperatureCurve {
    pub points: Vec<u8>,
}

impl TemperatureCurve {
    /// Temperature in degrees Celsius of the point at `index`.
    pub fn temperature_at(index: usize) -> u8 {
        CURVE_FIRST_TEMP_C + index as u8
    }

    /// A flat curve at `duty`, used as a starting point in the editor.
    pub fn flat(duty: u8) -> Self {
        Self {
            points: vec![duty; CURVE_POINT_COUNT],
        }
    }

    /// Index of the point that governs `temperature_c`.
    ///
    /// Clamped at both ends: the ABI starts at 20 C and the firmware runs the
    /// last point above 59 C, so a reading outside the range still names the
    /// point that is in force.
    pub fn point_index_for(temperature_c: f32) -> usize {
        let offset = (temperature_c - CURVE_FIRST_TEMP_C as f32).round();
        offset.clamp(0.0, (CURVE_POINT_COUNT - 1) as f32) as usize
    }

    /// The duty this curve commands at `temperature_c`.
    ///
    /// `None` when the curve does not carry the ABI's point count, because a
    /// curve of the wrong length names no duty at any temperature.
    pub fn duty_at(&self, temperature_c: f32) -> Option<u8> {
        if self.points.len() != CURVE_POINT_COUNT {
            return None;
        }
        self.points
            .get(Self::point_index_for(temperature_c))
            .copied()
    }
}

/// Degrees between two adjacent editor nodes.
///
/// Five, so every node lands on a round temperature and the axis reads 20, 25,
/// 30 rather than the arbitrary 20, 24, 29 that spacing ten nodes evenly across
/// forty points produces. The ABI's own range is what bounds this: there is no
/// point above 59 C to put a node on.
pub const CURVE_NODE_STEP_C: usize = 5;

/// Control nodes the editor exposes, fewer than the ABI's points.
///
/// Forty draggable points would be unusable and would let a curve wobble
/// between adjacent degrees. Nine nodes every five degrees over the same
/// 20-59 C range are what the operator edits; [`CurveNodes::interpolate`] turns
/// them into the exact 40 integer values the kernel accepts.
pub const CURVE_NODE_COUNT: usize = 9;

/// The editable form of a curve: one duty per node over the fixed kernel range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveNodes {
    /// Duty of each node, 0-255, in ascending temperature order.
    pub duty: [u8; CURVE_NODE_COUNT],
}

impl CurveNodes {
    /// ABI point index a node sits on.
    ///
    /// One point per whole [`CURVE_NODE_STEP_C`] degrees, so every node names a
    /// round temperature. The last node is the exception: five degrees past
    /// 55 C is 60 C, which the ABI does not reach, so it sits on the last point
    /// the kernel has. That makes the final segment four degrees wide where the
    /// others are five, which is a degree of drawing error and no error at all
    /// in what gets written.
    ///
    /// Landing on whole points is what makes [`CurveNodes::from_curve`] the
    /// exact inverse of [`CurveNodes::interpolate`]: a node read back off a
    /// stored curve is the value that was written, not a value re-derived
    /// through two roundings that would creep by one step per edit cycle.
    pub fn point_index(index: usize) -> usize {
        (index * CURVE_NODE_STEP_C).min(CURVE_POINT_COUNT - 1)
    }

    /// Temperature of the node at `index`, in whole degrees Celsius.
    ///
    /// The first node sits on the first ABI point and the last on the last, so
    /// the editor spans exactly the range the kernel accepts.
    pub fn temperature_at(index: usize) -> f32 {
        (CURVE_FIRST_TEMP_C as usize + Self::point_index(index)) as f32
    }

    /// Every node at the same duty.
    pub fn flat(duty: u8) -> Self {
        Self {
            duty: [duty; CURVE_NODE_COUNT],
        }
    }

    /// Set one node, keeping the whole set monotonically non-decreasing.
    ///
    /// Raising a node lifts every node above it and lowering one pulls every
    /// node below it. Monotonicity is therefore a property of the editor, not
    /// something the operator has to reconstruct after each edit; validation
    /// still runs before Apply because the daemon does not trust the client.
    pub fn set(&mut self, index: usize, duty: u8) {
        if index >= CURVE_NODE_COUNT {
            return;
        }
        self.duty[index] = duty;
        for higher in index + 1..CURVE_NODE_COUNT {
            if self.duty[higher] < duty {
                self.duty[higher] = duty;
            }
        }
        for lower in (0..index).rev() {
            if self.duty[lower] > duty {
                self.duty[lower] = duty;
            }
        }
    }

    /// Expand the nodes into exactly [`CURVE_POINT_COUNT`] integer duties.
    ///
    /// Linear between nodes, rounded to the nearest integer. Rounding is
    /// monotone, so a non-decreasing node set always produces a non-decreasing
    /// curve.
    pub fn interpolate(&self) -> TemperatureCurve {
        let points = (0..CURVE_POINT_COUNT)
            .map(|point| {
                let lower = (0..CURVE_NODE_COUNT)
                    .rev()
                    .find(|node| Self::point_index(*node) <= point)
                    .unwrap_or(0);
                let upper = (lower + 1).min(CURVE_NODE_COUNT - 1);
                let low_point = Self::point_index(lower);
                let high_point = Self::point_index(upper);
                let low = self.duty[lower] as f32;
                let high = self.duty[upper] as f32;
                let value = if high_point == low_point {
                    low
                } else {
                    let fraction = (point - low_point) as f32 / (high_point - low_point) as f32;
                    low + (high - low) * fraction
                };
                value.round().clamp(0.0, 255.0) as u8
            })
            .collect();
        TemperatureCurve { points }
    }

    /// The curve the editor starts from when no profile defines one.
    ///
    /// A linear ramp from 40% to 100% over the whole range: above the pump
    /// floor at every node, and already monotonic, so the first Apply cannot be
    /// the first time validation is exercised.
    pub fn starting_ramp() -> Self {
        let mut duty = [0u8; CURVE_NODE_COUNT];
        for (index, entry) in duty.iter_mut().enumerate() {
            // Built in percent and converted, so the curve the editor opens on
            // sits on the same grid a drag produces. A default that was the one
            // value off the grid would read as an edit the moment it was
            // touched.
            let percent = 40.0 + index as f32 * (60.0 / (CURVE_NODE_COUNT - 1) as f32);
            *entry = duty_from_percent(percent.round().clamp(0.0, 100.0) as u8);
        }
        Self { duty }
    }

    /// Read nodes back out of a stored curve, for the editor to load.
    ///
    /// Each node sits on a whole ABI point, so this reads the stored value
    /// directly and is the exact inverse of [`CurveNodes::interpolate`].
    ///
    /// A curve with the wrong point count cannot be sampled, so the caller
    /// gets `None` rather than a set built from whatever was there.
    pub fn from_curve(curve: &TemperatureCurve) -> Option<Self> {
        if curve.points.len() != CURVE_POINT_COUNT {
            return None;
        }
        let mut duty = [0u8; CURVE_NODE_COUNT];
        for (index, entry) in duty.iter_mut().enumerate() {
            *entry = curve.points[Self::point_index(index)];
        }
        Some(Self { duty })
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
    pub lighting: Vec<crate::lighting::LightingCommand>,
    /// What the panel should show, when the profile sets anything.
    ///
    /// A description only: no resolution, no pixel and no protocol byte, for
    /// the same reason the lighting field stores named colors rather than
    /// packets. Defaulted so a profile written before the panel existed loads.
    #[serde(default)]
    pub display: Option<crate::display::DisplayPreset>,
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
            | Self::Lighting { .. } => None,
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
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
pub fn validate_curve(channel: Channel, curve: &TemperatureCurve) -> Result<(), ValidationError> {
    if curve.points.len() != CURVE_POINT_COUNT {
        return Err(ValidationError::CurvePointCount {
            expected: CURVE_POINT_COUNT,
            actual: curve.points.len(),
        });
    }

    let min = channel.min_duty();
    let mut previous: Option<(u8, u8)> = None;
    for (index, &value) in curve.points.iter().enumerate() {
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
/// Lighting is validated for shape only. Whether a channel exists is a fact
/// about the connected controller, so it is checked at activation against what
/// the controller reported, not against a number stored in a file.
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

    /// A default that sat off the grid would read as an edit as soon as any
    /// node was touched, and with autosave it would be written.
    #[test]
    fn the_starting_ramp_sits_on_the_grid() {
        for duty in CurveNodes::starting_ramp().duty {
            assert_eq!(
                duty_from_percent(duty_to_percent(duty)),
                duty,
                "node duty {duty} is not a whole percentage"
            );
        }
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

    #[test]
    fn curve_requires_exactly_forty_points() {
        let short = TemperatureCurve {
            points: vec![100; 39],
        };
        let error = validate_curve(Channel::Fan, &short).unwrap_err();
        assert!(matches!(
            error,
            ValidationError::CurvePointCount {
                expected: 40,
                actual: 39
            }
        ));
    }

    #[test]
    fn curve_must_not_decrease() {
        let mut curve = TemperatureCurve::flat(120);
        curve.points[20] = 119;
        let error = validate_curve(Channel::Pump, &curve).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("40 C"), "{message}");
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

    #[test]
    fn the_nodes_span_exactly_the_kernel_range_on_round_degrees() {
        assert_eq!(CURVE_NODE_COUNT, 9);
        assert_eq!(CurveNodes::temperature_at(0), CURVE_FIRST_TEMP_C as f32);
        assert!(
            (CurveNodes::temperature_at(CURVE_NODE_COUNT - 1) - CURVE_LAST_TEMP_C as f32).abs()
                < 0.001
        );
        assert!(
            (0..CURVE_NODE_COUNT)
                .map(CurveNodes::temperature_at)
                .all(|t| (CURVE_FIRST_TEMP_C as f32..=CURVE_LAST_TEMP_C as f32).contains(&t))
        );
    }

    #[test]
    fn nodes_interpolate_to_exactly_forty_integer_points() {
        let nodes = CurveNodes::starting_ramp();
        let curve = nodes.interpolate();
        assert_eq!(curve.points.len(), CURVE_POINT_COUNT);
        // The endpoints land on the nodes rather than near them.
        assert_eq!(curve.points[0], nodes.duty[0]);
        assert_eq!(
            curve.points[CURVE_POINT_COUNT - 1],
            nodes.duty[CURVE_NODE_COUNT - 1]
        );
        assert!(validate_curve(Channel::Pump, &curve).is_ok());
        assert!(validate_curve(Channel::Fan, &curve).is_ok());
    }

    #[test]
    fn interpolation_between_two_nodes_is_linear() {
        let mut nodes = CurveNodes::flat(0);
        nodes.duty = [0, 90, 90, 90, 90, 90, 90, 90, 90];
        let curve = nodes.interpolate();
        // Node 1 sits on point index 5, so point 2 is two fifths of the way
        // up: 0 + 90 * 2/5 = 36.
        assert_eq!(curve.points[0], 0);
        assert_eq!(curve.points[2], 36);
        assert_eq!(curve.points[5], 90);
        assert!(curve.points.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn nodes_sit_on_whole_points_and_whole_degrees() {
        let indices: Vec<usize> = (0..CURVE_NODE_COUNT).map(CurveNodes::point_index).collect();
        assert_eq!(indices, vec![0, 5, 10, 15, 20, 25, 30, 35, 39]);
        assert!(
            indices.windows(2).all(|pair| pair[0] < pair[1]),
            "two nodes must never share a point"
        );
        let temperatures: Vec<f32> = (0..CURVE_NODE_COUNT)
            .map(CurveNodes::temperature_at)
            .collect();
        assert!(
            temperatures.iter().all(|t| t.fract() == 0.0),
            "{temperatures:?}"
        );
        assert_eq!(temperatures.first(), Some(&20.0));
        assert_eq!(temperatures.last(), Some(&59.0));
    }

    /// The axis an operator reads has to be made of round temperatures. Every
    /// node but the last sits on a multiple of the step; the last sits on the
    /// highest point the ABI has, because there is nothing above it to sit on.
    #[test]
    fn every_node_but_the_last_lands_on_a_round_temperature() {
        let temperatures: Vec<f32> = (0..CURVE_NODE_COUNT)
            .map(CurveNodes::temperature_at)
            .collect();
        assert_eq!(
            temperatures,
            vec![20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 59.0]
        );

        let step = CURVE_NODE_STEP_C as f32;
        for (index, temperature) in temperatures.iter().enumerate().take(CURVE_NODE_COUNT - 1) {
            assert_eq!(
                temperature % step,
                0.0,
                "node {index} is at {temperature} C, not a multiple of {step}"
            );
        }
        assert_eq!(
            *temperatures.last().unwrap(),
            CURVE_LAST_TEMP_C as f32,
            "the last node is pinned to the end of the ABI, not to a round degree"
        );
    }

    #[test]
    fn editing_one_node_keeps_the_whole_set_monotonic() {
        let mut nodes = CurveNodes::flat(100);
        nodes.set(4, 200);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(nodes.duty[3], 100);
        assert_eq!(nodes.duty[4], 200);
        assert_eq!(
            nodes.duty[CURVE_NODE_COUNT - 1],
            200,
            "higher nodes are lifted"
        );

        nodes.set(7, 50);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(nodes.duty[7], 50);
        assert_eq!(nodes.duty[4], 50, "lower nodes are pulled down");
        assert_eq!(
            nodes.duty[CURVE_NODE_COUNT - 1],
            200,
            "nodes above are untouched"
        );

        // An index outside the set changes nothing rather than panicking.
        let before = nodes;
        nodes.set(CURVE_NODE_COUNT, 255);
        assert_eq!(nodes, before);
    }

    #[test]
    fn an_edited_node_set_always_validates_as_a_curve() {
        let mut nodes = CurveNodes::starting_ramp();
        for (index, duty) in [(0, 255), (8, 60), (5, 130), (2, 200)] {
            nodes.set(index, duty);
            let curve = nodes.interpolate();
            assert!(
                validate_curve(Channel::Fan, &curve).is_ok(),
                "set({index}, {duty}) produced {curve:?}"
            );
        }
    }

    #[test]
    fn nodes_round_trip_through_a_stored_curve() {
        for nodes in [
            CurveNodes::starting_ramp(),
            CurveNodes::flat(200),
            CurveNodes {
                duty: [51, 51, 60, 80, 110, 150, 190, 240, 255],
            },
        ] {
            // Exact, not merely close: an edit cycle that crept by one PWM
            // step would drift a saved curve over repeated loads.
            assert_eq!(CurveNodes::from_curve(&nodes.interpolate()), Some(nodes));
        }

        // A curve of the wrong length cannot be sampled into nodes.
        assert!(
            CurveNodes::from_curve(&TemperatureCurve {
                points: vec![100; 12]
            })
            .is_none()
        );
    }

    #[test]
    fn the_starting_ramp_clears_the_pump_floor_at_every_node() {
        let nodes = CurveNodes::starting_ramp();
        assert!(nodes.duty.iter().all(|duty| *duty >= MIN_PUMP_DUTY));
        assert_eq!(nodes.duty[CURVE_NODE_COUNT - 1], 255);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}
