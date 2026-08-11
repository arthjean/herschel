// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Evidence-backed description of what an allowlisted device actually exposes.
//!
//! Every field is either observed on the running machine or explicitly
//! [`Evidenced::Unknown`]. No default is fabricated to fill a gap, because a
//! fabricated default is indistinguishable from a validated capability once it
//! reaches a control surface.

use serde::{Deserialize, Serialize};

use crate::DeviceId;

/// Bumped whenever the shape of [`CapabilityRecord`] changes.
///
/// A record carrying an unknown version is rejected instead of guessed.
///
/// Version 2 added the USB endpoint list on every interface and the RGB
/// controller's [`RgbTopology`], both required to gate a lighting write.
///
/// Version 3 added the Kraken's [`LcdTopology`], which gates a panel write.
pub const CAPABILITY_SCHEMA_VERSION: u32 = 3;

/// A value that is either observed with its evidence, or explicitly unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Evidenced<T> {
    /// The value was read from `source`.
    Known { value: T, source: String },
    /// The value could not be established, and `reason` says what was tried.
    Unknown { reason: String, source: String },
}

impl<T> Evidenced<T> {
    pub fn known(value: T, source: impl Into<String>) -> Self {
        Self::Known {
            value,
            source: source.into(),
        }
    }

    pub fn unknown(reason: impl Into<String>, source: impl Into<String>) -> Self {
        Self::Unknown {
            reason: reason.into(),
            source: source.into(),
        }
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known { value, .. } => Some(value),
            Self::Unknown { .. } => None,
        }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known { .. })
    }

    /// Why the value is missing, for a control that has to say so.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Known { .. } => None,
            Self::Unknown { reason, .. } => Some(reason),
        }
    }
}

/// A capability a later story depends on.
///
/// The identifiers are stable: a cooling, RGB or LCD story names the exact
/// variant it requires and reads its [`CapabilityState`] before enabling a
/// control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityId {
    /// Coolant temperature in millidegrees Celsius.
    LiquidTemperature,
    /// Pump tachometer in RPM.
    PumpSpeed,
    /// Fan tachometer in RPM.
    FanSpeed,
    /// Fixed pump duty, 0-255 PWM.
    PumpDuty,
    /// Fixed fan duty, 0-255 PWM.
    FanDuty,
    /// Onboard pump curve over the driver's temperature points.
    PumpCurve,
    /// Onboard fan curve over the driver's temperature points.
    FanCurve,
    /// Per-channel fixed RGB color.
    RgbFixedColor,
    /// Animated RGB effects.
    RgbEffects,
    /// LCD framebuffer transfer.
    LcdFrame,
    /// LCD orientation and brightness commands.
    LcdDisplayControl,
}

impl CapabilityId {
    /// Human-readable label used by diagnostics and the client.
    pub fn label(self) -> &'static str {
        match self {
            Self::LiquidTemperature => "Liquid temperature",
            Self::PumpSpeed => "Pump speed",
            Self::FanSpeed => "Fan speed",
            Self::PumpDuty => "Pump duty",
            Self::FanDuty => "Fan duty",
            Self::PumpCurve => "Pump curve",
            Self::FanCurve => "Fan curve",
            Self::RgbFixedColor => "RGB fixed color",
            Self::RgbEffects => "RGB effects",
            Self::LcdFrame => "LCD frame transfer",
            Self::LcdDisplayControl => "LCD display control",
        }
    }
}

/// How far a capability may be exercised right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CapabilityState {
    /// Readable, and writable only when `writable` is true.
    Available {
        writable: bool,
        /// Filesystem path or interface the capability resolves to.
        source: String,
    },
    /// The device exposes it, but the protocol is not validated yet.
    ///
    /// No write is permitted while a capability is in this state. `reason` is
    /// shown verbatim on the control, so it carries its own punctuation.
    Unvalidated { reason: String },
    /// The device does not expose it on this firmware or kernel binding.
    Unavailable { reason: String },
}

impl CapabilityState {
    /// True only for a capability that may be written today.
    pub fn is_writable(&self) -> bool {
        matches!(self, Self::Available { writable: true, .. })
    }

    pub fn is_readable(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    /// Short operator-facing explanation of why a control is disabled.
    pub fn blocked_reason(&self) -> Option<String> {
        match self {
            Self::Available { writable: true, .. } => None,
            Self::Available {
                writable: false,
                source,
            } => Some(format!("Read-only: no write permission on {source}.")),
            Self::Unvalidated { reason } | Self::Unavailable { reason } => Some(reason.clone()),
        }
    }
}

/// One capability with its resolved state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub id: CapabilityId,
    pub state: CapabilityState,
}

