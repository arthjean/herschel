// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The panel row of the Lighting screen, and everything its editor needs.
//!
//! The glass reads nothing back, so the daemon's record of the last preset it
//! committed is the only evidence of what is on it. The preview is not drawn
//! with the toolkit's shapes: it calls the same renderer the daemon calls, so
//! what is on screen is what is on the glass, or the two differ only because
//! the preset does.

use gpui::{Div, PathPromptOptions, div, prelude::*};

use gpui::{Context, Pixels, px};
use kori_core::KRAKEN_BASE;
use kori_core::capability::CapabilityId;
use kori_core::display::{DisplayMode, LcdMetric, MetricSample};

use crate::assets::Icon;
use crate::components::{Button, ButtonVariant, ControlState, Note, NoteLevel, SelectOption};
use crate::display::{DisplayColorField, DisplayEditor};
use crate::feed::Command;
use crate::shell::Shell;
use crate::theme::{color, space};

use super::row::ROW_DETAIL_INDENT;
use super::row::{LightingRow, RowLine, RowNote, row_thumbnail};
use super::swatch::ColorPicker;
use super::tab::{
    LCD_OFFSET_COLOR_BASE, LCD_OFFSET_IMAGE, LCD_OFFSET_METRIC_ONE, LCD_OFFSET_RESUME,
    ROW_OFFSET_MODE,
};
use super::write::WriteTarget;
use super::{Caption, SCREEN_MODES, field_line};

/// Width of the preview column, wide enough for the panel plus its padding.
pub const PREVIEW_COLUMN_WIDTH: Pixels = px(276.0);

impl Shell {
    /// The panel as a single line, with its editor one press away.
    pub(crate) fn lcd_row(&self, base: isize, cx: &mut Context<Self>) -> Div {
        let frame = self.link.control_state(KRAKEN_BASE, CapabilityId::LcdFrame);
        let editor = self.lcd.editor().clone();
        let panel = self
            .link
            .status()
            .and_then(|status| status.display.panel.clone());

        let subtitle = match self
            .link
            .status()
            .and_then(|status| status.display.committed.clone())
        {
            Some(preset) => format!(
                "{} at {}, {}%",
                preset.mode.label(),
                preset.orientation.label(),
                preset.brightness.percent()
            ),
            None => match &panel {
                Some(panel) => {
                    format!("{}x{} panel, nothing sent yet", panel.width, panel.height)
                }
                None => "no panel has answered".to_string(),
            },
        };

        let background = crate::components::parse_hex_color(
            self.lcd.editor().color_text(DisplayColorField::Background),
        )
        .ok();

        self.device_row(
            LightingRow::Lcd,
            RowLine {
                thumbnail: row_thumbnail(background, Icon::Photo, true),
                title: "LCD display".to_string(),
                // The panel keeps a second line where a channel does not: what
                // it is showing is a mode, an orientation and a brightness,
                // which is a sentence rather than a fragment, and there is one
                // panel row instead of a list of them to keep even.
                note: Some(RowNote::Sentence(subtitle)),
                brightness: editor.brightness,
                // Moving the slider is what sends the brightness now, so the
                // panel's own refusal belongs on it. It used to sit on the
                // Apply button alone, which left an operable control in front
                // of a panel nothing can be written to.
                write: frame.clone(),
                mode: self.select(
                    "lcd-mode",
                    "Display mode",
                    Caption::Hidden,
                    SCREEN_MODES
                        .into_iter()
                        .map(|mode| SelectOption::new(mode.key(), mode.label()))
                        .collect(),
                    editor.mode.key().to_string(),
                    frame.clone(),
                    base + ROW_OFFSET_MODE,
                    cx,
                    |shell, value, cx| {
                        let Some(mode) = DisplayMode::from_key(value) else {
                            return;
                        };
                        shell.lcd.edit(|editor| editor.mode = mode);
                        shell.schedule_write(WriteTarget::Lighting(LightingRow::Lcd), cx);
                    },
                ),
                tab_index: base,
            },
            self.is_open(LightingRow::Lcd)
                .then(|| self.lcd_detail(base, &editor, panel.as_ref(), frame, cx)),
            cx,
        )
    }

