// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The window's own decorations: a caption bar and a resizable frame.
//!
//! The window asks the compositor for client-side decorations, so nothing else
//! draws a title bar, a border or a resize handle for it. This module is that
//! chrome, and it is the same one Paneflow draws: a 36-pixel bar carrying the
//! caption buttons on its trailing edge, a rounded one-pixel frame, and an
//! invisible band around the window that starts a resize.
//!
//! Two things stay the compositor's decision rather than this window's. Under
//! server-side decorations the caption buttons are not drawn at all, because the
//! compositor already drew them and two sets would double up. And every corner
//! is rounded only while the matching edges are free: a tiled or maximized
//! window has no gap to round into, so a radius there would carve a notch out
//! of the screen instead of softening a corner.
//!
//! GPUI masks an element's children to a rectangle rather than to its radius,
//! so nothing the window contains may paint an opaque fill into a corner. The
//! shell keeps that true by insetting its cards from the window edge and
//! leaving the ground to the surface this module paints.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, CursorStyle, Decorations, Div, HitboxBehavior, MouseButton, Pixels, Point, ResizeEdge,
    Size, Stateful, Tiling, Window, WindowControlArea, WindowControls, canvas, div, point,
    prelude::*, px, size,
};

use crate::assets::Icon;
use crate::components::{ICON_SIZE, icon};
use crate::theme::{
    RESIZE_BORDER, TITLE_BAR_CONTROL, TITLE_BAR_CONTROL_GAP, TITLE_BAR_INSET, TITLE_BAR_MIN_HEIGHT,
    WINDOW_BORDER, WINDOW_RADIUS, color,
};

/// Whether the pointer is holding the bar, waiting for a move to start.
///
/// The move is not started on the press itself. A press that never moves is a
/// click, and a double click is how a window is maximized: handing the pointer
/// to the compositor on the way down would swallow both.
pub type DragLatch = Rc<Cell<bool>>;

/// Height of the caption bar at the current interface scale.
///
/// Tied to the root font size so the bar grows with a 200% scale instead of
/// clipping the controls it carries, and floored so it never becomes shorter
/// than a pointer target at 100%.
pub fn title_bar_height(window: &Window) -> Pixels {
    let scaled = window.rem_size() * 1.75;
    if scaled > TITLE_BAR_MIN_HEIGHT {
        scaled
    } else {
        TITLE_BAR_MIN_HEIGHT
    }
}

/// Which corners the window is rounding right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Corners {
    top_left: bool,
    top_right: bool,
    bottom_left: bool,
    bottom_right: bool,
}

impl Corners {
    /// A corner is rounded only when both of its edges are free to float.
    fn from_tiling(tiling: Tiling) -> Self {
        let (top, bottom) = (!tiling.top, !tiling.bottom);
        let (left, right) = (!tiling.left, !tiling.right);
        Self {
            top_left: top && left,
            top_right: top && right,
            bottom_left: bottom && left,
            bottom_right: bottom && right,
        }
    }
}