/// A USB interface of a probed device, with the driver the kernel bound to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbInterface {
    /// `bInterfaceNumber`.
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// Kernel driver bound to the interface, when any.
    pub driver: Evidenced<String>,
    /// Endpoints the interface publishes, in address order.
    pub endpoints: Vec<UsbEndpoint>,
}

/// One endpoint of a USB interface, as sysfs describes it.
///
/// `direction` and `transfer` are taken from the kernel's own decoded strings
/// rather than re-derived from `bmAttributes`, so the record cannot disagree
/// with what the kernel believes about the same endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbEndpoint {
    /// `bEndpointAddress`.
    pub address: u8,
    /// `in` or `out`, verbatim from sysfs.
    pub direction: String,
    /// `Interrupt`, `Bulk`, `Control` or `Isoc`, verbatim from sysfs.
    pub transfer: String,
    pub max_packet_size: u16,
    /// `bInterval`, in frames or microframes depending on the bus speed.
    pub interval: u8,
}

/// Stable identity of a probed USB device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbIdentity {
    pub id: DeviceId,
    pub manufacturer: Evidenced<String>,
    pub product: Evidenced<String>,
    /// USB serial number. Redacted before any diagnostics export.
    pub serial: Evidenced<String>,
    /// `bcdDevice`, the closest thing to a firmware revision USB exposes.
    pub firmware: Evidenced<String>,
    /// sysfs path the record was built from.
    pub sysfs_path: String,
}

/// One `hwmon` attribute the probe resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwmonAttribute {
    /// Attribute file name, for example `temp1_input`.
    pub name: String,
    pub path: String,
    pub readable: bool,
    pub writable: bool,
    /// Label exposed by the driver, when it publishes one.
    pub label: Evidenced<String>,
}

/// The `hwmon` surface of a device bound to a kernel driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwmonCapabilities {
    /// Driver name, for example `kraken2023`.
    pub driver: String,
    pub path: String,
    pub attributes: Vec<HwmonAttribute>,
    /// Number of `tempN_auto_pointM_pwm` files found per channel.
    pub curve_points: Vec<CurveChannel>,
}

impl HwmonCapabilities {
    pub fn attribute(&self, name: &str) -> Option<&HwmonAttribute> {
        self.attributes.iter().find(|a| a.name == name)
    }
}

/// The onboard curve surface of one PWM channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurveChannel {
    /// `1` for `temp1_auto_point*`.
    pub temp_index: u8,
    pub point_count: u16,
    pub writable: bool,
}

/// What the RGB controller answered about itself.
///
/// Every field is separately evidenced because they fail separately: the node
/// can exist while permission is missing, and permission can be granted while
/// the controller answers nothing. A field that could not be established says
/// so, and the capability state that reads it stays unvalidated rather than
/// falling back to a plausible topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbTopology {
    /// `hidraw` node the transport would use.
    pub node: Evidenced<String>,
    /// Firmware the controller reports, which is not the USB `bcdDevice`.
    pub firmware: Evidenced<String>,
    /// Channels the controller reports, in index order.
    pub channels: Evidenced<Vec<RgbChannel>>,
}

impl RgbTopology {
    /// A topology nothing could be read from, with the reason on every field.
    pub fn unavailable(reason: impl Into<String>, source: impl Into<String>) -> Self {
        let (reason, source) = (reason.into(), source.into());
        Self {
            node: Evidenced::unknown(reason.clone(), source.clone()),
            firmware: Evidenced::unknown(reason.clone(), source.clone()),
            channels: Evidenced::unknown(reason, source),
        }
    }

    /// Number of channels the controller reported, or `None` when unknown.
    pub fn channel_count(&self) -> Option<u8> {
        self.channels.value().map(|channels| channels.len() as u8)
    }
}

/// One addressable lighting channel and what is plugged into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbChannel {
    /// One-based index, matching the label printed on the controller.
    pub index: u8,
    /// Accessories the controller detected on this channel.
    pub accessories: Vec<RgbAccessory>,
    /// LEDs on the channel.
    ///
    /// The controller reports accessory identifiers, not LED counts, so this is
    /// `Unknown` on every accessory whose LED count has not been counted on
    /// real hardware. A control that needs it must read it, not assume it.
    pub led_count: Evidenced<u16>,
}

/// One accessory the controller detected on a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RgbAccessory {
    /// Identifier byte the controller returned.
    pub id: u8,
    /// Name for that identifier, or an explicit unknown marker.
    pub name: String,
}

