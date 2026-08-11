// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The editable form of a cooling curve.
//!
//! The kernel takes forty duties, one per degree. An operator does not edit
//! forty of anything, so the editor holds nine nodes and this module is the
//! exact, lossless translation between the two. Nothing here validates and
//! nothing here writes: a node set becomes a [`TemperatureCurve`], and the
//! curve passes the same gate as any other.

use super::{CURVE_FIRST_TEMP_C, CURVE_POINT_COUNT, MAX_DUTY, TemperatureCurve, duty_from_percent};

/// Degrees between two adjacent editor nodes.
///
/// Five, so every node lands on a round temperature and the axis reads 20, 25,
/// 30 rather than the arbitrary 20, 24, 29 that spacing ten nodes evenly across
/// forty points produces. The ABI's own range is what bounds this: there is no
/// point above 59 C to put a node on.
pub const CURVE_NODE_STEP_C: usize = 5;

/// Control nodes the editor exposes, fewer than the ABI's points.
///
/// Forty draggable points would be unusable and would let a curve wobble
/// between adjacent degrees. Nine nodes every five degrees over the same
/// 20-59 C range are what the operator edits; [`CurveNodes::interpolate`] turns
/// them into the exact 40 integer values the kernel accepts.
pub const CURVE_NODE_COUNT: usize = 9;

/// The editable form of a curve: one duty per node over the fixed kernel range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveNodes {
    /// Duty of each node, 0-255, in ascending temperature order.
    pub duty: [u8; CURVE_NODE_COUNT],
}

impl CurveNodes {
    /// ABI point index a node sits on.
    ///
    /// One point per whole [`CURVE_NODE_STEP_C`] degrees, so every node names a
    /// round temperature. The last node is the exception: five degrees past
    /// 55 C is 60 C, which the ABI does not reach, so it sits on the last point
    /// the kernel has. That makes the final segment four degrees wide where the
    /// others are five, which is a degree of drawing error and no error at all
    /// in what gets written.
    ///
    /// Landing on whole points is what makes [`CurveNodes::from_curve`] the
    /// exact inverse of [`CurveNodes::interpolate`]: a node read back off a
    /// stored curve is the value that was written, not a value re-derived
    /// through two roundings that would creep by one step per edit cycle.
    pub fn point_index(index: usize) -> usize {
        (index * CURVE_NODE_STEP_C).min(CURVE_POINT_COUNT - 1)
    }

    /// Temperature of the node at `index`, in whole degrees Celsius.
    ///
    /// The first node sits on the first ABI point and the last on the last, so
    /// the editor spans exactly the range the kernel accepts.
    pub fn temperature_at(index: usize) -> f32 {
        (CURVE_FIRST_TEMP_C as usize + Self::point_index(index)) as f32
    }

    /// Every node at the same duty.
    pub fn flat(duty: u8) -> Self {
        Self {
            duty: [duty; CURVE_NODE_COUNT],
        }
    }

    /// Set one node, keeping the whole set monotonically non-decreasing.
    ///
    /// Raising a node lifts every node above it and lowering one pulls every
    /// node below it. Monotonicity is therefore a property of the editor, not
    /// something the operator has to reconstruct after each edit; validation
    /// still runs before Apply because the daemon does not trust the client.
    pub fn set(&mut self, index: usize, duty: u8) {
        if index >= CURVE_NODE_COUNT {
            return;
        }
        self.duty[index] = duty;
        for higher in index + 1..CURVE_NODE_COUNT {
            if self.duty[higher] < duty {
                self.duty[higher] = duty;
            }
        }
        for lower in (0..index).rev() {
            if self.duty[lower] > duty {
                self.duty[lower] = duty;
            }
        }
    }

    /// Expand the nodes into exactly [`CURVE_POINT_COUNT`] integer duties.
    ///
    /// Linear between nodes, rounded to the nearest integer. Rounding is
    /// monotone, so a non-decreasing node set always produces a non-decreasing
    /// curve.
    pub fn interpolate(&self) -> TemperatureCurve {
        let points = (0..CURVE_POINT_COUNT)
            .map(|point| {
                // Which segment the point falls in, by arithmetic rather than
                // by search: node `n` sits on point `n * CURVE_NODE_STEP_C`.
                // Capping at the second-to-last node is what keeps the two ends
                // of the segment on distinct points, including for the last
                // node, which the ABI pins one step short of its own spacing.
                let lower = (point / CURVE_NODE_STEP_C).min(CURVE_NODE_COUNT - 2);
                let low_point = Self::point_index(lower);
                let high_point = Self::point_index(lower + 1);
                let low = self.duty[lower] as f32;
                let high = self.duty[lower + 1] as f32;
                let fraction = (point - low_point) as f32 / (high_point - low_point) as f32;
                (low + (high - low) * fraction)
                    .round()
                    .clamp(0.0, MAX_DUTY as f32) as u8
            })
            .collect();
        TemperatureCurve { points }
    }

