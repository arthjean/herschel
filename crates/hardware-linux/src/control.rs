// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The only code in this workspace that writes to cooling hardware.
//!
//! Everything goes through the bound `kraken2023` driver. No kernel driver is
//! detached, no USB endpoint is opened, and nothing outside the `pwmN`,
//! `pwmN_enable` and `tempN_auto_pointM_pwm` attributes of one `hwmon`
//! directory is ever touched. The curve ABI stops at 59 C by construction, so
//! no value written here can alter the firmware's behavior above it.
//!
//! Write order matters and is deliberate. `pwmN` is written before
//! `pwmN_enable`, so switching a channel into direct mode applies the duty the
//! operator asked for rather than whatever the driver was holding. The kernel
//! documents mode `0` as running the channel at 100%, mode `1` as direct PWM
//! and mode `2` as the onboard curve.
//!
//! A curve is written through direct mode for the same reason, and it is not a
//! detail. `kraken3_fan_curve_pwm_store` sends the whole 40-point curve to the
//! device on *every* point written while the channel is already in mode `2`, so
//! writing forty points into a channel that is already running a curve is forty
//! HID transfers in a burst. The kernel documents what the firmware does with
//! that: these devices "can lock up or discard the changes if they are too
//! numerous at once", and prescribe setting the points in another mode, then
//! applying them by switching to curve. Below mode `2` a point write only
//! updates the driver's in-memory array, so the sequence here is one transfer,
//! not forty-one. This is what [`CoolingControl::apply_curve`] does.
//!
//! See `Documentation/hwmon/nzxt-kraken3.rst` and `drivers/hwmon/nzxt-kraken3.c`.

use std::io::Write;
use std::path::Path;

use kori_core::ipc::ChannelReadback;
use kori_core::profile::{CURVE_POINT_COUNT, Channel, MAX_DUTY, TemperatureCurve};
use kori_core::telemetry::PwmMode;

use crate::hwmon::{KrakenHwmon, curve_point_attribute, duty_attribute, mode_attribute};

/// A write that did not land, naming the exact attribute.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{attribute}: {detail}")]
pub struct WriteFailure {
    pub attribute: String,
    pub detail: String,
}

impl WriteFailure {
    fn new(attribute: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            attribute: attribute.into(),
            detail: detail.into(),
        }
    }
}

/// What one channel write produced: the readback, and how many attributes it
/// actually touched.
///
/// The count is measured rather than derived from the program's shape, because
/// a curve write costs two extra attributes when the channel has to leave curve
/// mode first. An operator is told how many attributes were written, so that
/// number has to be the one that was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub readback: ChannelReadback,
    pub writes: u32,
}

/// Everything a transaction on one channel may change.
///
/// Captured before the first write so a partial failure has something to
/// restore. A field that could not be read is `None`, and restoration then
/// leaves that attribute alone rather than writing a guess over it.
///
/// The curve is the awkward one. `tempN_auto_pointM_pwm` is write-only on this
/// driver (`--w-------` on the development machine), so a curve can never be
/// read back off the device. The only place a last known-good curve can come
/// from is the daemon's own record of what it last committed, which is sound
/// precisely because the daemon is the sole writer. [`ChannelSnapshot::with_curve`]
/// is how that record is folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSnapshot {
    pub channel: Channel,
    pub mode: Option<PwmMode>,
    pub duty: Option<u8>,
    pub curve: Option<TemperatureCurve>,
}

impl ChannelSnapshot {
    /// Attach the last curve this process committed to the channel.
    pub fn with_curve(mut self, curve: Option<TemperatureCurve>) -> Self {
        if self.curve.is_none() {
            self.curve = curve;
        }
        self
    }

    /// True when restoring this snapshot puts the channel back on the same
    /// program, rather than merely on a plausible one.
    ///
    /// Curve points are inert unless the channel is in curve mode, so a
    /// channel that was on the failsafe or on a fixed duty is fully restored
    /// by its mode and duty alone.
    pub fn restores_behavior(&self) -> bool {
        match self.mode {
            None => false,
            Some(PwmMode::FullSpeed) => true,
            Some(PwmMode::Fixed) => self.duty.is_some(),
            Some(PwmMode::Curve) => self.curve.is_some(),
        }
    }
}

