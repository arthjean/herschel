// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The Lighting screen: one card per device, one row per addressable thing.
//!
//! The panel used to be a destination of its own, which put the two things an
//! operator changes about the same machine's appearance two clicks apart. They
//! are one screen: the controller card lists its channels, the Kraken card
//! carries its panel, and each row opens the controls that belong to it alone.

use gpui::{Div, div, prelude::*, px};

use kori_core::capability::CapabilityId;
use kori_core::ipc::ChannelState;
use kori_core::lighting::{EffectDirection, EffectSpeed, LightingCommand};
use kori_core::{DeviceId, KRAKEN_BASE, RGB_CONTROLLER};

use crate::assets::Icon;
use crate::components::{ControlState, Note, NoteLevel, SelectOption, row_panel};
use crate::feed::{Command, CommandSubject};
use crate::lighting::LightingMode;
use crate::shell::Shell;
use crate::theme::{color, space};
use gpui::Context;

use super::row::ROW_DETAIL_INDENT;
use super::row::{LightingRow, RowLine, RowNote, row_thumbnail};
use super::swatch::ColorPicker;
use super::tab::lcd_row_tab;
use super::tab::{
    CHANNEL_OFFSET_COLOR, CHANNEL_OFFSET_DIRECTION, CHANNEL_OFFSET_SPEED, ROW_OFFSET_MODE,
    lighting_row_tab,
};
use super::write::WriteTarget;
use super::{Caption, FIELD_WIDTH, screen};

/// What the controller's card is headed with.
///
/// What the device does rather than the product string it reports: that string
/// is a vendor wordmark, and this heading says which of the two devices the
/// card is for, which is what the operator needs from it. The reported string
/// is still shown on the monitoring screen's device strip.
pub const RGB_CONTROLLER_NAME: &str = "RGB & Fan Controller";
/// What a channel row leads with: what the controller says is plugged in.
///
/// The channel number is not repeated here. It sits on the line below, where a
/// row that has nothing plugged in carries it as the heading instead.
pub fn channel_headline(state: &ChannelState) -> String {
    match state.accessories.len() {
        0 => format!("Channel {}", state.channel),
        1 => state.accessories[0].clone(),
        count => format!("{count} accessories"),
    }
}

impl Shell {
    /// The Lighting screen: one card per device, one row per addressable thing.
    ///
    /// The panel used to be a destination of its own, which put the two things
    /// an operator changes about the same machine's appearance two clicks
    /// apart. They are one screen now: the controller card lists its channels,
    /// the Kraken card carries its panel, and each row opens the controls that
    /// belong to it alone.
    pub(crate) fn lighting(&self, cx: &mut Context<Self>) -> Div {
        let channels: Vec<u8> = self
            .link
            .lighting_channels()
            .iter()
            .map(|state| state.channel)
            .collect();

        screen(
            "Lighting",
            "What each device shows: the controller's channels, and the panel on the Kraken.",
        )
        .child(self.controller_card(&channels, cx))
        .child(self.panel_card(channels.len(), cx))
        .children(self.outcome_note(CommandSubject::is_appearance))
    }

    /// The controller and one row per channel it reported.
    fn controller_card(&self, channels: &[u8], cx: &mut Context<Self>) -> Div {
        let fixed = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor);
        let card = Self::device_card(RGB_CONTROLLER_NAME);

        // No channel means the controller has not told this daemon what it is.
        // The reason the capability record carries is the whole content of the
        // card: there is nothing to control and nothing to pretend about.
        if channels.is_empty() {
            return card.child(
                Note::new(
                    NoteLevel::Warning,
                    fixed.message().cloned().unwrap_or_else(|| {
                        "No lighting controller answered. Lighting is read-only until it does."
                            .into()
                    }),
                )
                .render(),
            );
        }