/// The caption bar: an empty drag strip, then the window's own controls.
///
/// It has no surface of its own, neither a fill nor a divider: it is laid over
/// the top of the window so the controls sit inside whatever card the shell put
/// there, exactly as Paneflow's do inside its sidebar. Everything below it has
/// to reserve [`title_bar_height`] so nothing is painted under the strip.
pub fn title_bar(window: &Window, drag: &DragLatch) -> Stateful<Div> {
    let height = title_bar_height(window);
    let supported = window.window_controls();
    let maximized = window.is_maximized();
    // A fullscreen window has no caption buttons to offer, and under
    // server-side decorations the compositor already drew them.
    let controls = !window.is_fullscreen()
        && matches!(window.window_decorations(), Decorations::Client { .. });

    let press = Rc::clone(drag);
    let release = Rc::clone(drag);
    let leave = Rc::clone(drag);
    let held = Rc::clone(drag);

    div()
        .id("title-bar")
        // What tells the compositor this strip moves the window, so a drag
        // that starts here is a window move even where the toolkit protocol,
        // rather than this process, performs it.
        .window_control_area(WindowControlArea::Drag)
        .flex()
        .items_center()
        .flex_none()
        .w_full()
        .h(height)
        .on_mouse_down(MouseButton::Left, move |_, _, _| press.set(true))
        .on_mouse_up(MouseButton::Left, move |_, _, _| release.set(false))
        .on_mouse_down_out(move |_, _, _| leave.set(false))
        .on_mouse_move(move |_, window, _| {
            // The latch is taken, not read: the compositor owns the pointer
            // from here, and this window will not see the release that ends
            // the gesture it just handed over.
            if held.replace(false) {
                window.start_window_move();
            }
        })
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                window.zoom_window();
            }
        })
        // The desktop's own window menu, where the compositor offers one.
        .when(supported.window_menu, |bar| {
            bar.on_mouse_down(MouseButton::Right, |event, window, _| {
                window.show_window_menu(event.position);
            })
        })
        .when(controls, |bar| {
            bar.child(control_group(supported, maximized))
        })
        // The bar carries no label, so everything after the controls is drag
        // surface.
        .child(div().flex_1().min_w_0())
}

/// One caption button.
///
/// Named here rather than taken from GPUI: the crate describes the control
/// areas a platform understands, not the buttons a window chooses to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Minimize,
    Maximize,
    Close,
}

/// The caption buttons the compositor says this platform has.
///
/// Leading edge, in `close, minimize, maximize` order: that is the layout this
/// desktop is set to (`org.gnome.desktop.wm.preferences button-layout` reads
/// `close,minimize,maximize:`) and the one Paneflow ends up drawing. Paneflow
/// reads it from the desktop at runtime through `App::button_layout`, which the
/// published `gpui` 0.2.2 does not carry, so the side is stated here instead of
/// followed. A desktop that keeps its buttons on the right needs this changed,
/// not a preference.
fn control_group(supported: WindowControls, maximized: bool) -> Div {
    div()
        .flex()
        .flex_none()
        // Pinned to the leading edge, whatever else the bar ever carries.
        .mr_auto()
        .items_center()
        .gap(TITLE_BAR_CONTROL_GAP)
        .px(TITLE_BAR_INSET)
        // A press on a button is not a press on the bar. Without this the same
        // press would also arm the drag latch, and moving the pointer while
        // closing the window would start a window move instead.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        // Close is not conditional: a window that cannot be closed from its own
        // caption bar is one an operator has to reach the compositor to quit.
        .child(control_button(Control::Close, maximized))
        .when(supported.minimize, |group| {
            group.child(control_button(Control::Minimize, maximized))
        })
        .when(supported.maximize, |group| {
            group.child(control_button(Control::Maximize, maximized))
        })
}

fn control_button(control: Control, maximized: bool) -> Stateful<Div> {
    let (id, area, glyph) = match control {
        Control::Minimize => (
            "window-minimize",
            WindowControlArea::Min,
            Icon::WindowMinimize,
        ),
        Control::Maximize => (
            "window-maximize",
            WindowControlArea::Max,
            if maximized {
                Icon::WindowRestore
            } else {
                Icon::WindowMaximize
            },
        ),
        Control::Close => ("window-close", WindowControlArea::Close, Icon::WindowClose),
    };

    div()
        .id(id)
        // The compositor hit-tests these rectangles itself, so a platform that
        // performs minimize, maximize or close on its own side finds them where
        // it expects them, and the click handler below covers the rest.
        .window_control_area(area)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(TITLE_BAR_CONTROL)
        .rounded_full()
        .cursor_pointer()
        .hover(|button| button.bg(color::CONTROL.hsla()))
        .active(|button| button.bg(color::SEPARATOR.hsla()))
        .child(icon(glyph, ICON_SIZE, color::TEXT.hsla()))
        .on_click(move |_, window, _| match control {
            Control::Minimize => window.minimize_window(),
            Control::Maximize => window.zoom_window(),
            Control::Close => window.remove_window(),
        })
}