/// The writer for one bound Kraken.
#[derive(Debug, Clone)]
pub struct CoolingControl {
    hwmon: KrakenHwmon,
}

impl CoolingControl {
    pub fn new(hwmon: KrakenHwmon) -> Self {
        Self { hwmon }
    }

    pub fn hwmon(&self) -> &KrakenHwmon {
        &self.hwmon
    }

    /// Capture the last known-good state of one channel.
    ///
    /// `curve` is `Some` only when the driver happens to expose readable curve
    /// points. It does not on the certified platform, so callers fold in their
    /// own record with [`ChannelSnapshot::with_curve`].
    pub fn snapshot(&self, channel: Channel) -> ChannelSnapshot {
        ChannelSnapshot {
            channel,
            mode: self.hwmon.mode(channel).copied(),
            duty: self.hwmon.duty(channel).copied(),
            curve: self.hwmon.curve(channel).ok(),
        }
    }

    /// Put one channel on a constant duty.
    ///
    /// Two attributes, in this order, then a readback of both.
    pub fn apply_fixed(&self, channel: Channel, duty: u8) -> Result<Applied, WriteFailure> {
        self.write(&duty_attribute(channel), duty)?;
        self.write(&mode_attribute(channel), PwmMode::Fixed.to_kernel())?;
        let writes = 2;

        let mut readback = ChannelReadback::new(channel);
        readback.mode = self.hwmon.mode(channel).copied();
        readback.duty = self.hwmon.duty(channel).copied();

        let mut mismatches = Vec::new();
        if readback.mode != Some(PwmMode::Fixed) {
            mismatches.push(format!(
                "{} reads back as {}, not direct PWM",
                mode_attribute(channel),
                describe_mode(readback.mode)
            ));
        }
        if readback.duty != Some(duty) {
            mismatches.push(format!(
                "{} reads back as {}, not {duty}",
                duty_attribute(channel),
                describe_duty(readback.duty)
            ));
        }
        if !mismatches.is_empty() {
            readback.mismatch = Some(mismatches.join("; "));
        }
        Ok(Applied { readback, writes })
    }

    /// Put one channel on an onboard curve.
    ///
    /// Every point is written before the mode switches, so the device never
    /// runs a half-updated curve: while the write is in progress the channel is
    /// still on its previous program.
    pub fn apply_curve(
        &self,
        channel: Channel,
        curve: &TemperatureCurve,
    ) -> Result<Applied, WriteFailure> {
        if curve.points.len() != CURVE_POINT_COUNT {
            return Err(WriteFailure::new(
                curve_point_attribute(channel, 0),
                format!(
                    "a curve must carry exactly {CURVE_POINT_COUNT} points, got {}",
                    curve.points.len()
                ),
            ));
        }

        // Leave curve mode before touching a point, so the forty writes below
        // are driver-memory updates rather than forty curve transfers the
        // firmware is documented to discard. The channel holds the duty it is
        // already running for the width of this transaction, so the transition
        // changes no behavior.
        let mut writes = 0;
        if self.hwmon.mode(channel).copied() == Some(PwmMode::Curve) {
            let holding = self.transition_duty(channel, curve);
            self.write(&duty_attribute(channel), holding)?;
            self.write(&mode_attribute(channel), PwmMode::Fixed.to_kernel())?;
            writes += 2;
        }

        for (index, duty) in curve.points.iter().enumerate() {
            self.write(&curve_point_attribute(channel, index), *duty)?;
        }
        // The one transfer that actually programs the device.
        self.write(&mode_attribute(channel), PwmMode::Curve.to_kernel())?;
        writes += CURVE_POINT_COUNT as u32 + 1;

        let mut readback = ChannelReadback::new(channel);
        readback.mode = self.hwmon.mode(channel).copied();
        readback.duty = self.hwmon.duty(channel).copied();

        let mut mismatches = Vec::new();
        // The curve attributes are write-only on this driver, so an unreadable
        // curve is the normal case and leaves `curve_points_confirmed` at
        // `None`. Only a curve that reads back *wrong* is a mismatch; one that
        // cannot be read at all is recorded as unconfirmed, which is what
        // "readback for every attribute that supports it" means here.
        if let Ok(written) = self.hwmon.curve(channel) {
            let confirmed = written
                .points
                .iter()
                .zip(curve.points.iter())
                .filter(|(read, expected)| read == expected)
                .count();
            readback.curve_points_confirmed = Some(confirmed as u16);
            if confirmed != CURVE_POINT_COUNT {
                mismatches.push(format!(
                    "{confirmed} of {CURVE_POINT_COUNT} curve points read back as written"
                ));
            }
        }
        if readback.mode != Some(PwmMode::Curve) {
            mismatches.push(format!(
                "{} reads back as {}, not curve control",
                mode_attribute(channel),
                describe_mode(readback.mode)
            ));
        }
        if !mismatches.is_empty() {
            readback.mismatch = Some(mismatches.join("; "));
        }
        Ok(Applied { readback, writes })
    }