/// What the Kraken answered about its panel, and what this product asserts.
///
/// The two are deliberately not mixed. `firmware` and `display` come from the
/// device: it answers a report carrying its revision, and another carrying the
/// brightness and orientation it is currently set to. A device with no panel
/// answers neither, which is what makes them evidence rather than decoration.
///
/// `panel` is different. No report on this device reports a resolution, so the
/// geometry is a candidate carried by this product for this exact product id,
/// and its `source` says so. Nothing is written on the strength of a candidate:
/// the capability gate turns on a validated firmware, and a firmware becomes
/// validated only through a write probe an operator watched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcdTopology {
    /// `hidraw` node the display commands travel over.
    pub hid_node: Evidenced<String>,
    /// `usbfs` node the framebuffer's bulk endpoint is reached through.
    pub bulk_node: Evidenced<String>,
    /// Firmware the Kraken reports, which is not the USB `bcdDevice`.
    pub firmware: Evidenced<String>,
    /// Geometry and pixel format a frame must be laid out in.
    pub panel: Evidenced<LcdPanel>,
    /// Brightness and orientation the panel reports it is currently using.
    pub display: Evidenced<LcdDisplaySettings>,
}

impl LcdTopology {
    /// A topology nothing could be read from, with the reason on every field.
    pub fn unavailable(reason: impl Into<String>, source: impl Into<String>) -> Self {
        let (reason, source) = (reason.into(), source.into());
        Self {
            hid_node: Evidenced::unknown(reason.clone(), source.clone()),
            bulk_node: Evidenced::unknown(reason.clone(), source.clone()),
            firmware: Evidenced::unknown(reason.clone(), source.clone()),
            panel: Evidenced::unknown(reason.clone(), source.clone()),
            display: Evidenced::unknown(reason, source),
        }
    }

    /// True when the panel answered the report only a panel can answer.
    pub fn answered(&self) -> bool {
        self.display.is_known()
    }
}

/// The shape of the physical panel behind the framebuffer.
///
/// A circular panel still takes a square framebuffer; the corners simply are
/// not visible. The renderer needs to know which, so nothing meaningful is laid
/// out where the glass ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LcdPanelShape {
    Circular,
    Square,
}

/// Geometry and pixel format one frame must match exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcdPanel {
    pub width: u16,
    pub height: u16,
    pub shape: LcdPanelShape,
    /// Human-readable pixel format, for example `RGB565 big-endian`.
    pub pixel_format: String,
    /// Exact payload size of one frame, in bytes.
    pub frame_bytes: u32,
    /// Bulk endpoint address the framebuffer is written to.
    pub bulk_endpoint: u8,
    /// USB interface that endpoint belongs to.
    pub bulk_interface: u8,
}

/// What the panel says it is currently set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LcdDisplaySettings {
    pub brightness_percent: u8,
    /// Orientation as the device counts it: `0` through `3`, each a quarter turn.
    pub quarter_turns: u8,
}

/// Whether the product is allowed to talk to a discovered device at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SupportState {
    Supported,
    /// Discovered but outside the allowlist. Nothing is opened.
    Unsupported {
        reason: String,
    },
}

/// Everything the probe established about one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub support: SupportState,
    pub usb: UsbIdentity,
    pub interfaces: Vec<UsbInterface>,
    pub hwmon: Option<HwmonCapabilities>,
    /// Present only on the RGB controller, and only once its probe has run.
    pub rgb: Option<RgbTopology>,
    /// Present only on the Kraken, and only once its LCD probe has run.
    pub lcd: Option<LcdTopology>,
    pub capabilities: Vec<Capability>,
}

impl DeviceRecord {
    pub fn id(&self) -> DeviceId {
        self.usb.id
    }

    pub fn is_supported(&self) -> bool {
        matches!(self.support, SupportState::Supported)
    }

    pub fn capability(&self, id: CapabilityId) -> Option<&Capability> {
        self.capabilities.iter().find(|c| c.id == id)
    }

    /// True only when the capability is present, supported and writable.
    pub fn can_write(&self, id: CapabilityId) -> bool {
        self.is_supported() && self.capability(id).is_some_and(|c| c.state.is_writable())
    }
}

/// Context of the machine the probe ran on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeContext {
    pub kernel_release: Evidenced<String>,
    /// Milliseconds since the Unix epoch.
    pub probed_at_unix_ms: u64,
}

/// A versioned, evidence-backed capability record for the whole machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityRecord {
    pub schema_version: u32,
    pub context: ProbeContext,
    /// Allowlisted devices found on the machine.
    pub devices: Vec<DeviceRecord>,
    /// Devices discovered on the bus that the allowlist rejects.
    pub rejected: Vec<RejectedDevice>,
}

/// A device seen during enumeration and deliberately left untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedDevice {
    pub id: DeviceId,
    pub sysfs_path: String,
    pub reason: String,
}

