// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where every screen control sits in keyboard traversal order.
//!
//! Each row of a screen reserves a whole block of stops rather than sharing one
//! detail range: an open row's controls sit above the next row, so a shared
//! range would make Tab jump past a row and come back to it. The offsets inside
//! a block are named rather than counted at the call site, and they are
//! reserved whether or not the control renders, so a mode that hides two of
//! them cannot renumber the rest.
//!
//! The tests below are what makes the arithmetic safe to read: they collect the
//! stops as each screen emits them and assert the sequence is strictly
//! increasing, rather than restating the numbers a second time.

use kori_core::profile::Channel;

use crate::display::DisplayColorField;

/// First tab index available to a screen's own controls.
pub const SCREEN_TAB_BASE: isize = 10;
/// Tab stops of the Cooling screen, in traversal order.
///
/// Each channel row reserves a whole block: the row header itself, then every
/// control its open detail can render. Reserving the block per row rather than
/// sharing one detail range is what keeps keyboard traversal in visual order
/// whichever row is open, since an open row's controls sit above the next row.
pub const COOLING_TAB_MODE: isize = SCREEN_TAB_BASE;
pub const COOLING_TAB_PROFILE: isize = COOLING_TAB_MODE + 1;
pub const COOLING_TAB_ROW_BASE: isize = COOLING_TAB_PROFILE + 1;
/// Stops one row occupies: its header, the duty slider and the plot.
///
/// Wider than the controls an open row renders, and deliberately: the stops are
/// reserved per row whichever mode it is in, so a mode that hides the duty
/// slider cannot renumber the row below it.
pub const COOLING_ROW_STRIDE: isize = 4;
/// Offsets inside a channel's block, named rather than counted at the call site.
pub const COOLING_OFFSET_DUTY: isize = 1;
pub const COOLING_OFFSET_CURVE: isize = 3;
/// There is no Apply stop: an edit reaches the hardware on its own.
pub const COOLING_TAB_REVERT: isize = COOLING_TAB_ROW_BASE + 2 * COOLING_ROW_STRIDE;
pub const COOLING_TAB_SAVE: isize = COOLING_TAB_REVERT + 1;
pub const COOLING_TAB_DELETE: isize = COOLING_TAB_SAVE + 1;
/// First tab stop of one channel row's block.
pub fn cooling_row_tab(channel: Channel) -> isize {
    let index = match channel {
        Channel::Pump => 0,
        Channel::Fan => 1,
    };
    COOLING_TAB_ROW_BASE + index * COOLING_ROW_STRIDE
}
/// Tab stops of the Lighting screen, in traversal order.
///
/// The screen is a list of device rows, so the stops are allocated per row the
/// way the Cooling screen allocates them per channel: one block each, wide
/// enough for every control an open row can render. Reserving the block rather
/// than sharing one detail range is what keeps traversal in visual order
/// whichever row is open, since an open row's controls sit above the next row.
///
/// The offsets inside a block are named rather than counted at the call site,
/// so a control that appears or disappears with the mode cannot renumber the
/// ones after it.
pub const LIGHTING_TAB_ROW_BASE: isize = SCREEN_TAB_BASE;
/// Stops one lighting channel occupies, open or closed.
pub const LIGHTING_ROW_STRIDE: isize = 10;
/// Offsets inside a channel's block.
///
/// The block ends at the effect parameters. There is no Apply stop and no Turn
/// off stop: an edit reaches the controller on its own, and Off is one of the
/// entries in the mode select rather than a button repeating it.
pub const ROW_OFFSET_BRIGHTNESS: isize = 1;
pub const ROW_OFFSET_MODE: isize = 2;
pub const CHANNEL_OFFSET_COLOR: isize = 3;
pub const CHANNEL_OFFSET_SPEED: isize = 4;
pub const CHANNEL_OFFSET_DIRECTION: isize = 5;
/// Offsets inside the panel's block, which is the last one on the screen.
///
/// It is wider than [`LIGHTING_ROW_STRIDE`] and may be: nothing is allocated
/// after it. The color fields keep a fixed stop per field rather than per
/// rendered control, so a mode that hides two of them does not renumber the
/// rest.
pub const LCD_OFFSET_METRIC_ONE: isize = 3;
pub const LCD_OFFSET_METRIC_TWO: isize = 4;
pub const LCD_OFFSET_COLOR_BASE: isize = 5;
/// The file picker, which only image mode renders.
pub const LCD_OFFSET_IMAGE: isize = LCD_OFFSET_COLOR_BASE + DisplayColorField::ALL.len() as isize;
/// The one action the panel row still carries, and only while it is faulted.
///
/// Not an Apply: an edit is already on its way. A stopped stream is the one
/// state no edit can clear, because nothing about the preset changed when the
/// transfer failed, so it takes a deliberate activation. The stop is reserved
/// whether or not the control renders, for the same reason every other offset
/// here is.
pub const LCD_OFFSET_RESUME: isize = LCD_OFFSET_IMAGE + 1;
/// First tab stop of the channel row at `index` in the rendered list.
pub fn lighting_row_tab(index: usize) -> isize {
    LIGHTING_TAB_ROW_BASE + index as isize * LIGHTING_ROW_STRIDE
}
/// First tab stop of the panel row, which follows every channel row.
///
/// Derived from how many channels the controller reported rather than from a
/// fixed ceiling: a controller that answers with more channels than expected
/// pushes the panel's block down instead of colliding with it.
pub fn lcd_row_tab(channel_count: usize) -> isize {
    lighting_row_tab(channel_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Destination;

    /// Every stop one channel row emits, in the order it draws them.
    ///
    /// Collected as the screen emits them rather than written as a range: a
    /// stop counted by hand lands on a control that already exists the first
    /// time one of them renders more than a single element.
    fn channel_row_stops(base: isize) -> Vec<isize> {
        vec![
            base,
            base + ROW_OFFSET_BRIGHTNESS,
            base + ROW_OFFSET_MODE,
            base + CHANNEL_OFFSET_COLOR,
            base + CHANNEL_OFFSET_SPEED,
            base + CHANNEL_OFFSET_DIRECTION,
        ]
    }

    /// Every stop the panel row emits, in the order it draws them.
    fn lcd_row_stops(base: isize) -> Vec<isize> {
        let mut stops = vec![
            base,
            base + ROW_OFFSET_BRIGHTNESS,
            base + ROW_OFFSET_MODE,
            base + LCD_OFFSET_METRIC_ONE,
            base + LCD_OFFSET_METRIC_TWO,
        ];
        stops.extend(
            (0..DisplayColorField::ALL.len() as isize)
                .map(|index| base + LCD_OFFSET_COLOR_BASE + index),
        );
        stops.push(base + LCD_OFFSET_IMAGE);
        stops.push(base + LCD_OFFSET_RESUME);
        stops
    }

    #[test]
    fn every_lighting_control_keeps_traversal_order_equal_to_visual_order() {
        // However many channels the controller reports, each row's block has to
        // clear the row above it and the panel's block has to clear them all:
        // an open row's controls sit above the next line, so a shared range
        // would make Tab jump past a row and come back to it.
        for channel_count in 0..4usize {
            let mut stops = Vec::new();
            for index in 0..channel_count {
                stops.extend(channel_row_stops(lighting_row_tab(index)));
            }
            stops.extend(lcd_row_stops(lcd_row_tab(channel_count)));

            let sorted = {
                let mut copy = stops.clone();
                copy.sort();
                copy.dedup();
                copy
            };
            assert_eq!(
                stops, sorted,
                "with {channel_count} channels two controls share a stop, or run out of order"
            );
            assert!(
                stops
                    .iter()
                    .all(|stop| *stop >= SCREEN_TAB_BASE
                        && *stop > Destination::Settings.tab_index()),
                "screen controls come after every rail entry"
            );
        }
    }

    #[test]
    fn a_channel_block_never_reaches_the_row_below_it() {
        let widest = channel_row_stops(lighting_row_tab(0))
            .into_iter()
            .max()
            .expect("a channel row emits stops");
        assert!(
            widest < lighting_row_tab(1),
            "the widest channel detail must stay inside its own block"
        );
    }

    #[test]
    fn every_cooling_control_keeps_traversal_order_equal_to_visual_order() {
        // The stops the screen emits, in the order they are drawn, with the
        // Pump row open. Its detail sits between the two rows, so a shared
        // detail range would make Tab jump past the Fan row and back.
        let pump = cooling_row_tab(Channel::Pump);
        let fan = cooling_row_tab(Channel::Fan);
        let indices = [
            COOLING_TAB_MODE,
            COOLING_TAB_PROFILE,
            pump,
            // The widest detail, which is the Fixed mode: the duty slider, then
            // the curve plot. Both carry their own editing through the arrow
            // keys, so each is one stop rather than a row of buttons, and the
            // stops stay reserved whichever mode the row is in so a mode change
            // never renumbers the row below it.
            pump + COOLING_OFFSET_DUTY,
            pump + COOLING_OFFSET_CURVE,
            fan,
            COOLING_TAB_REVERT,
            COOLING_TAB_SAVE,
            COOLING_TAB_DELETE,
        ];

        let mut sorted = indices.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            indices.to_vec(),
            sorted,
            "no two cooling controls share a stop, and they run in visual order"
        );
        assert!(
            indices.iter().all(
                |index| *index >= SCREEN_TAB_BASE && *index > Destination::Settings.tab_index()
            ),
            "screen controls come after every rail entry"
        );
        assert!(
            fan + COOLING_ROW_STRIDE - 1 < COOLING_TAB_REVERT,
            "the last row's detail must not reach the action buttons"
        );
    }

    #[test]
    fn rail_tab_order_is_stable_and_precedes_screen_controls() {
        let mut indices: Vec<isize> = Destination::PRIMARY
            .into_iter()
            .chain([Destination::Settings])
            .map(Destination::tab_index)
            .collect();
        let sorted = {
            let mut copy = indices.clone();
            copy.sort();
            copy
        };
        assert_eq!(indices, sorted, "rail order must match visual order");

        indices.dedup();
        assert_eq!(indices.len(), 4, "every entry needs its own tab stop");
        assert!(indices.iter().all(|index| *index < SCREEN_TAB_BASE));
    }

    #[test]
    fn the_picker_takes_a_stop_of_its_own_ahead_of_the_recovery_button() {
        // The offsets are fixed per control rather than per rendered control,
        // so a mode that draws no picker does not renumber what follows it.
        assert_eq!(
            LCD_OFFSET_IMAGE,
            LCD_OFFSET_COLOR_BASE + DisplayColorField::ALL.len() as isize
        );
        assert_eq!(LCD_OFFSET_RESUME, LCD_OFFSET_IMAGE + 1);
    }

    #[test]
    fn the_panel_row_follows_whatever_the_controller_reported() {
        // The panel's block is derived from the channel count rather than from
        // a fixed ceiling, so a controller answering with more channels than
        // expected pushes the panel down instead of colliding with it.
        for channel_count in 0..8usize {
            let base = lcd_row_tab(channel_count);
            let last_channel_stop = channel_count
                .checked_sub(1)
                .and_then(|index| channel_row_stops(lighting_row_tab(index)).into_iter().max());
            if let Some(last) = last_channel_stop {
                assert!(last < base, "the panel row overlaps the last channel row");
            }
            assert!(lcd_row_stops(base).iter().all(|stop| *stop >= base));
        }
    }
}
