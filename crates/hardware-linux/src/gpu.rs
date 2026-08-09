// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU load and temperature from the vendor's local management library.
//!
//! The discrete GPU on the certified machine is driven by the NVIDIA
//! proprietary stack, which publishes no `hwmon` instance and no
//! `gpu_busy_percent`: `/sys/class/drm/cardN` carries neither load nor
//! temperature for it. NVML is therefore the only local interface that answers
//! the question, and `nvml-wrapper` loads `libnvidia-ml.so.1` at runtime rather
//! than linking it, so a machine without the driver simply reports the GPU as
//! unavailable.
//!
//! Nothing here reaches the network. NVML is a local `dlopen` of a library the
//! driver package already installed.

use std::ffi::OsStr;

use herschel_core::telemetry::{GpuTelemetry, Reading, Unavailable, clamp_percent};
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

/// Name of the interface, reported alongside the readings.
pub const SOURCE: &str = "NVML";

/// Shared library the NVIDIA driver package installs.
///
/// Named here rather than left to the wrapper's default so the one path this
/// product loads is visible, and so the absent-driver behavior can be
/// exercised by pointing at a library that is not there.
pub const NVML_LIBRARY: &str = "libnvidia-ml.so.1";

/// The GPU this build reads.
///
/// Index 0 is the first device NVML enumerates. The PRD scopes v1 to the
/// development machine's single discrete GPU, so there is no selection to
/// make and none is invented.
pub const DEVICE_INDEX: u32 = 0;

/// A resolved GPU interface, or the reason there is none.
pub struct GpuSensor {
    nvml: Option<Nvml>,
    cause: Option<Unavailable>,
}

impl std::fmt::Debug for GpuSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuSensor")
            .field("available", &self.nvml.is_some())
            .field("cause", &self.cause)
            .finish()
    }
}

impl GpuSensor {
    /// Load NVML from the path the driver package installs it at.
    pub fn open() -> Self {
        Self::open_with_library(OsStr::new(NVML_LIBRARY))
    }

    /// Load NVML from an explicit path.
    ///
    /// This is how the unavailable path is tested without uninstalling a
    /// driver: point it at a library that is not there.
    pub fn open_with_library(path: &OsStr) -> Self {
        Self::from_result(Nvml::builder().lib_path(path).init())
    }

    fn from_result(result: Result<Nvml, nvml_wrapper::error::NvmlError>) -> Self {
        match result {
            Ok(nvml) => Self {
                nvml: Some(nvml),
                cause: None,
            },
            Err(error) => Self {
                nvml: None,
                cause: Some(Unavailable::absent(format!(
                    "NVML is not available on this machine: {error}. GPU metrics stay \
                     unavailable and every other metric keeps updating."
                ))),
            },
        }
    }

    pub fn is_available(&self) -> bool {
        self.nvml.is_some()
    }

    /// One GPU sample.
    ///
    /// Each field is resolved independently, so a driver that answers for the
    /// temperature but not for utilization reports one value and one `N/A`
    /// rather than losing both.
    pub fn sample(&self, at_unix_ms: u64) -> GpuTelemetry {
        let Some(nvml) = &self.nvml else {
            return GpuTelemetry::unavailable(
                at_unix_ms,
                self.cause.clone().unwrap_or_else(|| {
                    Unavailable::absent("No GPU management interface was resolved.")
                }),
            );
        };

        let device = match nvml.device_by_index(DEVICE_INDEX) {
            Ok(device) => device,
            Err(error) => {
                return GpuTelemetry::unavailable(
                    at_unix_ms,
                    Unavailable::absent(format!(
                        "NVML exposes no device at index {DEVICE_INDEX}: {error}"
                    )),
                );
            }
        };

        GpuTelemetry {
            at_unix_ms,
            source: SOURCE.to_string(),
            name: match device.name() {
                Ok(name) => Reading::valid(name),
                Err(error) => Reading::unavailable(Unavailable::unreadable(format!(
                    "NVML did not report a device name: {error}"
                ))),
            },
            load_percent: match device.utilization_rates() {
                Ok(rates) => Reading::valid(clamp_percent(rates.gpu as f32)),
                Err(error) => Reading::unavailable(Unavailable::unreadable(format!(
                    "NVML did not report utilization: {error}"
                ))),
            },
            // NVML reports whole degrees; the value keeps the product's single
            // decimal place when it is formatted, and is not invented here.
            temperature_c: match device.temperature(TemperatureSensor::Gpu) {
                Ok(degrees) => Reading::valid(degrees as f32),
                Err(error) => Reading::unavailable(Unavailable::unreadable(format!(
                    "NVML did not report a die temperature: {error}"
                ))),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_leaves_every_gpu_value_unavailable_with_a_reason() {
        let sensor = GpuSensor::open_with_library(OsStr::new("libnvidia-ml-absent.so.0"));
        assert!(!sensor.is_available());

        let telemetry = sensor.sample(1_700);
        assert_eq!(telemetry.at_unix_ms, 1_700);
        assert_eq!(
            telemetry.source,
            herschel_core::telemetry::GPU_SOURCE_UNAVAILABLE
        );
        for reading in [
            telemetry.load_percent.is_valid(),
            telemetry.temperature_c.is_valid(),
        ] {
            assert!(!reading);
        }
        let detail = telemetry.load_percent.cause().unwrap().detail().to_string();
        assert!(detail.contains("NVML is not available"), "{detail}");
        assert!(
            detail.contains("every other metric keeps updating"),
            "{detail}"
        );
    }

    #[test]
    fn a_present_library_reports_load_and_temperature_in_range() {
        let sensor = GpuSensor::open();
        if !sensor.is_available() {
            // No NVIDIA driver on this machine: the unavailable path is what
            // the test above pins, and there is nothing further to assert.
            return;
        }

        let telemetry = sensor.sample(1_700);
        assert_eq!(telemetry.source, SOURCE);
        if let Some(load) = telemetry.load_percent.copied() {
            assert!((0.0..=100.0).contains(&load), "load {load}");
        }
        if let Some(temperature) = telemetry.temperature_c.copied() {
            assert!((0.0..=125.0).contains(&temperature), "temp {temperature}");
        }
        if let Some(name) = telemetry.name.value() {
            assert!(!name.is_empty());
        }
    }
}