/// Wrap the window's content in the frame the compositor is not drawing.
///
/// The outer inset is the compositor's shadow and the resize hitbox, so it stays
/// transparent. The application surface is the inner element, and it drops its
/// radius, its border and its inset edge by edge as the window is tiled.
pub fn window_shell(content: impl IntoElement, window: &mut Window, background: gpui::Hsla) -> Div {
    let decorations = window.window_decorations();
    let Decorations::Client { tiling } = decorations else {
        window.set_client_inset(px(0.0));
        return div().size_full().bg(background).child(content);
    };

    // What the compositor leaves outside the surface for the resize band.
    window.set_client_inset(RESIZE_BORDER);

    let round = Corners::from_tiling(tiling);
    let (free_top, free_bottom) = (!tiling.top, !tiling.bottom);
    let (free_left, free_right) = (!tiling.left, !tiling.right);

    let surface = div()
        .size_full()
        .bg(background)
        .border_color(color::SEPARATOR.hsla())
        .when(round.top_left, |this| this.rounded_tl(WINDOW_RADIUS))
        .when(round.top_right, |this| this.rounded_tr(WINDOW_RADIUS))
        .when(round.bottom_left, |this| this.rounded_bl(WINDOW_RADIUS))
        .when(round.bottom_right, |this| this.rounded_br(WINDOW_RADIUS))
        .when(free_top, |this| this.border_t(WINDOW_BORDER))
        .when(free_bottom, |this| this.border_b(WINDOW_BORDER))
        .when(free_left, |this| this.border_l(WINDOW_BORDER))
        .when(free_right, |this| this.border_r(WINDOW_BORDER))
        .when(!tiling.is_tiled(), |this| {
            this.shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0.0, 0.0, 0.0, 0.4),
                offset: point(px(0.0), px(0.0)),
                blur_radius: RESIZE_BORDER / 2.0,
                spread_radius: px(0.0),
            }])
        })
        .child(content);

    div()
        .relative()
        .size_full()
        .bg(gpui::transparent_black())
        .when(free_top, |this| this.pt(RESIZE_BORDER))
        .when(free_bottom, |this| this.pb(RESIZE_BORDER))
        .when(free_left, |this| this.pl(RESIZE_BORDER))
        .when(free_right, |this| this.pr(RESIZE_BORDER))
        .on_mouse_down(MouseButton::Left, move |event, window, _| {
            let size = window.window_bounds().get_bounds().size;
            if let Some(edge) = resize_edge(event.position, RESIZE_BORDER, size, tiling) {
                window.start_window_resize(edge);
            }
        })
        .child(surface)
        // The cursor is set during paint rather than from a move handler: it
        // has to be right the moment the pointer crosses the band, including
        // when the window moved under a pointer that never did.
        .child(
            canvas(
                |_, window, _| {
                    window.insert_hitbox(
                        Bounds::new(
                            point(px(0.0), px(0.0)),
                            window.window_bounds().get_bounds().size,
                        ),
                        HitboxBehavior::Normal,
                    )
                },
                move |_, hitbox, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    let Some(edge) =
                        resize_edge(window.mouse_position(), RESIZE_BORDER, size, tiling)
                    else {
                        return;
                    };
                    window.set_cursor_style(
                        match edge {
                            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
                            ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                CursorStyle::ResizeUpLeftDownRight
                            }
                            ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                CursorStyle::ResizeUpRightDownLeft
                            }
                        },
                        &hitbox,
                    );
                },
            )
            .absolute()
            .size_full(),
        )
}

