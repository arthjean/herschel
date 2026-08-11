// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the pointer is moving, and where each control was painted.
//!
//! The window decides which control a press landed on, rather than the control
//! hearing about the press itself. A listener on the control was the first
//! arrangement and it never fired: the press has to travel down an element
//! whose interactive state, hover styling and focus ring all sit between it and
//! the handler. The window's own capture handler is the one place an event is
//! guaranteed to arrive, and the same handler already runs there for focus.

use std::cell::RefCell;

use gpui::{Bounds, Pixels, Point, px};

use kori_core::profile::{Channel, CurveNodes};

use super::row::LightingRow;
use crate::components::{Slider, node_at};
use crate::shell::Shell;
use gpui::Context;
use kori_core::profile::{MAX_DUTY_PERCENT, duty_from_percent, duty_to_percent};

/// What the pointer is currently moving, if anything.
///
/// One value rather than three independent `Option`s. A pointer moves one
/// thing, and three fields let the type say otherwise: the press handler armed
/// each of them from its own `if`, so a position inside two dilated rectangles
/// started two gestures, and every later move applied both.
///
/// Every variant captures the rectangle it was painted at rather than reading
/// it again on each move. That is what lets the pointer leave the control, or
/// the row below it, and keep moving the value it grabbed, and what keeps a
/// second row's control from answering for the one under the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Brightness {
        row: LightingRow,
        track: Bounds<Pixels>,
    },
    Duty {
        channel: Channel,
        track: Bounds<Pixels>,
    },
    /// A curve node. The node is fixed at the press and does not change for the
    /// rest of the gesture, so a drag moves the point that was grabbed rather
    /// than whichever one the pointer happens to be over. `base` is the whole
    /// curve as it stood at that moment, which is what
    /// [`CoolingEditor::set_node_from`] replays every move against.
    Curve {
        channel: Channel,
        node: usize,
        base: CurveNodes,
        plot: Bounds<Pixels>,
    },
}
impl Drag {
    /// Whether this gesture is editing the cooling program.
    ///
    /// Both cooling gestures hold back the autosave and the adoption of what
    /// the daemon committed; a brightness drag on another screen holds back
    /// neither.
    pub fn is_cooling(self) -> bool {
        matches!(self, Self::Duty { .. } | Self::Curve { .. })
    }
}
/// Where each operable track was painted, by whatever names the control.
///
/// The window decides which slider a press landed on, rather than the slider
/// hearing about the press itself. A listener on the control was the first
/// arrangement and it never fired: the press has to travel down an element
/// whose interactive state, hover styling and focus ring all sit between it and
/// the handler. The window's own capture handler is the one place an event is
/// guaranteed to arrive, and the same handler already runs there for focus.
///
/// A disabled slider records nothing, so a track that cannot be moved cannot be
/// grabbed either.
///
/// Keyed by whatever names the control rather than by a lighting row: the same
/// arrangement is what the fixed-duty sliders on Cooling need, and two copies of
/// this bookkeeping would be two places for a stale rectangle to survive.
#[derive(Debug)]
pub struct TrackMap<K>(RefCell<Vec<(K, Bounds<Pixels>)>>);
impl<K> Default for TrackMap<K> {
    fn default() -> Self {
        Self(RefCell::new(Vec::new()))
    }
}
impl<K: Copy + PartialEq> TrackMap<K> {
    /// Publish where one key's track was painted, replacing its last position.
    pub fn record(&self, key: K, track: Bounds<Pixels>) {
        let mut tracks = self.0.borrow_mut();
        match tracks.iter_mut().find(|(known, _)| *known == key) {
            Some(entry) => entry.1 = track,
            None => tracks.push((key, track)),
        }
    }

    /// Forget every track, so a row that went away takes its rectangle with it.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// The track under a pointer position, if any.
    ///
    /// Dilated a little: the handle is 22 pixels tall inside a taller control,
    /// and a press a pixel above the track is a press on the track as far as
    /// the operator is concerned.
    pub fn at(&self, position: Point<Pixels>) -> Option<(K, Bounds<Pixels>)> {
        self.0
            .borrow()
            .iter()
            .find(|(_, track)| track.dilate(TRACK_GRAB_MARGIN).contains(&position))
            .copied()
    }
}
/// How far outside a track a press still counts as landing on it.
pub const TRACK_GRAB_MARGIN: Pixels = px(6.0);