impl CapabilityRecord {
    pub fn device(&self, id: DeviceId) -> Option<&DeviceRecord> {
        self.devices.iter().find(|d| d.id() == id)
    }

    /// Supported devices only, in probe order.
    pub fn supported(&self) -> impl Iterator<Item = &DeviceRecord> {
        self.devices.iter().filter(|d| d.is_supported())
    }

    /// Redact every USB serial number in place.
    ///
    /// Applied before a record leaves the daemon for diagnostics export.
    pub fn redact_serials(&mut self) {
        for device in &mut self.devices {
            device.usb.serial = Evidenced::unknown("redacted", "policy:redact-serials");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KRAKEN_BASE;

    fn record() -> CapabilityRecord {
        CapabilityRecord {
            schema_version: CAPABILITY_SCHEMA_VERSION,
            context: ProbeContext {
                kernel_release: Evidenced::known("7.1.6".into(), "uname"),
                probed_at_unix_ms: 1,
            },
            devices: vec![DeviceRecord {
                support: SupportState::Supported,
                usb: UsbIdentity {
                    id: KRAKEN_BASE,
                    manufacturer: Evidenced::known("NZXT Inc.".into(), "sysfs"),
                    product: Evidenced::known("NZXT Kraken Base".into(), "sysfs"),
                    serial: Evidenced::known("SECRET".into(), "sysfs"),
                    firmware: Evidenced::known("0200".into(), "sysfs"),
                    sysfs_path: "/sys/bus/usb/devices/1-9".into(),
                },
                interfaces: vec![],
                hwmon: None,
                rgb: None,
                lcd: None,
                capabilities: vec![
                    Capability {
                        id: CapabilityId::PumpDuty,
                        state: CapabilityState::Available {
                            writable: false,
                            source: "/sys/class/hwmon/hwmon4/pwm1".into(),
                        },
                    },
                    Capability {
                        id: CapabilityId::LcdFrame,
                        state: CapabilityState::Unvalidated {
                            reason: "LCD transport is not proven on this firmware.".into(),
                        },
                    },
                ],
            }],
            rejected: vec![],
        }
    }

    #[test]
    fn unvalidated_capability_is_never_writable() {
        let record = record();
        let device = record.device(KRAKEN_BASE).unwrap();
        assert!(!device.can_write(CapabilityId::LcdFrame));
        assert!(!device.can_write(CapabilityId::PumpDuty));
        assert!(!device.can_write(CapabilityId::RgbFixedColor));
    }

    #[test]
    fn blocked_reason_explains_why_the_write_is_refused() {
        let record = record();
        let state = &record
            .device(KRAKEN_BASE)
            .unwrap()
            .capability(CapabilityId::LcdFrame)
            .unwrap()
            .state;
        let reason = state.blocked_reason().unwrap();
        assert!(reason.contains("not proven on this firmware"), "{reason}");
    }

    #[test]
    fn redaction_removes_serial_numbers() {
        let mut record = record();
        record.redact_serials();
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("SECRET"), "{serialized}");
        assert!(!record.devices[0].usb.serial.is_known());
    }

    #[test]
    fn an_unreadable_rgb_topology_states_the_reason_on_every_field() {
        let topology = RgbTopology::unavailable("permission denied", "/dev/hidraw12");
        assert_eq!(topology.channel_count(), None);
        assert!(!topology.node.is_known());
        assert!(!topology.firmware.is_known());
        assert!(!topology.channels.is_known());

        // The reason survives serialization, because the client renders it.
        let json = serde_json::to_string(&topology).unwrap();
        assert!(json.contains("permission denied"), "{json}");
    }

    #[test]
    fn a_reported_topology_counts_only_the_channels_it_carries() {
        let topology = RgbTopology {
            node: Evidenced::known("/dev/hidraw12".into(), "sysfs"),
            firmware: Evidenced::known("1.2.3".into(), "report 0x11"),
            channels: Evidenced::known(
                (1..=3)
                    .map(|index| RgbChannel {
                        index,
                        accessories: Vec::new(),
                        led_count: Evidenced::unknown(
                            "the controller reports accessory ids, not LED counts",
                            "report 0x21",
                        ),
                    })
                    .collect(),
                "report 0x21",
            ),
        };
        assert_eq!(topology.channel_count(), Some(3));
        assert!(
            topology
                .channels
                .value()
                .is_some_and(|channels| channels.iter().all(|c| !c.led_count.is_known())),
            "an uncounted LED total must never become a number"
        );
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = record();
        let json = serde_json::to_string(&record).unwrap();
        let parsed: CapabilityRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, parsed);
    }
}