    /// What the open panel row reveals: the fields on the left, the frame the
    /// daemon would send on the right.
    fn lcd_detail(
        &self,
        base: isize,
        editor: &DisplayEditor,
        panel: Option<&kori_core::capability::LcdPanel>,
        frame: ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        // The preview ages with telemetry exactly as the panel does, from the
        // same samples the daemon renders against.
        let samples = match self.link.telemetry() {
            Some(snapshot) => self.lcd.preview().samples(snapshot),
            None => [
                MetricSample::unavailable(editor.metrics[0]),
                MetricSample::unavailable(editor.metrics[1]),
            ],
        };

        // A stopped stream is the one state on this screen an edit cannot
        // clear: the transfer failed without anything about the preset
        // changing, so nothing an automatic write watches has moved. It takes
        // a deliberate activation, which is the explicit recoverable state the
        // stream waits for.
        let faulted = self
            .link
            .status()
            .and_then(|status| status.display.faulted.clone())
            .filter(|_| frame.is_enabled());

        // Image mode plays its own compiled frames rather than re-rendering the
        // preset: the file was decoded when it was picked, and an animation has
        // a frame to show that no repaint could work out on its own.
        let film = self.lcd.film();
        let picture = film.and_then(|film| film.frame().map(<[u8]>::to_vec));

        // One grid, two columns wide: slot 1 on the left, slot 2 on the right,
        // one line per thing being chosen. Built here rather than inside the
        // element tree because a `map` closure would have to hold the mutable
        // context borrow across calls, which the 2024 edition's capture rules
        // reject.
        let mut lines: Vec<Div> = Vec::new();
        if editor.mode.uses_readings() {
            let mut controls = Vec::new();
            for slot in 0..editor.mode.reading_slots() {
                controls.push(self.metric_select(
                    slot,
                    frame.clone(),
                    base + LCD_OFFSET_METRIC_ONE + slot as isize,
                    cx,
                ));
            }
            lines.push(field_line(controls));
        }
        // The colors follow in the same grid, paired by role: the band of both
        // readings on one line, then what each fades to, then their text, then
        // the panel's own background. A line the current mode draws nothing for
        // is absent rather than empty.
        for (line, fields) in DisplayColorField::ALL.chunks(2).enumerate() {
            let mut controls = Vec::new();
            for (column, field) in fields.iter().enumerate() {
                if !field.is_used_by(editor.mode) {
                    continue;
                }
                let index = (line * 2 + column) as isize;
                controls.push(self.color_picker(
                    ColorPicker::Panel(*field),
                    frame.clone(),
                    base + LCD_OFFSET_COLOR_BASE + index,
                    cx,
                ));
            }
            if !controls.is_empty() {
                lines.push(field_line(controls));
            }
        }
        if editor.mode.uses_image() {
            lines.push(self.image_field(editor, film, frame.clone(), base + LCD_OFFSET_IMAGE, cx));
        }

        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(space::LG)
            .w_full()
            .min_w_0()
            .pb(space::MD)
            .pl(ROW_DETAIL_INDENT)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    // Every field is the same width and every line the same
                    // gap, so the labels line up down both columns and the
                    // screen reads as one form rather than as three panels.
                    .gap(space::MD)
                    .children(lines)
                    .children(
                        editor
                            .preset()
                            .err()
                            // A file that cannot be read is named the same way
                            // a color that cannot be parsed is. The preset
                            // error comes first: it is the one that stops the
                            // frame before the picture is even looked for.
                            .or_else(|| self.lcd.film_error().cloned())
                            .map(|error| Note::new(NoteLevel::Warning, error.to_string()).render()),
                    )
                    .children(faulted.map(|reason| {
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .pt(space::SM)
                            .gap(space::SM)
                            .child(
                                Button::new("lcd-resume", "Resume display")
                                    .variant(ButtonVariant::Primary)
                                    .tab_index(base + LCD_OFFSET_RESUME)
                                    .render()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        // Acting on the panel dismisses any
                                        // open popover, so a swatch list never
                                        // hides the result.
                                        this.popover = None;
                                        this.resume_display(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(color::WARNING.hsla())
                                    .child(format!("The panel stopped updating: {reason}")),
                            )
                    }))
                    .children(frame.message().map(|reason| {
                        div()
                            .text_xs()
                            .text_color(color::WARNING.hsla())
                            .child(reason.to_string())
                    })),
            )
            .child(
                // Fixed rather than sized to content, so the two field columns
                // beside it keep their width: at 920 logical pixels there is
                // room for both and the disc, and nothing has to be clipped to
                // decide which.
                div()
                    .flex_none()
                    .w(PREVIEW_COLUMN_WIDTH)
                    .child(if editor.mode.uses_image() {
                        crate::preview::panel_frame(picture, self.lcd.preview().background)
                    } else {
                        crate::preview::panel_preview(self.lcd.preview(), &samples, panel)
                    }),
            )
    }