    /// Put a channel back to a captured state after a failed transaction.
    ///
    /// Only what actually differs is written. That is not an optimization: the
    /// write that failed may be the very attribute a blind restoration would
    /// rewrite, and failing to re-write a value that is already correct would
    /// report an uncertain state for a channel that never moved.
    ///
    /// The curve is rewritten only when the channel was running one, because
    /// curve points are inert in any other mode and a full curve is always
    /// written before a channel is switched into curve mode.
    ///
    /// Returns whether the channel could be proven back on its program. A
    /// `false` here is what turns the reported hardware state uncertain.
    pub fn restore(&self, snapshot: &ChannelSnapshot) -> Result<bool, WriteFailure> {
        let channel = snapshot.channel;

        if snapshot.mode == Some(PwmMode::Curve)
            && let Some(curve) = &snapshot.curve
        {
            // Same reason as `apply_curve`: points written while the channel is
            // still in curve mode are a burst the firmware may discard, and the
            // mode write below is then skipped as unchanged, so the restored
            // curve would never be transferred at all.
            if self.hwmon.mode(channel).copied() == Some(PwmMode::Curve) {
                let holding = self.transition_duty(channel, curve);
                self.write(&duty_attribute(channel), holding)?;
                self.write(&mode_attribute(channel), PwmMode::Fixed.to_kernel())?;
            }
            for (index, duty) in curve.points.iter().enumerate() {
                self.write(&curve_point_attribute(channel, index), *duty)?;
            }
        }
        if let Some(duty) = snapshot.duty
            && self.hwmon.duty(channel).copied() != Some(duty)
        {
            self.write(&duty_attribute(channel), duty)?;
        }
        if let Some(mode) = snapshot.mode
            && self.hwmon.mode(channel).copied() != Some(mode)
        {
            self.write(&mode_attribute(channel), mode.to_kernel())?;
        }

        let mut confirmed = snapshot.restores_behavior();
        if let Some(mode) = snapshot.mode {
            confirmed &= self.hwmon.mode(channel).copied() == Some(mode);
        }
        // A curve that cannot be read back is not evidence of failure: these
        // attributes are write-only. Only a readable curve that disagrees with
        // the snapshot downgrades the result.
        if let (Some(curve), Ok(read)) = (&snapshot.curve, self.hwmon.curve(channel)) {
            confirmed &= &read == curve;
        }
        // The duty a channel reports in curve mode is the one the curve
        // currently dictates, not the value stored in `pwmN`, so it is only
        // compared when the restored mode is direct PWM.
        if snapshot.mode == Some(PwmMode::Fixed)
            && let Some(duty) = snapshot.duty
        {
            confirmed &= self.hwmon.duty(channel).copied() == Some(duty);
        }
        Ok(confirmed)
    }