    /// The curve the editor starts from when no profile defines one.
    ///
    /// A linear ramp from 40% to 100% over the whole range: above the pump
    /// floor at every node, and already monotonic, so the first Apply cannot be
    /// the first time validation is exercised.
    pub fn starting_ramp() -> Self {
        let mut duty = [0u8; CURVE_NODE_COUNT];
        for (index, entry) in duty.iter_mut().enumerate() {
            // Built in percent and converted, so the curve the editor opens on
            // sits on the same grid a drag produces. A default that was the one
            // value off the grid would read as an edit the moment it was
            // touched.
            let percent = 40.0 + index as f32 * (60.0 / (CURVE_NODE_COUNT - 1) as f32);
            *entry = duty_from_percent(percent.round().clamp(0.0, 100.0) as u8);
        }
        Self { duty }
    }

    /// Read nodes back out of a stored curve, for the editor to load.
    ///
    /// Each node sits on a whole ABI point, so this reads the stored value
    /// directly and is the exact inverse of [`CurveNodes::interpolate`].
    ///
    /// A curve with the wrong point count cannot be sampled, so the caller
    /// gets `None` rather than a set built from whatever was there.
    pub fn from_curve(curve: &TemperatureCurve) -> Option<Self> {
        if curve.points.len() != CURVE_POINT_COUNT {
            return None;
        }
        let mut duty = [0u8; CURVE_NODE_COUNT];
        for (index, entry) in duty.iter_mut().enumerate() {
            *entry = curve.points[Self::point_index(index)];
        }
        Some(Self { duty })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{
        CURVE_LAST_TEMP_C, Channel, MIN_PUMP_DUTY, duty_to_percent, validate_curve,
    };

    /// A default that sat off the grid would read as an edit as soon as any
    /// node was touched, and with autosave it would be written.
    #[test]
    fn the_starting_ramp_sits_on_the_grid() {
        for duty in CurveNodes::starting_ramp().duty {
            assert_eq!(
                duty_from_percent(duty_to_percent(duty)),
                duty,
                "node duty {duty} is not a whole percentage"
            );
        }
    }

    #[test]
    fn the_nodes_span_exactly_the_kernel_range_on_round_degrees() {
        assert_eq!(CURVE_NODE_COUNT, 9);
        assert_eq!(CurveNodes::temperature_at(0), CURVE_FIRST_TEMP_C as f32);
        assert!(
            (CurveNodes::temperature_at(CURVE_NODE_COUNT - 1) - CURVE_LAST_TEMP_C as f32).abs()
                < 0.001
        );
        assert!(
            (0..CURVE_NODE_COUNT)
                .map(CurveNodes::temperature_at)
                .all(|t| (CURVE_FIRST_TEMP_C as f32..=CURVE_LAST_TEMP_C as f32).contains(&t))
        );
    }

    #[test]
    fn nodes_interpolate_to_exactly_forty_integer_points() {
        let nodes = CurveNodes::starting_ramp();
        let curve = nodes.interpolate();
        assert_eq!(curve.points.len(), CURVE_POINT_COUNT);
        // The endpoints land on the nodes rather than near them.
        assert_eq!(curve.points[0], nodes.duty[0]);
        assert_eq!(
            curve.points[CURVE_POINT_COUNT - 1],
            nodes.duty[CURVE_NODE_COUNT - 1]
        );
        assert!(validate_curve(Channel::Pump, &curve).is_ok());
        assert!(validate_curve(Channel::Fan, &curve).is_ok());
    }

    #[test]
    fn interpolation_between_two_nodes_is_linear() {
        let mut nodes = CurveNodes::flat(0);
        nodes.duty = [0, 90, 90, 90, 90, 90, 90, 90, 90];
        let curve = nodes.interpolate();
        // Node 1 sits on point index 5, so point 2 is two fifths of the way
        // up: 0 + 90 * 2/5 = 36.
        assert_eq!(curve.points[0], 0);
        assert_eq!(curve.points[2], 36);
        assert_eq!(curve.points[5], 90);
        assert!(curve.points.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    /// Every point lands on the segment its own two nodes define. The last
    /// segment is the one worth pinning: the ABI pins node 8 to point 39 rather
    /// than to the 40 its spacing asks for, so that segment is four points wide
    /// where the others are five.
    #[test]
    fn every_point_sits_on_the_segment_its_own_two_nodes_define() {
        let nodes = CurveNodes {
            duty: [0, 10, 30, 60, 100, 150, 200, 230, 255],
        };
        let curve = nodes.interpolate();

        for (index, &duty) in curve.points.iter().enumerate() {
            let lower = (index / CURVE_NODE_STEP_C).min(CURVE_NODE_COUNT - 2);
            let (low, high) = (nodes.duty[lower], nodes.duty[lower + 1]);
            assert!(
                (low..=high).contains(&duty),
                "point {index} is {duty}, outside the {low}-{high} segment it belongs to"
            );
        }

        // Both ends of the last, shorter segment are exact.
        assert_eq!(curve.points[35], nodes.duty[7]);
        assert_eq!(curve.points[CURVE_POINT_COUNT - 1], nodes.duty[8]);
        // And its midpoint is interpolated over four points, not five.
        assert_eq!(curve.points[37], 243);
    }

    #[test]
    fn nodes_sit_on_whole_points_and_whole_degrees() {
        let indices: Vec<usize> = (0..CURVE_NODE_COUNT).map(CurveNodes::point_index).collect();
        assert_eq!(indices, vec![0, 5, 10, 15, 20, 25, 30, 35, 39]);
        assert!(
            indices.windows(2).all(|pair| pair[0] < pair[1]),
            "two nodes must never share a point"
        );
        let temperatures: Vec<f32> = (0..CURVE_NODE_COUNT)
            .map(CurveNodes::temperature_at)
            .collect();
        assert!(
            temperatures.iter().all(|t| t.fract() == 0.0),
            "{temperatures:?}"
        );
        assert_eq!(temperatures.first(), Some(&20.0));
        assert_eq!(temperatures.last(), Some(&59.0));
    }

    /// The axis an operator reads has to be made of round temperatures. Every
    /// node but the last sits on a multiple of the step; the last sits on the
    /// highest point the ABI has, because there is nothing above it to sit on.
    #[test]
    fn every_node_but_the_last_lands_on_a_round_temperature() {
        let temperatures: Vec<f32> = (0..CURVE_NODE_COUNT)
            .map(CurveNodes::temperature_at)
            .collect();
        assert_eq!(
            temperatures,
            vec![20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0, 55.0, 59.0]
        );

        let step = CURVE_NODE_STEP_C as f32;
        for (index, temperature) in temperatures.iter().enumerate().take(CURVE_NODE_COUNT - 1) {
            assert_eq!(
                temperature % step,
                0.0,
                "node {index} is at {temperature} C, not a multiple of {step}"
            );
        }
        assert_eq!(
            *temperatures.last().unwrap(),
            CURVE_LAST_TEMP_C as f32,
            "the last node is pinned to the end of the ABI, not to a round degree"
        );
    }

    #[test]
    fn editing_one_node_keeps_the_whole_set_monotonic() {
        let mut nodes = CurveNodes::flat(100);
        nodes.set(4, 200);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(nodes.duty[3], 100);
        assert_eq!(nodes.duty[4], 200);
        assert_eq!(
            nodes.duty[CURVE_NODE_COUNT - 1],
            200,
            "higher nodes are lifted"
        );

        nodes.set(7, 50);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(nodes.duty[7], 50);
        assert_eq!(nodes.duty[4], 50, "lower nodes are pulled down");
        assert_eq!(
            nodes.duty[CURVE_NODE_COUNT - 1],
            200,
            "nodes above are untouched"
        );

        // An index outside the set changes nothing rather than panicking.
        let before = nodes;
        nodes.set(CURVE_NODE_COUNT, 255);
        assert_eq!(nodes, before);
    }

    #[test]
    fn an_edited_node_set_always_validates_as_a_curve() {
        let mut nodes = CurveNodes::starting_ramp();
        for (index, duty) in [(0, 255), (8, 60), (5, 130), (2, 200)] {
            nodes.set(index, duty);
            let curve = nodes.interpolate();
            assert!(
                validate_curve(Channel::Fan, &curve).is_ok(),
                "set({index}, {duty}) produced {curve:?}"
            );
        }
    }

    #[test]
    fn nodes_round_trip_through_a_stored_curve() {
        for nodes in [
            CurveNodes::starting_ramp(),
            CurveNodes::flat(200),
            CurveNodes {
                duty: [51, 51, 60, 80, 110, 150, 190, 240, 255],
            },
        ] {
            // Exact, not merely close: an edit cycle that crept by one PWM
            // step would drift a saved curve over repeated loads.
            assert_eq!(CurveNodes::from_curve(&nodes.interpolate()), Some(nodes));
        }

        // A curve of the wrong length cannot be sampled into nodes.
        assert!(
            CurveNodes::from_curve(&TemperatureCurve {
                points: vec![100; 12]
            })
            .is_none()
        );
    }

    #[test]
    fn the_starting_ramp_clears_the_pump_floor_at_every_node() {
        let nodes = CurveNodes::starting_ramp();
        assert!(nodes.duty.iter().all(|duty| *duty >= MIN_PUMP_DUTY));
        assert_eq!(nodes.duty[CURVE_NODE_COUNT - 1], 255);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}