/// The resize the pointer is over, if any.
///
/// A tiled edge is not resizable, so it answers nothing there: dragging the
/// shared edge of a tiled window is the compositor's gesture, not this one.
/// Corners are tested first and are half again as wide as an edge, because a
/// corner an operator has to hit within ten pixels is a corner they miss.
pub fn resize_edge(
    position: Point<Pixels>,
    border: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let inner = Bounds::new(Point::default(), window_size).inset(border * 1.5);
    if inner.contains(&position) {
        return None;
    }

    let corner = size(border * 1.5, border * 1.5);
    let free = |a: bool, b: bool| !a && !b;

    if free(tiling.top, tiling.left)
        && Bounds::new(point(px(0.0), px(0.0)), corner).contains(&position)
    {
        return Some(ResizeEdge::TopLeft);
    }
    if free(tiling.top, tiling.right)
        && Bounds::new(point(window_size.width - corner.width, px(0.0)), corner).contains(&position)
    {
        return Some(ResizeEdge::TopRight);
    }
    if free(tiling.bottom, tiling.left)
        && Bounds::new(point(px(0.0), window_size.height - corner.height), corner)
            .contains(&position)
    {
        return Some(ResizeEdge::BottomLeft);
    }
    if free(tiling.bottom, tiling.right)
        && Bounds::new(
            point(
                window_size.width - corner.width,
                window_size.height - corner.height,
            ),
            corner,
        )
        .contains(&position)
    {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && position.y <= border {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && position.y >= window_size.height - border {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && position.x <= border {
        Some(ResizeEdge::Left)
    } else if !tiling.right && position.x >= window_size.width - border {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_window_rounds_every_corner() {
        assert_eq!(
            Corners::from_tiling(Tiling::default()),
            Corners {
                top_left: true,
                top_right: true,
                bottom_left: true,
                bottom_right: true,
            }
        );
    }

    #[test]
    fn a_tiled_edge_squares_both_of_its_corners() {
        let corners = Corners::from_tiling(Tiling {
            top: true,
            ..Tiling::default()
        });
        assert!(!corners.top_left);
        assert!(!corners.top_right);
        assert!(corners.bottom_left);
        assert!(corners.bottom_right);
    }

    #[test]
    fn a_maximized_window_rounds_nothing() {
        assert_eq!(Corners::from_tiling(Tiling::tiled()), Corners::default());
    }

    #[test]
    fn the_middle_of_the_window_starts_no_resize() {
        let window = size(px(900.0), px(600.0));
        assert_eq!(
            resize_edge(
                point(px(400.0), px(300.0)),
                px(10.0),
                window,
                Tiling::default()
            ),
            None
        );
    }

    #[test]
    fn each_edge_and_corner_answers_with_its_own_direction() {
        let window = size(px(900.0), px(600.0));
        let border = px(10.0);
        let free = Tiling::default();
        let at = |x: f32, y: f32| resize_edge(point(px(x), px(y)), border, window, free);

        assert_eq!(at(2.0, 2.0), Some(ResizeEdge::TopLeft));
        assert_eq!(at(898.0, 2.0), Some(ResizeEdge::TopRight));
        assert_eq!(at(2.0, 598.0), Some(ResizeEdge::BottomLeft));
        assert_eq!(at(898.0, 598.0), Some(ResizeEdge::BottomRight));
        assert_eq!(at(450.0, 2.0), Some(ResizeEdge::Top));
        assert_eq!(at(450.0, 598.0), Some(ResizeEdge::Bottom));
        assert_eq!(at(2.0, 300.0), Some(ResizeEdge::Left));
        assert_eq!(at(898.0, 300.0), Some(ResizeEdge::Right));
    }

    #[test]
    fn a_tiled_edge_is_the_compositors_to_drag_not_this_windows() {
        let window = size(px(900.0), px(600.0));
        let tiling = Tiling {
            left: true,
            ..Tiling::default()
        };
        assert_eq!(
            resize_edge(point(px(2.0), px(300.0)), px(10.0), window, tiling),
            None
        );
        // The free side of the same window still resizes.
        assert_eq!(
            resize_edge(point(px(898.0), px(300.0)), px(10.0), window, tiling),
            Some(ResizeEdge::Right)
        );
    }
}
