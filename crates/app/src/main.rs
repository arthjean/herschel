// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! The GPUI client.
//!
//! It opens one window, asks the daemon what the machine exposes and renders
//! the four destinations. It never writes to hardware itself: every control it
//! offers is gated on a capability the daemon confirmed.

use std::process::ExitCode;

use gpui::{AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use nzxt_app::link::LinkState;
use nzxt_app::offline::NoNetwork;
use nzxt_app::shell::{Shell, key_bindings};
use nzxt_app::startup::{
    EXIT_AFTER_FIRST_FRAME_ENV, StartupTrace, detect_backend_from_env, is_enabled,
};
use nzxt_app::theme::{PRODUCT_NAME, WINDOW_HEIGHT, WINDOW_WIDTH};
use nzxt_core::ipc::socket_path_from_env;

fn main() -> ExitCode {
    let mut trace = StartupTrace::start();

    // The backend is checked before GPUI initialises, so a headless or broken
    // session produces a diagnostic instead of a panic inside the graphics
    // stack.
    let backend = match detect_backend_from_env() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("{PRODUCT_NAME}: {error}");
            return ExitCode::FAILURE;
        }
    };

    // A failure deeper in the stack still reaches the operator as one line
    // naming the backend, rather than an unwinding panic message.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!(
            "{PRODUCT_NAME}: the {} backend failed during initialisation. \
             Check that a compositor is running and that a Vulkan driver is installed. \
             Details: {info}",
            backend.name()
        );
        default_hook(info);
    }));

    let socket = socket_path_from_env();
    let link = LinkState::connect(&socket);
    let exit_after_first_frame = is_enabled(EXIT_AFTER_FIRST_FRAME_ENV);

    // GPUI ships an HTTP client whether or not an application wants one.
    // Replacing it with a refusing client makes the local-only guarantee a
    // property of the binary rather than a promise about call sites.
    Application::new()
        .with_http_client(std::sync::Arc::new(NoNetwork))
        .run(move |cx| {
            cx.bind_keys(key_bindings());

            let bounds = Bounds::centered(None, size(WINDOW_WIDTH, WINDOW_HEIGHT), cx);
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(PRODUCT_NAME.into()),
                        ..Default::default()
                    }),
                    // Below this the rail and the work surface cannot both hold
                    // their minimum widths.
                    window_min_size: Some(size(px(760.0), px(520.0))),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Shell::new(link, window, cx)),
            );

            match window {
                Ok(handle) => {
                    cx.activate(true);
                    // Timed on the frame the compositor actually presents, not on
                    // the internal draw `open_window` performs before returning.
                    let _ = handle.update(cx, move |_, window, _| {
                        window.on_next_frame(move |_, cx| {
                            trace.first_frame(backend);
                            if exit_after_first_frame {
                                cx.quit();
                            }
                        });
                    });
                }
                Err(error) => {
                    eprintln!(
                        "{PRODUCT_NAME}: the {} backend could not open a window: {error}",
                        backend.name()
                    );
                    cx.quit();
                }
            }
        });

    ExitCode::SUCCESS
}
