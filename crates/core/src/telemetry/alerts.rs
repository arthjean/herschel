// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Conditions the Cooling screen must surface immediately.
//!
//! This is a state machine over the sample stream, not part of the sampling
//! vocabulary: it consumes [`KrakenTelemetry`] and decides what an operator has
//! to be told right now. Keeping it apart is what lets the streak rules be read
//! and tested without the reading types around them.

use serde::{Deserialize, Serialize};

use super::{ChannelTelemetry, KrakenTelemetry, PwmMode, format_temperature};
use crate::profile::Channel;

/// Liquid temperature at which the firmware failsafe owns the cooler.
///
/// The kernel curve ABI stops at 59 C by construction, so nothing this product
/// writes can alter behavior at or above this temperature. The threshold is
/// here so the interface can say so, not so it can intervene.
///
/// What the firmware does from here up is only documented for the pump: the
/// liquidctl Kraken X3/Z3 guide records that pump speed is forcibly programmed
/// to 100% at 60 C and above. No primary source says the fan is treated the
/// same way, so [`SafetyAlert::LiquidCritical`] does not claim it is. An
/// unproven failsafe stated as a fact is worse than none, because it is the
/// sentence that tells an operator not to intervene.
pub const LIQUID_CRITICAL_C: f32 = 60.0;

/// Consecutive zero-RPM samples that turn a commanded channel into an alert.
pub const STALL_SAMPLE_COUNT: u8 = 3;

/// A condition the Cooling screen must surface immediately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "alert", rename_all = "snake_case")]
pub enum SafetyAlert {
    /// The coolant is at or above the failsafe threshold.
    LiquidCritical {
        temperature_c: f32,
        threshold_c: f32,
    },
    /// A commanded channel has reported zero RPM for several samples.
    ChannelStalled {
        channel: Channel,
        commanded_duty: u8,
        samples: u8,
        rpm: u32,
    },
}

impl SafetyAlert {
    /// The channel this alert is about, when it names one.
    pub fn channel(&self) -> Option<Channel> {
        match self {
            Self::LiquidCritical { .. } => None,
            Self::ChannelStalled { channel, .. } => Some(*channel),
        }
    }

    /// One sentence naming the affected channel and the current readback.
    pub fn message(&self) -> String {
        match self {
            Self::LiquidCritical {
                temperature_c,
                threshold_c,
            } => format!(
                "Liquid temperature is {} C, at or above the {threshold_c:.0} C failsafe \
                 threshold. The firmware forces the pump to 100% from here up. Whether it does \
                 the same for the fan is not established, so treat the fan as still running the \
                 curve. This application overrides neither.",
                format_temperature(*temperature_c)
            ),
            Self::ChannelStalled {
                channel,
                commanded_duty,
                samples,
                rpm,
            } => format!(
                "{channel} reports {rpm} RPM for {samples} consecutive samples while a duty of \
                 {commanded_duty}/255 is commanded. Readback shows the channel is not turning."
            ),
        }
    }
}

/// Turns a stream of Kraken samples into the alerts the Cooling screen shows.
///
/// A single zero-RPM reading is not a fault: a tachometer misses a pulse now
/// and then. [`STALL_SAMPLE_COUNT`] consecutive zero readings on a channel that
/// is being commanded to turn is, and at one sample per second the condition
/// surfaces well inside the two seconds a stall may stay unreported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlertTracker {
    pump_zero_samples: u8,
    fan_zero_samples: u8,
}