    /// Write one attribute, or say exactly which one refused.
    /// Duty to hold a channel at while its curve points are being rewritten.
    ///
    /// The duty the device reports is what the channel is running right now, so
    /// holding it changes nothing an operator could hear or measure. When it
    /// cannot be read, the curve's own value at the current liquid temperature
    /// says the same thing one step less directly. With neither available the
    /// last point wins: a curve is validated non-decreasing, so that is its
    /// highest duty, and a transition that cools too hard is safe where one that
    /// cools too little is not.
    fn transition_duty(&self, channel: Channel, curve: &TemperatureCurve) -> u8 {
        if let Some(duty) = self.hwmon.duty(channel).copied() {
            return duty.max(channel.min_duty());
        }
        let from_curve = self
            .hwmon
            .liquid_temperature_c()
            .copied()
            .and_then(|temperature_c| curve.duty_at(temperature_c))
            .or_else(|| curve.points.last().copied());
        from_curve.unwrap_or(MAX_DUTY).max(channel.min_duty())
    }

    fn write(&self, attribute: &str, value: u8) -> Result<(), WriteFailure> {
        let path = self.hwmon.attribute(attribute);
        write_attribute(&path, value)
            .map_err(|error| WriteFailure::new(attribute, describe_io(&path, &error)))
    }
}

/// Write a single sysfs attribute.
///
/// Opened without `O_CREAT`, so a missing attribute fails as missing rather
/// than looking like a file this process could create. `O_TRUNC` is kept: the
/// kernel ignores it on a sysfs attribute, and without it a shorter value
/// written over a longer one would leave trailing digits behind on any backing
/// store that is a real file.
fn write_attribute(path: &Path, value: u8) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    file.write_all(value.to_string().as_bytes())
}

fn describe_io(path: &Path, error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => format!(
            "permission denied on {}. Check the installed udev rule.",
            path.display()
        ),
        std::io::ErrorKind::NotFound => format!("{} does not exist.", path.display()),
        _ => format!("{} could not be written: {error}", path.display()),
    }
}

fn describe_mode(mode: Option<PwmMode>) -> String {
    match mode {
        Some(mode) => mode.label().to_string(),
        None => "unreadable".to_string(),
    }
}

