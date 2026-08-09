// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Launch preflight and startup instrumentation.
//!
//! GPUI initializes a display backend before any of this crate's code runs, so
//! an unusable environment has to be diagnosed *before* the framework is
//! entered. The alternative is a panic inside a graphics stack, which tells an
//! operator nothing about what to fix.

use std::time::Instant;

/// Environment variable that makes the process report its startup timing.
///
/// Set to `1` to print the milliseconds from process start to the first
/// painted frame, which is how the cold-start budget is measured.
pub const STARTUP_TRACE_ENV: &str = "HERSCHEL_STARTUP_TRACE";

/// Environment variable that closes the window after the first frame.
///
/// Used by the cold-start measurement so a launch can be timed without a human
/// closing the window.
pub const EXIT_AFTER_FIRST_FRAME_ENV: &str = "HERSCHEL_EXIT_AFTER_FIRST_FRAME";

/// A display backend the process can run against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Wayland,
    X11,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Wayland => "Wayland",
            Self::X11 => "X11",
        }
    }
}

/// Why the process cannot open a window, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendUnavailable {
    pub attempted: &'static str,
    pub next_action: String,
}

impl std::fmt::Display for BackendUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} unavailable. {}", self.attempted, self.next_action)
    }
}

/// Decide which backend to use from the environment.
///
/// Wayland wins when both are present, matching GPUI's own preference. The
/// error names the backend that was attempted and the next diagnostic step,
/// rather than reporting a generic initialization failure.
pub fn detect_backend(
    wayland_display: Option<&str>,
    x11_display: Option<&str>,
) -> Result<Backend, BackendUnavailable> {
    let wayland = wayland_display.filter(|value| !value.is_empty());
    let x11 = x11_display.filter(|value| !value.is_empty());

    match (wayland, x11) {
        (Some(_), _) => Ok(Backend::Wayland),
        (None, Some(_)) => Ok(Backend::X11),
        (None, None) => Err(BackendUnavailable {
            attempted: "Wayland and X11",
            next_action: "Neither WAYLAND_DISPLAY nor DISPLAY is set. Run this from a graphical \
                          session, or set DISPLAY to a reachable X server."
                .to_string(),
        }),
    }
}

/// Read the backend choice from the real environment.
pub fn detect_backend_from_env() -> Result<Backend, BackendUnavailable> {
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let x11 = std::env::var("DISPLAY").ok();
    detect_backend(wayland.as_deref(), x11.as_deref())
}

/// Measures time from process start to first painted frame.
pub struct StartupTrace {
    started: Instant,
    enabled: bool,
    reported: bool,
}

impl StartupTrace {
    /// Start the clock, honoring [`STARTUP_TRACE_ENV`].
    pub fn start() -> Self {
        Self {
            started: Instant::now(),
            enabled: is_enabled(STARTUP_TRACE_ENV),
            reported: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Report the first frame. Later calls do nothing.
    pub fn first_frame(&mut self, backend: Backend) {
        if self.reported {
            return;
        }
        self.reported = true;
        if self.enabled {
            println!(
                "startup: first frame after {:.1} ms on {}",
                self.started.elapsed().as_secs_f64() * 1000.0,
                backend.name()
            );
        }
    }

    pub fn has_reported(&self) -> bool {
        self.reported
    }
}

/// Whether a boolean environment variable is set to something truthy.
pub fn is_enabled(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) => matches!(value.trim(), "1" | "true" | "yes" | "on"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_wins_when_both_backends_are_present() {
        assert_eq!(
            detect_backend(Some("wayland-0"), Some(":0")),
            Ok(Backend::Wayland)
        );
    }

    #[test]
    fn x11_is_used_when_wayland_is_absent() {
        assert_eq!(detect_backend(None, Some(":0")), Ok(Backend::X11));
        assert_eq!(detect_backend(Some(""), Some(":0")), Ok(Backend::X11));
    }

    #[test]
    fn no_backend_reports_what_was_attempted_and_the_next_action() {
        let error = detect_backend(None, None).unwrap_err();
        assert_eq!(error.attempted, "Wayland and X11");
        assert!(error.next_action.contains("WAYLAND_DISPLAY"), "{error}");
        assert!(error.next_action.contains("DISPLAY"), "{error}");
        assert!(error.to_string().starts_with("Wayland and X11 unavailable"));
    }

    #[test]
    fn empty_variables_count_as_absent() {
        assert!(detect_backend(Some(""), Some("")).is_err());
        assert!(detect_backend(Some(""), None).is_err());
    }

    #[test]
    fn the_first_frame_is_reported_once() {
        let mut trace = StartupTrace {
            started: Instant::now(),
            enabled: false,
            reported: false,
        };
        assert!(!trace.has_reported());
        trace.first_frame(Backend::Wayland);
        assert!(trace.has_reported());
        trace.first_frame(Backend::X11);
        assert!(trace.has_reported());
    }

    #[test]
    fn truthy_environment_values_are_recognized() {
        let key = "HERSCHEL_TEST_FLAG_TRUTHY";
        for value in ["1", "true", "yes", "on", " 1 "] {
            unsafe { std::env::set_var(key, value) };
            assert!(is_enabled(key), "{value:?} should be truthy");
        }
        for value in ["0", "false", "", "no"] {
            unsafe { std::env::set_var(key, value) };
            assert!(!is_enabled(key), "{value:?} should be falsy");
        }
        unsafe { std::env::remove_var(key) };
    }
}