impl AlertTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one sample in and return every alert currently active.
    pub fn observe(&mut self, telemetry: &KrakenTelemetry) -> Vec<SafetyAlert> {
        let mut alerts = Vec::new();

        if let Some(temperature_c) = telemetry.liquid_temperature_c.copied()
            && temperature_c >= LIQUID_CRITICAL_C
        {
            alerts.push(SafetyAlert::LiquidCritical {
                temperature_c,
                threshold_c: LIQUID_CRITICAL_C,
            });
        }

        for channel in [Channel::Pump, Channel::Fan] {
            let entry = telemetry.channel(channel);
            let commanded = commanded_duty(entry);
            let rpm = entry.rpm.copied();

            let counter = match channel {
                Channel::Pump => &mut self.pump_zero_samples,
                Channel::Fan => &mut self.fan_zero_samples,
            };

            match (commanded, rpm) {
                (Some(duty), Some(0)) if duty > 0 => {
                    *counter = counter.saturating_add(1);
                    if *counter >= STALL_SAMPLE_COUNT {
                        alerts.push(SafetyAlert::ChannelStalled {
                            channel,
                            commanded_duty: duty,
                            samples: *counter,
                            rpm: 0,
                        });
                    }
                }
                // An unreadable sample proves nothing either way, so the streak
                // is neither advanced nor cleared by it.
                (None, _) | (_, None) => {}
                _ => *counter = 0,
            }
        }

        alerts
    }
}