        // A `map` closure would have to hold the mutable context borrow across
        // calls, which the 2024 edition's capture rules reject.
        let mut rows = Vec::with_capacity(channels.len());
        for (index, channel) in channels.iter().enumerate() {
            rows.push(self.channel_row_lighting(*channel, index, cx));
        }
        card.children(rows)
    }

    /// The Kraken and the row its panel occupies.
    ///
    /// Headed with what the cooler reported, unlike the controller: there is
    /// one panel and one pump, so the exact model is what tells the operator
    /// which cooler answered.
    fn panel_card(&self, channel_count: usize, cx: &mut Context<Self>) -> Div {
        let name = self
            .reported_name(KRAKEN_BASE)
            .unwrap_or_else(|| "Kraken".to_string());
        Self::device_card(&name).child(self.lcd_row(lcd_row_tab(channel_count), cx))
    }

    /// The product string a device reported, if it answered at all.
    fn reported_name(&self, device: DeviceId) -> Option<String> {
        self.link
            .device_rows()
            .into_iter()
            .find(|summary| summary.id == device)
            .map(|summary| summary.name)
    }

    /// The card one device gets: what it is, and its rows.
    ///
    /// `name` is what the card is headed with. Where it is a fixed string, the
    /// product string the device reported is deliberately not shown: that
    /// string carries a vendor wordmark this product does not use, and what the
    /// operator needs from the heading is which of the two devices this is. The
    /// reported string is still on the monitoring screen's device strip,
    /// unchanged, which is where identifying the exact hardware belongs.
    ///
    /// The header carries the name alone. Firmware, kernel binding and state
    /// are all on that strip, which is the screen for identifying hardware;
    /// repeating them over every card put a line of provenance above controls
    /// that are about appearance.
    fn device_card(name: &str) -> Div {
        let name = name.to_string();

        row_panel().child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .w_full()
                .min_w_0()
                .child(
                    // Claims the line rather than being sized by its own text.
                    // Left to measure itself, the block collapsed to its
                    // narrowest column and set the device name one letter per
                    // line down the side of the card.
                    div()
                        .flex()
                        .flex_1()
                        .flex_col()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_color(color::TEXT.hsla())
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(name),
                        ),
                ),
        )
    }

    /// One lighting channel as a single line, with its controls one press away.
    ///
    /// Collapsed, the line still carries the two controls an operator reaches
    /// for most: how bright the channel is and what it is doing. Opening it
    /// reveals the color and the effect parameters for that channel alone.
    fn channel_row_lighting(&self, channel: u8, index: usize, cx: &mut Context<Self>) -> Div {
        let base = lighting_row_tab(index);
        let Some(editor) = self.lighting.channel(channel) else {
            return div();
        };

        let effects = self
            .link
            .control_state(RGB_CONTROLLER, CapabilityId::RgbEffects);
        // Every control on this row now writes by itself, so every one of them
        // carries this state. It is the capability the pending mode needs, not
        // a single "lighting is writable": an effect refused on a controller
        // that accepts a fixed color has to say so on the controls that would
        // have sent the effect.
        let write = if matches!(editor.mode, LightingMode::Effect(_)) {
            effects.clone()
        } else {
            self.link
                .control_state(RGB_CONTROLLER, CapabilityId::RgbFixedColor)
        };

        // One line, not two. What is plugged in leads, because that is what the
        // operator is looking at inside the case, and the channel follows it as
        // a qualifier because that is what the write is addressed to.
        //
        // The accessory name is what the controller answered for the identifier
        // byte it returned, never a label this product chose. Nothing in that
        // answer says what kind of thing is on the channel, so nothing here
        // calls it one: a strip named "Fan 2" would be a fabricated fact sitting
        // next to a control that writes to real hardware.
        //
        // What the channel last had sent to it moved into the open detail with
        // the accessory summary. Both are sentences, and a line that also
        // carries a slider and a select has room for neither.
        let detected = self
            .link
            .lighting_channels()
            .iter()
            .find(|state| state.channel == channel)
            .filter(|state| !state.accessories.is_empty());
        let (title, qualifier) = match detected {
            Some(state) => (channel_headline(state), Some(format!("Channel {channel}"))),
            // With nothing detected the headline is already the channel, so the
            // qualifier would repeat it word for word.
            None => (format!("Channel {channel}"), None),
        };
        // The controller answers accessory identifiers, not what kind of thing
        // carries them, so the fan is drawn only where it answered something at
        // all. A channel that answered nothing gets an empty outline: the row
        // exists and can be written to, and nothing on it claims a fan is
        // plugged into it. Same rule as the headline right above, which stays
        // "Channel N" until the controller names something.
        let glyph = match detected {
            Some(_) => Icon::Windmill,
            None => Icon::CircleDashed,
        };

        // The thumbnail is what the channel is pending, so a color chosen and
        // not yet applied is visible on the collapsed line rather than only in
        // the open one. An unusable entry paints nothing instead of a value it
        // never held.
        let swatch = editor
            .mode
            .uses_color()
            .then(|| crate::components::parse_hex_color(&editor.color).ok())
            .flatten();
        let brightness = editor.brightness;
        let mode_value = editor.mode.value();

        let row = LightingRow::Channel(channel);
        self.device_row(
            row,
            RowLine {
                thumbnail: row_thumbnail(swatch, glyph, false),
                title,
                note: qualifier.map(RowNote::Fragment),
                brightness,
                write: write.clone(),
                mode: self.select(
                    format!("lighting-mode-{channel}"),
                    "Mode",
                    Caption::Hidden,
                    LightingMode::all(effects.is_enabled())
                        .into_iter()
                        .map(|mode| SelectOption::new(mode.value(), mode.label()))
                        .collect(),
                    mode_value,
                    write.clone(),
                    base + ROW_OFFSET_MODE,
                    cx,
                    move |shell, value, cx| {
                        let Some(mode) = LightingMode::from_value(value) else {
                            return;
                        };
                        let Some(editor) = shell.lighting.channel_mut(channel) else {
                            return;
                        };
                        editor.mode = mode;
                        shell.schedule_write(WriteTarget::Lighting(row), cx);
                    },
                ),
                tab_index: base,
            },
            self.is_open(row)
                .then(|| self.channel_detail_lighting(channel, base, write, cx)),
            cx,
        )
    }

    /// What an open channel row reveals: its color and its effect parameters.
    ///
    /// No action. Every control here writes on its own once the row goes quiet,
    /// so a button repeating what the last edit already did would be a second
    /// way to send the same command.
    fn channel_detail_lighting(
        &self,
        channel: u8,
        base: isize,
        write: ControlState,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(editor) = self.lighting.channel(channel) else {
            return div();
        };
        // A pending program that cannot be built is the one refusal no control
        // on this row carries: each field names what is wrong with its own
        // value, and this names what is wrong with the combination. It is a
        // note rather than a disabled button, because there is no longer a
        // button to disable and nothing is sent while it stands.
        let program = editor.program();

        // What the controller was last told to show. The channel answers no
        // report that reads its program back, so this record is the only
        // evidence there is, and saying "sent" rather than "showing" is what
        // keeps it honest.
        //
        // It is also the standing confirmation the Apply button used to give by
        // changing its own label, and it is how the row shows the current
        // confirmed mode. The panel row carries the same fact on
        // its line; a channel line already holds a slider and a select, so this
        // one lives in the detail.
        let sent = self
            .link
            .lighting_channels()
            .iter()
            .find(|state| state.channel == channel)
            .and_then(|state| state.committed.as_ref())
            .map(|program| format!("Last sent: {}", program.summary()))
            .unwrap_or_else(|| "Nothing sent to this channel yet".to_string());

        div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(space::MD)
            .pb(space::MD)
            .pl(ROW_DETAIL_INDENT)
            .child(
                div()
                    .text_xs()
                    .text_color(color::TEXT_MUTED.hsla())
                    .child(sent),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(space::MD)
                    .w_full()
                    .min_w_0()
                    .when(editor.mode.uses_color(), |this| {
                        this.child(div().flex_none().w(FIELD_WIDTH).child(self.color_picker(
                            ColorPicker::Channel(channel),
                            write.clone(),
                            base + CHANNEL_OFFSET_COLOR,
                            cx,
                        )))
                    })
                    .when(editor.mode.uses_speed(), |this| {
                        this.child(
                            div().flex_none().w(FIELD_WIDTH).child(
                                self.select(
                                    format!("lighting-speed-{channel}"),
                                    "Speed",
                                    Caption::Shown,
                                    EffectSpeed::ALL
                                        .into_iter()
                                        .map(|speed| SelectOption::new(speed.key(), speed.label()))
                                        .collect(),
                                    editor.speed.key().to_string(),
                                    write.clone(),
                                    base + CHANNEL_OFFSET_SPEED,
                                    cx,
                                    move |shell, value, cx| {
                                        let Some(speed) = EffectSpeed::from_key(value) else {
                                            return;
                                        };
                                        let Some(editor) = shell.lighting.channel_mut(channel)
                                        else {
                                            return;
                                        };
                                        editor.speed = speed;
                                        shell.schedule_write(
                                            WriteTarget::Lighting(LightingRow::Channel(channel)),
                                            cx,
                                        );
                                    },
                                ),
                            ),
                        )
                    })
                    .when(editor.mode.uses_direction(), |this| {
                        this.child(
                            div().flex_none().w(FIELD_WIDTH).child(
                                self.select(
                                    format!("lighting-direction-{channel}"),
                                    "Direction",
                                    Caption::Shown,
                                    EffectDirection::ALL
                                        .into_iter()
                                        .map(|direction| {
                                            SelectOption::new(direction.key(), direction.label())
                                        })
                                        .collect(),
                                    editor.direction.key().to_string(),
                                    write.clone(),
                                    base + CHANNEL_OFFSET_DIRECTION,
                                    cx,
                                    move |shell, value, cx| {
                                        let Some(direction) = EffectDirection::from_key(value)
                                        else {
                                            return;
                                        };
                                        let Some(editor) = shell.lighting.channel_mut(channel)
                                        else {
                                            return;
                                        };
                                        editor.direction = direction;
                                        shell.schedule_write(
                                            WriteTarget::Lighting(LightingRow::Channel(channel)),
                                            cx,
                                        );
                                    },
                                ),
                            ),
                        )
                    }),
            )
            .children(
                program
                    .err()
                    .map(|error| Note::new(NoteLevel::Warning, error.to_string()).render()),
            )
    }

    /// Send one channel's pending program, unless it is already showing it.
    ///
    /// Nothing is sent when the program cannot be built. The control holding
    /// the unusable value already names the problem, and a command the daemon
    /// would refuse would answer it with a sentence about the channel instead.
    ///
    /// The comparison against what the daemon committed does not replace the
    /// daemon's own deduplication, which still runs and is still the authority.
    /// It is what keeps an edit that lands back on the current value from
    /// costing a request, a reply and a polling cycle.
    pub(crate) fn send_lighting(&mut self, channel: u8) {
        let Some(program) = self
            .lighting
            .channel(channel)
            .and_then(|editor| editor.program().ok())
        else {
            return;
        };
        let committed = self
            .link
            .lighting_channels()
            .iter()
            .find(|state| state.channel == channel)
            .and_then(|state| state.committed.as_ref());
        if committed == Some(&program) {
            return;
        }
        self.feed
            .send(Command::ApplyLighting(LightingCommand { channel, program }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_state(channel: u8, accessories: &[&str]) -> ChannelState {
        ChannelState {
            channel,
            accessories: accessories.iter().map(|name| name.to_string()).collect(),
            committed: None,
        }
    }

    /// The row is named by what the controller answered, never by a type this
    /// product guessed. The controller returns an accessory identifier and a
    /// name for it, and nothing in that answer says whether the thing is a fan,
    /// a strip or anything else, so the headline never claims one.
    #[test]
    fn a_channel_is_named_by_what_the_controller_answered() {
        let one = channel_state(1, &["HUE 2 LED Strip 300 mm"]);
        assert_eq!(channel_headline(&one), "HUE 2 LED Strip 300 mm");

        let many = channel_state(2, &["AER RGB 2 120 mm", "AER RGB 2 140 mm"]);
        assert_eq!(channel_headline(&many), "2 accessories");

        // Nothing plugged in leaves the channel number as the heading, so a row
        // is never nameless. It is also why the row adds no "Channel 2"
        // qualifier in this case: it would repeat the headline word for word.
        let empty = channel_state(3, &[]);
        assert_eq!(channel_headline(&empty), "Channel 3");
    }
}