impl Shell {
    /// Begin whatever gesture a press at `position` starts, if it starts one.
    ///
    /// One answer for the whole window, because a pointer moves one thing. The
    /// books are asked in the order the controls are stacked, and each only
    /// holds rectangles this frame painted for a control the hardware accepts,
    /// so a press that reaches here has already been filtered by the screen
    /// that is drawn, the rows that are open and the capability gate. Deciding
    /// it here rather than from a listener on each control is what makes it
    /// arrive at all: an event that has to reach a control nested under its own
    /// interactive state never did.
    pub(crate) fn begin_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some((row, track)) = self.brightness_tracks.at(position) {
            self.popover = None;
            self.drag = Some(Drag::Brightness { row, track });
        } else if let Some((channel, track)) = self.duty_tracks.at(position) {
            self.popover = None;
            self.drag = Some(Drag::Duty { channel, track });
        } else if let Some((channel, plot)) = self.curve_plots.at(position) {
            // The node is chosen once, here, and held for the rest of the
            // gesture, so a drag that wanders keeps editing the point the
            // operator aimed at. The press is already an edit: it moves that
            // node to where it landed.
            let (node, duty) = node_at(plot, position);
            let base = *self.cooling.curve(channel);
            self.drag = Some(Drag::Curve {
                channel,
                node,
                base,
                plot,
            });
            self.cooling.select_node(channel, node);
            self.cooling.set_node_from(channel, base, node, duty);
            self.touch_cooling();
        } else {
            return;
        }
        self.drag_to(position, cx);
    }

    /// Move whatever is being dragged to a pointer position.
    ///
    /// Every rectangle was captured at the press, so a gesture that leaves its
    /// control keeps pinning the end it left by rather than stopping where the
    /// pointer crossed the edge.
    pub(crate) fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        match drag {
            Drag::Brightness { row, track } => {
                let max = f32::from(kori_core::lighting::MAX_BRIGHTNESS);
                let value = Slider::value_at(track, position, 0.0, max);
                self.set_brightness(row, value.round() as i16);
                cx.notify();
            }
            // Read as whole percent for the reason `node_at` reads the plot
            // that way: the driver stores a percentage, so a finer reading
            // would offer positions the hardware cannot hold and would let a
            // pixel of hand tremor produce a write that changes nothing.
            Drag::Duty { channel, track } => {
                let floor = f32::from(duty_to_percent(channel.min_duty()));
                let percent = Slider::value_at(track, position, floor, f32::from(MAX_DUTY_PERCENT));
                self.cooling
                    .set_duty(channel, duty_from_percent(percent.round() as u8));
                cx.notify();
            }
            // Only the height is read, so a gesture that wanders sideways does
            // not drag a furrow across the plot.
            Drag::Curve {
                channel,
                node,
                base,
                plot,
            } => {
                let (_, duty) = node_at(plot, position);
                let before = *self.cooling.curve(channel);
                self.cooling.set_node_from(channel, base, node, duty);
                // A pointer reports far more moves than a 255-step scale has
                // values, and a horizontal drag reports moves that change
                // nothing at all. Repainting for those is what makes the
                // gesture feel heavy.
                if *self.cooling.curve(channel) != before {
                    self.touch_cooling();
                    cx.notify();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duty_track_and_a_brightness_track_never_answer_for_each_other() {
        // Two books rather than one keyed by a sum type: a rectangle left
        // behind by a screen that is no longer drawn must not be grabbable, and
        // the press handler asks each book for its own kind of drag.
        let duty: TrackMap<Channel> = TrackMap::default();
        let at = Point {
            x: px(300.0),
            y: px(200.0),
        };
        duty.record(
            Channel::Pump,
            Bounds {
                origin: Point {
                    x: px(200.0),
                    y: px(190.0),
                },
                size: gpui::size(px(200.0), px(22.0)),
            },
        );
        assert_eq!(duty.at(at).map(|(channel, _)| channel), Some(Channel::Pump));
        duty.clear();
        assert_eq!(duty.at(at), None);
    }

    fn track(row: LightingRow, x: f32, y: f32) -> (LightingRow, Bounds<Pixels>) {
        (
            row,
            Bounds {
                origin: Point { x: px(x), y: px(y) },
                size: gpui::size(px(110.0), px(22.0)),
            },
        )
    }

    #[test]
    fn a_press_finds_the_track_it_landed_on_and_no_other() {
        // Three rows' sliders sit in one column, 87 pixels apart, which is what
        // the screen lays out. Picking the wrong one would move a channel the
        // operator never touched.
        let tracks = TrackMap::default();
        for (row, bounds) in [
            track(LightingRow::Channel(1), 576.0, 210.0),
            track(LightingRow::Channel(2), 576.0, 297.0),
            track(LightingRow::Lcd, 576.0, 573.0),
        ] {
            tracks.record(row, bounds);
        }

        let at = |x: f32, y: f32| tracks.at(Point { x: px(x), y: px(y) }).map(|(row, _)| row);

        assert_eq!(at(600.0, 220.0), Some(LightingRow::Channel(1)));
        assert_eq!(at(600.0, 305.0), Some(LightingRow::Channel(2)));
        assert_eq!(at(680.0, 580.0), Some(LightingRow::Lcd));
        // Between two rows, and left of the column: a press there is not a
        // press on a slider, and must not move one.
        assert_eq!(at(600.0, 260.0), None);
        assert_eq!(at(400.0, 220.0), None);

        // A few pixels off the track still counts: the rail is four pixels
        // tall inside a taller control, and aiming at it exactly is not what
        // the operator is doing.
        assert_eq!(at(600.0, 206.0), Some(LightingRow::Channel(1)));
        assert_eq!(at(572.0, 220.0), Some(LightingRow::Channel(1)));
    }

    #[test]
    fn a_row_that_went_away_takes_its_track_with_it() {
        let tracks = TrackMap::default();
        let (row, bounds) = track(LightingRow::Channel(1), 576.0, 210.0);
        tracks.record(row, bounds);
        let inside = Point {
            x: px(600.0),
            y: px(220.0),
        };
        assert!(tracks.at(inside).is_some());

        // Re-recording moves the rectangle rather than adding a second one, so
        // a row cannot accumulate stale hit areas as the screen scrolls.
        let (_, moved) = track(LightingRow::Channel(1), 576.0, 400.0);
        tracks.record(row, moved);
        assert_eq!(tracks.at(inside), None);
        assert!(
            tracks
                .at(Point {
                    x: px(600.0),
                    y: px(410.0)
                })
                .is_some()
        );

        // And a frame that draws no slider leaves nothing to grab.
        tracks.clear();
        assert_eq!(
            tracks.at(Point {
                x: px(600.0),
                y: px(410.0)
            }),
            None
        );
    }
}