fn describe_duty(duty: Option<u8>) -> String {
    match duty {
        Some(duty) => duty.to_string(),
        None => "unreadable".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeSysfs, running_as_root};
    use kori_core::profile::CurveNodes;

    /// A fixture with an installed udev rule: both channels writable.
    ///
    /// Curve points stay write-only, exactly as the kernel exposes them, so
    /// these tests exercise the same readback gap production code faces.
    fn writable(name: &str) -> (FakeSysfs, std::path::PathBuf, CoolingControl) {
        let fake = FakeSysfs::new(name);
        fake.add_kraken();
        let hwmon = fake.add_kraken_hwmon();
        for channel in 1..=2 {
            fake.grant_write(&hwmon, &format!("pwm{channel}"));
            fake.grant_write(&hwmon, &format!("pwm{channel}_enable"));
            for point in 1..=40 {
                fake.grant_write(&hwmon, &format!("temp{channel}_auto_point{point}_pwm"));
            }
        }
        let located = KrakenHwmon::locate(&fake.root()).expect("the fixture exposes kraken2023");
        (fake, hwmon, CoolingControl::new(located))
    }

    #[test]
    fn a_fixed_duty_writes_two_attributes_and_reads_both_back() {
        let (_fake, _hwmon, control) = writable("control-fixed");

        let applied = control.apply_fixed(Channel::Pump, 180).unwrap();
        assert_eq!(applied.writes, 2);
        let readback = applied.readback;
        assert_eq!(readback.channel, Channel::Pump);
        assert_eq!(readback.mode, Some(PwmMode::Fixed));
        assert_eq!(readback.duty, Some(180));
        assert_eq!(readback.mismatch, None);
        assert!(readback.is_confirmed());

        // The other channel was not touched.
        assert_eq!(
            control.hwmon().mode(Channel::Fan).copied(),
            Some(PwmMode::FullSpeed)
        );
    }

    #[test]
    fn a_curve_writes_forty_points_then_switches_mode() {
        let (fake, hwmon, control) = writable("control-curve");
        let curve = CurveNodes::starting_ramp().interpolate();

        let applied = control.apply_curve(Channel::Fan, &curve).unwrap();
        assert_eq!(
            applied.writes,
            CURVE_POINT_COUNT as u32 + 1,
            "a channel not already on a curve needs no transition"
        );
        let readback = applied.readback;
        assert_eq!(readback.mode, Some(PwmMode::Curve));
        // Write-only attributes: unconfirmed rather than wrong.
        assert_eq!(readback.curve_points_confirmed, None);
        assert_eq!(readback.mismatch, None);
        assert_eq!(fake.written_curve(&hwmon, 2), curve.points);

        // Nothing outside the forty points and the mode moved: the pump is
        // still where it was, so the failsafe above 59 C is untouched.
        assert_eq!(
            control.hwmon().mode(Channel::Pump).copied(),
            Some(PwmMode::FullSpeed)
        );
        assert_eq!(fake.written_curve(&hwmon, 1), vec![0u8; CURVE_POINT_COUNT]);
    }

    #[test]
    fn rewriting_a_curve_leaves_curve_mode_before_touching_a_point() {
        let (fake, hwmon, control) = writable("control-curve-rewrite");
        let first = CurveNodes::starting_ramp().interpolate();
        control.apply_curve(Channel::Fan, &first).unwrap();
        assert_eq!(
            control.hwmon().mode(Channel::Fan).copied(),
            Some(PwmMode::Curve)
        );

        // The second write goes into a channel that is already running a curve.
        // The driver would send the whole curve on every point written in that
        // state, so the channel has to leave it first: two extra attributes,
        // the held duty and the mode.
        let second = CurveNodes::flat(220).interpolate();
        let applied = control.apply_curve(Channel::Fan, &second).unwrap();
        assert_eq!(
            applied.writes,
            CURVE_POINT_COUNT as u32 + 3,
            "the transition through direct mode is two more attributes"
        );
        assert_eq!(applied.readback.mode, Some(PwmMode::Curve));
        assert_eq!(applied.readback.mismatch, None);
        assert_eq!(fake.written_curve(&hwmon, 2), second.points);
    }

    #[test]
    fn the_duty_held_across_a_curve_rewrite_is_the_one_the_channel_reports() {
        let (_fake, _hwmon, control) = writable("control-curve-hold");
        control
            .apply_curve(Channel::Pump, &CurveNodes::flat(200).interpolate())
            .unwrap();
        let running = control
            .hwmon()
            .duty(Channel::Pump)
            .copied()
            .expect("the fixture reports a duty");

        // Whatever the new curve says, the transition holds what the channel
        // was already doing, so nothing audible happens mid-write.
        control
            .apply_curve(Channel::Pump, &CurveNodes::flat(255).interpolate())
            .unwrap();
        assert!(
            running >= Channel::Pump.min_duty(),
            "the held duty never drops below the pump floor"
        );
        assert_eq!(
            control.hwmon().mode(Channel::Pump).copied(),
            Some(PwmMode::Curve),
            "the channel ends on the curve, not on the transition"
        );
    }

    #[test]
    fn a_curve_of_the_wrong_length_is_refused_before_the_first_write() {
        let (fake, hwmon, control) = writable("control-short-curve");

        let error = control
            .apply_curve(
                Channel::Pump,
                &TemperatureCurve {
                    points: vec![120; 39],
                },
            )
            .unwrap_err();
        assert!(error.detail.contains("exactly 40"), "{error}");
        assert_eq!(fake.written_curve(&hwmon, 1), vec![0u8; CURVE_POINT_COUNT]);
        assert_eq!(
            control.hwmon().mode(Channel::Pump).copied(),
            Some(PwmMode::FullSpeed)
        );
    }

    #[test]
    fn an_unwritable_attribute_names_itself_and_points_at_the_udev_rule() {
        if running_as_root() {
            return; // Root ignores the permission bits this test relies on.
        }
        let fake = FakeSysfs::new("control-denied");
        fake.add_kraken();
        fake.add_kraken_hwmon();
        let control =
            CoolingControl::new(KrakenHwmon::locate(&fake.root()).expect("kraken2023 is present"));

        let error = control.apply_fixed(Channel::Pump, 180).unwrap_err();
        assert_eq!(error.attribute, "pwm1");
        assert!(error.detail.contains("udev"), "{error}");
    }

    #[test]
    fn a_snapshot_restores_the_program_the_channel_was_running() {
        let (fake, hwmon, control) = writable("control-restore");
        let original = CurveNodes::flat(120).interpolate();
        control.apply_curve(Channel::Pump, &original).unwrap();

        // The device cannot hand the curve back, so the caller supplies the
        // one it committed.
        let snapshot = control
            .snapshot(Channel::Pump)
            .with_curve(Some(original.clone()));
        assert_eq!(snapshot.mode, Some(PwmMode::Curve));
        assert!(snapshot.restores_behavior());

        // Something else moves the channel, then the snapshot puts it back.
        control.apply_fixed(Channel::Pump, 200).unwrap();
        assert_eq!(
            control.hwmon().mode(Channel::Pump).copied(),
            Some(PwmMode::Fixed)
        );

        assert!(control.restore(&snapshot).unwrap());
        assert_eq!(
            control.hwmon().mode(Channel::Pump).copied(),
            Some(PwmMode::Curve)
        );
        assert_eq!(fake.written_curve(&hwmon, 1), original.points);
    }

    #[test]
    fn a_channel_on_the_failsafe_is_restored_by_its_mode_alone() {
        let (_fake, _hwmon, control) = writable("control-restore-failsafe");
        let snapshot = control.snapshot(Channel::Fan);
        assert_eq!(snapshot.mode, Some(PwmMode::FullSpeed));
        assert!(
            snapshot.restores_behavior(),
            "curve points are inert outside curve mode"
        );

        control
            .apply_curve(Channel::Fan, &CurveNodes::flat(200).interpolate())
            .unwrap();
        assert!(control.restore(&snapshot).unwrap());
        assert_eq!(
            control.hwmon().mode(Channel::Fan).copied(),
            Some(PwmMode::FullSpeed)
        );
    }

    #[test]
    fn a_snapshot_that_read_nothing_cannot_claim_a_restoration() {
        let (_fake, _hwmon, control) = writable("control-partial-snapshot");
        control.apply_fixed(Channel::Fan, 90).unwrap();

        let snapshot = ChannelSnapshot {
            channel: Channel::Fan,
            mode: None,
            duty: None,
            curve: None,
        };
        assert!(!snapshot.restores_behavior());
        assert!(
            !control.restore(&snapshot).unwrap(),
            "an empty snapshot must not report a confirmed restoration"
        );
        // Nothing was written, so the channel stayed exactly where it was.
        assert_eq!(control.hwmon().duty(Channel::Fan).copied(), Some(90));
        assert_eq!(
            control.hwmon().mode(Channel::Fan).copied(),
            Some(PwmMode::Fixed)
        );
    }

    #[test]
    fn a_curve_snapshot_without_its_curve_cannot_restore_behavior() {
        let snapshot = ChannelSnapshot {
            channel: Channel::Pump,
            mode: Some(PwmMode::Curve),
            duty: Some(200),
            curve: None,
        };
        assert!(!snapshot.restores_behavior());
        assert!(
            snapshot
                .clone()
                .with_curve(Some(CurveNodes::flat(90).interpolate()))
                .restores_behavior()
        );
    }

    #[test]
    fn attribute_names_follow_the_kernel_channel_layout() {
        assert_eq!(duty_attribute(Channel::Pump), "pwm1");
        assert_eq!(duty_attribute(Channel::Fan), "pwm2");
        assert_eq!(mode_attribute(Channel::Pump), "pwm1_enable");
        assert_eq!(
            curve_point_attribute(Channel::Pump, 0),
            "temp1_auto_point1_pwm"
        );
        assert_eq!(
            curve_point_attribute(Channel::Fan, CURVE_POINT_COUNT - 1),
            "temp2_auto_point40_pwm"
        );
    }
}