/// The duty a channel is currently being told to run at.
///
/// Mode 0 is the firmware failsafe, which commands 100% whatever `pwmN` holds.
fn commanded_duty(entry: &ChannelTelemetry) -> Option<u8> {
    match entry.mode.copied()? {
        PwmMode::FullSpeed => Some(255),
        PwmMode::Fixed | PwmMode::Curve => entry.duty.copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{Reading, Unavailable};

    fn kraken(temperature_c: f32, mode: PwmMode, duty: u8, pump_rpm: u32) -> KrakenTelemetry {
        KrakenTelemetry {
            at_unix_ms: 0,
            present: true,
            liquid_temperature_c: Reading::valid(temperature_c),
            pump: ChannelTelemetry {
                channel: Channel::Pump,
                rpm: Reading::valid(pump_rpm),
                duty: Reading::valid(duty),
                mode: Reading::valid(mode),
            },
            fan: ChannelTelemetry {
                channel: Channel::Fan,
                rpm: Reading::valid(1_700),
                duty: Reading::valid(duty),
                mode: Reading::valid(mode),
            },
        }
    }

    #[test]
    fn a_stalled_channel_alert_names_the_channel_and_its_readback() {
        let alert = SafetyAlert::ChannelStalled {
            channel: Channel::Pump,
            commanded_duty: 180,
            samples: STALL_SAMPLE_COUNT,
            rpm: 0,
        };
        let message = alert.message();
        assert_eq!(alert.channel(), Some(Channel::Pump));
        assert!(message.contains("Pump"), "{message}");
        assert!(message.contains("180"), "{message}");
        assert!(message.contains("0 RPM"), "{message}");
    }

    #[test]
    fn the_critical_alert_states_that_the_failsafe_is_not_overridden() {
        let alert = SafetyAlert::LiquidCritical {
            temperature_c: 61.4,
            threshold_c: LIQUID_CRITICAL_C,
        };
        let message = alert.message();
        assert!(message.contains("61.4"), "{message}");
        assert!(message.contains("overrides neither"), "{message}");
        assert_eq!(alert.channel(), None);
    }

    /// The failsafe above 60 C is documented for the pump only. Claiming it
    /// covers the fan would be a fabricated capability in the one sentence that
    /// tells an operator they do not need to act.
    #[test]
    fn the_critical_alert_claims_the_failsafe_only_where_it_is_evidenced() {
        let message = SafetyAlert::LiquidCritical {
            temperature_c: 62.0,
            threshold_c: LIQUID_CRITICAL_C,
        }
        .message();

        assert!(message.contains("pump to 100%"), "{message}");
        assert!(
            !message.contains("both channels"),
            "the fan failsafe is not evidenced and must not be stated: {message}"
        );
        assert!(
            message.contains("not established"),
            "the gap has to be named, not left out: {message}"
        );
    }

    #[test]
    fn a_single_zero_rpm_sample_is_not_yet_a_stall() {
        let mut tracker = AlertTracker::new();
        let stalled = kraken(30.0, PwmMode::Fixed, 180, 0);

        assert!(tracker.observe(&stalled).is_empty());
        assert!(tracker.observe(&stalled).is_empty());

        let alerts = tracker.observe(&stalled);
        assert_eq!(alerts.len(), 1);
        match &alerts[0] {
            SafetyAlert::ChannelStalled {
                channel,
                commanded_duty,
                samples,
                rpm,
            } => {
                assert_eq!(*channel, Channel::Pump);
                assert_eq!(*commanded_duty, 180);
                assert_eq!(*samples, STALL_SAMPLE_COUNT);
                assert_eq!(*rpm, 0);
            }
            other => panic!("expected a stall, got {other:?}"),
        }

        // One good sample clears the streak.
        tracker.observe(&kraken(30.0, PwmMode::Fixed, 180, 2_900));
        assert!(tracker.observe(&stalled).is_empty());
    }

    #[test]
    fn a_channel_commanded_to_zero_is_not_a_stall() {
        let mut tracker = AlertTracker::new();
        let idle = kraken(30.0, PwmMode::Fixed, 0, 0);
        for _ in 0..5 {
            assert!(tracker.observe(&idle).is_empty());
        }
    }

    #[test]
    fn the_failsafe_mode_counts_as_a_full_duty_command() {
        let mut tracker = AlertTracker::new();
        // `pwmN` can hold anything in mode 0: the channel still runs at 100%.
        let stalled = kraken(30.0, PwmMode::FullSpeed, 0, 0);
        for _ in 0..STALL_SAMPLE_COUNT {
            let _ = tracker.observe(&stalled);
        }
        let alerts = tracker.observe(&stalled);
        assert!(matches!(
            alerts.first(),
            Some(SafetyAlert::ChannelStalled {
                commanded_duty: 255,
                ..
            })
        ));
    }

    #[test]
    fn an_unreadable_sample_neither_raises_nor_clears_a_stall() {
        let mut tracker = AlertTracker::new();
        let stalled = kraken(30.0, PwmMode::Fixed, 180, 0);
        tracker.observe(&stalled);
        tracker.observe(&stalled);

        let mut unreadable = stalled.clone();
        unreadable.pump.rpm = Reading::unavailable(Unavailable::unreadable("EIO"));
        assert!(tracker.observe(&unreadable).is_empty());

        // The streak survived the gap rather than restarting from zero.
        assert!(!tracker.observe(&stalled).is_empty());
    }

    #[test]
    fn the_liquid_alert_fires_on_the_first_sample_at_the_threshold() {
        let mut tracker = AlertTracker::new();
        assert!(
            tracker
                .observe(&kraken(59.9, PwmMode::Curve, 200, 2_900))
                .is_empty()
        );
        let alerts = tracker.observe(&kraken(60.0, PwmMode::Curve, 200, 2_900));
        assert!(matches!(
            alerts.first(),
            Some(SafetyAlert::LiquidCritical { .. })
        ));
    }

    /// The two channels count separately. A stalled pump must not put the fan
    /// one sample from an alert it has not earned.
    #[test]
    fn the_two_channels_keep_their_own_streaks() {
        let mut tracker = AlertTracker::new();
        let mut stalled_pump = kraken(30.0, PwmMode::Fixed, 180, 0);
        stalled_pump.fan.rpm = Reading::valid(1_700);

        for _ in 0..STALL_SAMPLE_COUNT {
            tracker.observe(&stalled_pump);
        }
        let alerts = tracker.observe(&stalled_pump);
        assert!(
            alerts
                .iter()
                .all(|alert| alert.channel() == Some(Channel::Pump)),
            "{alerts:?}"
        );

        // Now stall the fan too: it needs its own full streak.
        let mut both = stalled_pump.clone();
        both.fan.rpm = Reading::valid(0);
        for _ in 0..STALL_SAMPLE_COUNT - 1 {
            let alerts = tracker.observe(&both);
            assert!(
                !alerts
                    .iter()
                    .any(|alert| alert.channel() == Some(Channel::Fan)),
                "the fan alerted before its own streak was met: {alerts:?}"
            );
        }
        let alerts = tracker.observe(&both);
        assert!(
            alerts
                .iter()
                .any(|alert| alert.channel() == Some(Channel::Fan)),
            "{alerts:?}"
        );
    }
}
