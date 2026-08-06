//! The circular LCD preview, painted rather than composed from boxes.
//!
//! The Kraken panel is round, so the preview is round. It is drawn from the
//! same editor state the device output will use, which is why rotation and
//! colours here are the ones a later transport story sends to the panel.

use gpui::{Bounds, Div, Hsla, PathBuilder, Pixels, Point, Window, canvas, div, prelude::*, px};

use crate::shell::LcdEditor;
use crate::theme::{Color, color, numeric_font, space};

/// Diameter of the preview, in logical pixels.
pub const PREVIEW_DIAMETER: Pixels = px(220.0);

/// The two arcs of the dual infographic, as fractions of a full turn.
///
/// Both are `None` until telemetry exists, which is what keeps the preview
/// from showing a temperature the product cannot measure yet.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PreviewArcs {
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
}

/// Build the preview element for `editor`.
pub fn circular_preview(editor: LcdEditor) -> Div {
    let background = editor.background.hsla();
    let reading = editor.reading.hsla();
    let text = editor.text.hsla();
    let logo = editor.logo.hsla();
    let rotation = editor.rotation;
    let arcs = PreviewArcs::default();
    let warning = readability_warning(&editor);

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(space::SM)
        .child(
            div()
                .relative()
                .w(PREVIEW_DIAMETER)
                .h(PREVIEW_DIAMETER)
                .rounded(PREVIEW_DIAMETER / 2.0)
                .bg(background)
                .border_1()
                .border_color(color::SEPARATOR.hsla())
                .child(
                    canvas(
                        move |_, _, _| {},
                        move |bounds: Bounds<Pixels>, _, window: &mut Window, _| {
                            paint_preview(window, bounds, rotation, reading, logo, &arcs);
                        },
                    )
                    .size_full(),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(space::XS)
                        .text_color(text)
                        .child(div().font(numeric_font()).text_lg().child("-- C"))
                        .child(div().text_xs().child("CPU"))
                        .child(div().font(numeric_font()).text_lg().child("-- C"))
                        .child(div().text_xs().child("GPU")),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(color::TEXT_MUTED.hsla())
                .child("Preview only. No frame is sent until the LCD transport is validated."),
        )
        .children(warning.map(|warning| {
            div()
                .text_xs()
                .text_color(color::WARNING.hsla())
                .child(warning)
        }))
}

/// Warn when the chosen text and background cannot be told apart.
///
/// The panel is small and read at a distance, so a pair below the AA threshold
/// is unreadable in practice, not merely low contrast.
pub fn readability_warning(editor: &LcdEditor) -> Option<String> {
    let ratio = preview_contrast(editor);
    (ratio < MIN_READABLE_CONTRAST).then(|| {
        format!(
            "Text and background contrast is {ratio:.1}:1. Choose a lighter or darker pair to stay readable on the panel."
        )
    })
}

/// Lowest text-to-background contrast the preview accepts without warning.
pub const MIN_READABLE_CONTRAST: f32 = 4.5;

fn paint_preview(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    rotation: u16,
    reading: Hsla,
    logo: Hsla,
    arcs: &PreviewArcs,
) {
    let centre = bounds.center();
    let radius = bounds.size.width.min(bounds.size.height) / 2.0;

    // Outer track, always drawn: it is the panel edge, not a value.
    paint_arc(
        window,
        centre,
        radius - px(10.0),
        0.0,
        1.0,
        color::SEPARATOR.hsla(),
        px(6.0),
    );

    let start = rotation as f32 / 360.0;
    if let Some(cpu) = arcs.cpu {
        paint_arc(
            window,
            centre,
            radius - px(10.0),
            start,
            cpu,
            reading,
            px(6.0),
        );
    }
    if let Some(gpu) = arcs.gpu {
        paint_arc(
            window,
            centre,
            radius - px(24.0),
            start,
            gpu,
            reading,
            px(6.0),
        );
    }

    // The project's own mark: a short tick at the rotation origin. It is not a
    // vendor logo and never will be.
    let origin_angle = -std::f32::consts::FRAC_PI_2 + start * std::f32::consts::TAU;
    let mut mark = PathBuilder::stroke(px(3.0));
    mark.move_to(Point {
        x: centre.x + (radius - px(34.0)) * origin_angle.cos(),
        y: centre.y + (radius - px(34.0)) * origin_angle.sin(),
    });
    mark.line_to(Point {
        x: centre.x + (radius - px(20.0)) * origin_angle.cos(),
        y: centre.y + (radius - px(20.0)) * origin_angle.sin(),
    });
    if let Ok(path) = mark.build() {
        window.paint_path(path, logo);
    }
}

fn paint_arc(
    window: &mut Window,
    centre: Point<Pixels>,
    radius: Pixels,
    start: f32,
    sweep: f32,
    color: Hsla,
    thickness: Pixels,
) {
    let sweep = sweep.clamp(0.0, 1.0);
    if sweep <= 0.0 || radius <= px(0.0) {
        return;
    }

    let segments = ((sweep * 96.0).ceil() as usize).max(2);
    let mut builder = PathBuilder::stroke(thickness);
    for step in 0..=segments {
        let progress = start + (step as f32 / segments as f32) * sweep;
        let angle = -std::f32::consts::FRAC_PI_2 + progress * std::f32::consts::TAU;
        let point = Point {
            x: centre.x + radius * angle.cos(),
            y: centre.y + radius * angle.sin(),
        };
        if step == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Colours the preview draws with, for a given editor state.
///
/// Exposed so the background and the text on top of it can be checked as a
/// pair: a preview whose text vanishes into its background is a bug, not a
/// styling preference.
pub fn preview_contrast(editor: &LcdEditor) -> f32 {
    Color::rgb(editor.text.0).contrast(Color::rgb(editor.background.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::LcdColorField;

    #[test]
    fn the_default_preview_keeps_its_text_readable() {
        let editor = LcdEditor::default();
        assert!(
            preview_contrast(&editor) >= MIN_READABLE_CONTRAST,
            "default preview contrast is {:.2}:1",
            preview_contrast(&editor)
        );
        assert_eq!(readability_warning(&editor), None);
    }

    #[test]
    fn a_low_contrast_choice_warns_with_the_measured_ratio() {
        let mut editor = LcdEditor::default();
        editor.set_color(LcdColorField::Text, editor.background);
        assert!((preview_contrast(&editor) - 1.0).abs() < 0.001);

        let warning = readability_warning(&editor).unwrap();
        assert!(warning.contains("1.0:1"), "{warning}");
        assert!(warning.contains("readable"), "{warning}");
    }

    #[test]
    fn arcs_are_unavailable_until_telemetry_exists() {
        let arcs = PreviewArcs::default();
        assert_eq!(arcs.cpu, None);
        assert_eq!(arcs.gpu, None);
    }
}
