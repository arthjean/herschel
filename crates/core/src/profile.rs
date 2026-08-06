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
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

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

/// Kernel curve points exposed by `kraken2023`, one per degree Celsius.
pub const CURVE_POINT_COUNT: usize = 40;
/// Temperature of the first curve point, in degrees Celsius.
pub const CURVE_FIRST_TEMP_C: u8 = 20;
/// Temperature of the last curve point, in degrees Celsius.
pub const CURVE_LAST_TEMP_C: u8 = CURVE_FIRST_TEMP_C + CURVE_POINT_COUNT as u8 - 1;

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
}

/// A named local profile.
///
/// Unknown fields are refused: an unrecognised key means the file or frame came
/// from a version this build cannot interpret, which is a recovery case rather
/// than something to silently ignore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub program: CoolingProgram,
    /// Device the profile was created for, when it is device-specific.
    pub device: Option<DeviceId>,
}

impl Profile {
    /// The built-in profile selected whenever configuration cannot be trusted.
    pub fn safe() -> Self {
        Self {
            name: SAFE_PROFILE_NAME.to_string(),
            program: CoolingProgram::Onboard,
            device: None,
        }
    }

    pub fn is_safe_builtin(&self) -> bool {
        self.name == SAFE_PROFILE_NAME && self.program == CoolingProgram::Onboard
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
            Self::NameLength { .. } | Self::NameBlank | Self::CurvePointCount { .. } => None,
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

/// Validate a whole profile: name, then every value its program carries.
pub fn validate_profile(profile: &Profile) -> Result<(), ValidationError> {
    validate_name(&profile.name)?;
    match &profile.program {
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

    for capability in profile.program.required_capabilities() {
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
}