    /// The file picker, and what it currently holds.
    ///
    /// The platform's own dialog rather than a field to type a path into. This
    /// codebase has no text-input primitive and does not need one here: a file
    /// name is not something an operator should have to spell, and the portal
    /// already filters by what this product can decode.
    fn image_field(
        &self,
        editor: &DisplayEditor,
        film: Option<&crate::display::ImageFilm>,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let chosen = std::path::Path::new(editor.image_path.trim())
            .file_name()
            .map(|name| name.to_string_lossy().to_string());

        // What the panel will actually do with the file, measured rather than
        // guessed from the extension: a GIF carrying one frame holds still, and
        // saying "animated" over it would be a fabricated capability.
        let note = match (chosen.as_deref(), film) {
            (None, _) => "No file chosen".to_string(),
            (Some(name), Some(film)) if film.is_animated() => format!(
                "{name}, {} frames over {:.1} s",
                film.frame_count(),
                film.duration().as_secs_f32()
            ),
            (Some(name), _) => format!("{name}, one still picture"),
        };

        // Not a `field_line`: the two color columns are a grid of equal boxes,
        // and a file name is a sentence that needs the width of both.
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space::MD)
            .w_full()
            .min_w_0()
            .child(
                Button::new("lcd-image", "Choose a picture")
                    .state(state)
                    .tab_index(tab_index)
                    .render()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.popover = None;
                        this.choose_image(cx);
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(color::TEXT_MUTED.hsla())
                    .truncate()
                    .child(note),
            )
    }

    /// The metric select for one reading slot.
    fn metric_select(
        &self,
        slot: usize,
        state: ControlState,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = self.lcd.editor().metrics[slot];
        // The metric line heads the grid and the colors under it are named by
        // role, so the slot alone is enough to say what this select chooses.
        let (id, label) = if slot == 0 {
            ("lcd-metric-1", "Reading 1")
        } else {
            ("lcd-metric-2", "Reading 2")
        };
        self.select(
            id,
            label,
            Caption::Shown,
            LcdMetric::ALL
                .into_iter()
                .map(|metric| SelectOption::new(metric.key(), metric.label()))
                .collect(),
            selected.key().to_string(),
            state,
            tab_index,
            cx,
            move |shell, value, cx| {
                let Some(metric) = LcdMetric::from_key(value) else {
                    return;
                };
                shell.lcd.edit(|editor| editor.metrics[slot] = metric);
                shell.schedule_write(WriteTarget::Lighting(LightingRow::Lcd), cx);
            },
        )
    }

    /// Send the panel's pending preset, unless it is already showing it.
    ///
    /// Deduplicating here matters more than it does for a channel: an unchanged
    /// preset still costs the daemon a full render before it can compare the
    /// picture it produced.
    pub(crate) fn send_display(&mut self) {
        let Ok(preset) = self.lcd.preset() else {
            return;
        };
        let committed = self
            .link
            .status()
            .and_then(|status| status.display.committed.as_ref());
        if committed == Some(&preset) {
            return;
        }
        self.feed.send(Command::ApplyDisplay(preset));
    }

    /// Ask the platform for a file, and put what comes back on the panel.
    ///
    /// The platform's own dialog rather than a path typed into a field. This
    /// codebase has no text-input primitive, and the reason it does not need
    /// one here is not that the picker is a workaround: a file name is not
    /// something an operator should have to spell.
    ///
    /// The dialog is asynchronous and this window keeps drawing while it is
    /// open, so a cancel is the same code path as never having opened it.
    fn choose_image(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Show on the panel".into()),
        });
        cx.spawn(async move |shell, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = shell.update(cx, |shell, cx| {
                shell
                    .lcd
                    .edit(|editor| editor.image_path = path.display().to_string());
                shell.schedule_write(WriteTarget::Lighting(LightingRow::Lcd), cx);
            });
        })
        .detach();
    }

    /// Compile the picture the preview needs, and keep it moving.
    ///
    /// Cheap when nothing about the file changed: the compiled frames carry
    /// what they depend on, so this is one comparison per edit and a decode
    /// only when the operator actually picked something else.
    pub(crate) fn refresh_film(&mut self, cx: &mut Context<Self>) {
        let panel = self
            .link
            .status()
            .and_then(|status| status.display.panel.as_ref());
        self.lcd.sync_film(panel);
        self.play_film(cx);
    }

    /// Arrange for the next frame of the animation, if one is playing.
    ///
    /// Every call takes a fresh generation and every timer carries the one it
    /// was spawned under, so a picture that has been replaced leaves timers
    /// that fire into nothing rather than a second clock advancing the new one
    /// at twice its cadence. Same shape as [`Shell::schedule_write`]: no
    /// task is cancelled, because cancelling the wrong one is the failure this
    /// avoids.
    pub(crate) fn play_film(&mut self, cx: &mut Context<Self>) {
        if !self.lighting_open.contains(&LightingRow::Lcd) {
            return;
        }
        let Some(delay) = self
            .lcd
            .film()
            .filter(|film| film.is_animated())
            .and_then(|film| film.delay())
        else {
            return;
        };

        self.film_generation = self.film_generation.wrapping_add(1);
        let generation = self.film_generation;
        cx.spawn(async move |shell, cx| {
            cx.background_executor().timer(delay).await;
            let _ = shell.update(cx, |shell, cx| {
                if shell.film_generation != generation {
                    return;
                }
                if shell.lcd.advance_film() {
                    shell.play_film(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Send the panel's preset whatever it is showing, to restart a stopped
    /// stream.
    ///
    /// The one write on this screen that is still a deliberate activation. A
    /// faulted stream is the state no edit can clear: the transfer failed
    /// without anything about the preset changing, so there is nothing for an
    /// automatic write to notice.
    fn resume_display(&mut self, cx: &mut Context<Self>) {
        if let Ok(preset) = self.lcd.preset() {
            self.feed.send(Command::ApplyDisplay(preset));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplayEditor;

    #[test]
    fn the_screen_offers_every_mode_and_a_control_for_each_of_them() {
        // A mode the daemon could only ever refuse is absent, not disabled,
        // which is the same rule the Lighting screen applies to an unproven
        // effect. Every mode the vocabulary carries is now offered, because
        // every one of them has the control it needs.
        assert_eq!(SCREEN_MODES.len(), DisplayMode::ALL.len());
        for mode in DisplayMode::ALL {
            assert!(SCREEN_MODES.contains(&mode), "{} is absent", mode.label());
        }

        // Every mode but one produces a preset from the defaults alone. Image
        // mode is the exception by construction: it names a file, and the
        // refusal it carries until one is picked is the whole reason the
        // picker sits in the same detail as the select.
        for mode in SCREEN_MODES {
            let mut editor = DisplayEditor::default();
            editor.mode = mode;
            assert_eq!(
                editor.preset().is_ok(),
                mode != DisplayMode::Image,
                "{} produced the wrong verdict from the defaults",
                mode.label()
            );
        }

        let mut editor = DisplayEditor::default();
        editor.mode = DisplayMode::Image;
        editor.image_path = "/home/a/loop.gif".to_string();
        assert!(
            editor.preset().is_ok(),
            "a picked file is all image mode is missing"
        );
    }

    #[test]
    fn the_editor_exposes_every_color_control_the_panel_can_paint() {
        // A color per reading, a text color per reading, and a background. The
        // band's second color is the one addition; a logo color is the one
        // omission, since the panel carries no wordmark for it to paint.
        let labels: Vec<_> = DisplayColorField::ALL
            .map(DisplayColorField::label)
            .to_vec();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            labels.len(),
            "labels must be distinct: {labels:?}"
        );
        for required in [
            "Band 1",
            "Band 2",
            "Fade 1",
            "Fade 2",
            "Text 1",
            "Text 2",
            "Background",
        ] {
            assert!(labels.contains(&required), "{required} is missing");
        }
        assert!(
            !labels.iter().any(|label| label.contains("Wordmark")),
            "nothing draws a wordmark, so nothing colors one: {labels:?}"
        );
    }
}
