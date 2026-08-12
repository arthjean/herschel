// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! The two forms of a cooling curve, and the translation between them.
//!
//! [`TemperatureCurve`] is what the kernel takes: forty duties, one per degree,
//! and exactly forty is a property of the type rather than a rule each caller
//! rechecks. [`CurveNodes`] is what an operator edits, because nobody drags
//! forty of anything: nine nodes every five degrees over the same range.
//!
//! Nothing here validates and nothing here writes. A node set becomes a curve
//! through [`CurveNodes::interpolate`], and that curve passes the same gate as
//! any other, in [`super::validate_curve`].

use serde::{Deserialize, Serialize};

use super::{CURVE_FIRST_TEMP_C, CURVE_POINT_COUNT, MAX_DUTY, ValidationError, duty_from_percent};

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

/// The wire and on-disk shape of a curve, which is a list of unknown length.
///
/// Kept separate from [`TemperatureCurve`] so the length is checked exactly
/// once, at the boundary where a list of unknown length actually arrives, and
/// so the object written to a config file or a socket frame stays the
/// `{"points": [..]}` it has always been.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurveWire {
    points: Vec<u8>,
}

/// Exactly one PWM value per kernel curve point.
///
/// The point count is a property of the type rather than something each caller
/// rechecks. It was the latter, and the four separate checks that resulted did
/// not cover the same ground: the divergence window in the daemon indexes this
/// array directly, and it stayed in bounds only because a length check in a
/// different crate happened to run two lines earlier. An invariant that holds
/// by construction is what makes that indexing safe to read.
///
/// A curve arriving from a socket frame or a config file is a list of unknown
/// length, so it enters through [`TemperatureCurve::new`] and is refused there
/// when it does not carry the ABI's count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CurveWire", into = "CurveWire")]
pub struct TemperatureCurve {
    points: [u8; CURVE_POINT_COUNT],
}

impl TryFrom<CurveWire> for TemperatureCurve {
    type Error = ValidationError;

    fn try_from(wire: CurveWire) -> Result<Self, Self::Error> {
        Self::new(wire.points)
    }
}

impl From<TemperatureCurve> for CurveWire {
    fn from(curve: TemperatureCurve) -> Self {
        Self {
            points: curve.points.to_vec(),
        }
    }
}

impl TemperatureCurve {
    /// A curve from a list whose length is not known yet.
    ///
    /// The one door into the type from outside, and the only place the ABI's
    /// point count is compared against anything.
    pub fn new(points: Vec<u8>) -> Result<Self, ValidationError> {
        let points: [u8; CURVE_POINT_COUNT] =
            points
                .as_slice()
                .try_into()
                .map_err(|_| ValidationError::CurvePointCount {
                    expected: CURVE_POINT_COUNT,
                    actual: points.len(),
                })?;
        Ok(Self { points })
    }

    /// A curve from a list that already carries the ABI's count.
    ///
    /// For a caller that built the array from [`CURVE_POINT_COUNT`] itself, so
    /// it does not have to handle a refusal that cannot happen. Without it such
    /// a caller would reach for an unwrap on a `Result` it can prove is `Ok`,
    /// which is the failure mode this crate refuses everywhere else.
    pub fn from_points(points: [u8; CURVE_POINT_COUNT]) -> Self {
        Self { points }
    }

    /// The duty commanded at each ABI point, in ascending temperature order.
    pub fn points(&self) -> &[u8; CURVE_POINT_COUNT] {
        &self.points
    }

    /// The same, for a caller editing a point in place.
    ///
    /// A `&mut [u8; N]` lets every existing edit through and lets no caller
    /// change how many points there are, which is the whole invariant.
    pub fn points_mut(&mut self) -> &mut [u8; CURVE_POINT_COUNT] {
        &mut self.points
    }

    /// Temperature in degrees Celsius of the point at `index`.
    pub fn temperature_at(index: usize) -> u8 {
        CURVE_FIRST_TEMP_C + index as u8
    }

    /// A flat curve at `duty`, used as a starting point in the editor.
    pub fn flat(duty: u8) -> Self {
        Self {
            points: [duty; CURVE_POINT_COUNT],
        }
    }

    /// Index of the point that governs `temperature_c`.
    ///
    /// Clamped at both ends: the ABI starts at 20 C and the firmware runs the
    /// last point above 59 C, so a reading outside the range still names the
    /// point that is in force.
    ///
    /// `None` for a temperature that is not a number. Clamping propagates a
    /// NaN and the cast that follows turns it into index 0, which would answer
    /// "the duty commanded at 20 C" for a reading nothing measured: the same
    /// fabricated default [`crate::telemetry::clamp_percent`] refuses, in the
    /// path that decides whether the device is running the committed curve.
    pub fn point_index_for(temperature_c: f32) -> Option<usize> {
        if !temperature_c.is_finite() {
            return None;
        }
        let offset = (temperature_c - CURVE_FIRST_TEMP_C as f32).round();
        Some(offset.clamp(0.0, (CURVE_POINT_COUNT - 1) as f32) as usize)
    }

    /// The duty this curve commands at `temperature_c`.
    ///
    /// `None` only when the temperature names no point, which is a temperature
    /// that is not a number. Every curve carries every point.
    pub fn duty_at(&self, temperature_c: f32) -> Option<u8> {
        Some(self.points[Self::point_index_for(temperature_c)?])
    }

    /// The highest duty this curve ever commands.
    ///
    /// The last point, because [`super::validate_curve`] refuses a curve that
    /// decreases. Named here so the callers that need a safe fallback duty do
    /// not each restate that reasoning next to an indexing expression.
    pub fn highest_duty(&self) -> u8 {
        self.points[CURVE_POINT_COUNT - 1]
    }
}

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
        let mut points = [0u8; CURVE_POINT_COUNT];
        for (point, entry) in points.iter_mut().enumerate() {
            // Which segment the point falls in, by arithmetic rather than by
            // search: node `n` sits on point `n * CURVE_NODE_STEP_C`. Capping
            // at the second-to-last node is what keeps the two ends of the
            // segment on distinct points, including for the last node, which
            // the ABI pins one step short of its own spacing.
            let lower = (point / CURVE_NODE_STEP_C).min(CURVE_NODE_COUNT - 2);
            let low_point = Self::point_index(lower);
            let high_point = Self::point_index(lower + 1);
            let low = self.duty[lower] as f32;
            let high = self.duty[lower + 1] as f32;
            let fraction = (point - low_point) as f32 / (high_point - low_point) as f32;
            *entry = (low + (high - low) * fraction)
                .round()
                .clamp(0.0, MAX_DUTY as f32) as u8;
        }
        TemperatureCurve::from_points(points)
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
    /// `None` when the curve is not one this editor could have drawn. Nine
    /// nodes describe forty points only when the thirty-one points between them
    /// lie on the segments they define; a curve that came from somewhere else,
    /// a hand-edited file or a future editor with a different node count, does
    /// not. Sampling it anyway would put a plot on screen whose own
    /// interpolation is a different curve from the one the device is running,
    /// and the next Apply would silently write that different curve. So the
    /// answer is checked rather than assumed: the nodes are sampled, expanded
    /// again, and compared against what was passed in.
    pub fn from_curve(curve: &TemperatureCurve) -> Option<Self> {
        let mut duty = [0u8; CURVE_NODE_COUNT];
        for (index, entry) in duty.iter_mut().enumerate() {
            *entry = curve.points()[Self::point_index(index)];
        }
        let nodes = Self { duty };
        (nodes.interpolate() == *curve).then_some(nodes)
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
        assert_eq!(curve.points().len(), CURVE_POINT_COUNT);
        // The endpoints land on the nodes rather than near them.
        assert_eq!(curve.points()[0], nodes.duty[0]);
        assert_eq!(
            curve.points()[CURVE_POINT_COUNT - 1],
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
        assert_eq!(curve.points()[0], 0);
        assert_eq!(curve.points()[2], 36);
        assert_eq!(curve.points()[5], 90);
        assert!(curve.points().windows(2).all(|pair| pair[0] <= pair[1]));
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

        for (index, &duty) in curve.points().iter().enumerate() {
            let lower = (index / CURVE_NODE_STEP_C).min(CURVE_NODE_COUNT - 2);
            let (low, high) = (nodes.duty[lower], nodes.duty[lower + 1]);
            assert!(
                (low..=high).contains(&duty),
                "point {index} is {duty}, outside the {low}-{high} segment it belongs to"
            );
        }

        // Both ends of the last, shorter segment are exact.
        assert_eq!(curve.points()[35], nodes.duty[7]);
        assert_eq!(curve.points()[CURVE_POINT_COUNT - 1], nodes.duty[8]);
        // And its midpoint is interpolated over four points, not five.
        assert_eq!(curve.points()[37], 243);
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
    }

    /// Nine nodes cannot describe every curve the ABI can hold. One that bends
    /// between two nodes is read back as `None` rather than as the nine values
    /// that happen to sit under the node positions, because those nine expand
    /// into a different curve from the one stored, and the editor would then be
    /// showing a plot the device is not running.
    #[test]
    fn a_curve_this_editor_could_not_have_drawn_is_not_sampled_into_nodes() {
        let mut bent = CurveNodes::flat(120).interpolate();
        // Points 0 and 5 carry two adjacent nodes, so point 2 lies on the
        // segment between them. Moving it alone puts the curve off that
        // segment while leaving every node position untouched.
        bent.points_mut()[2] = 200;
        assert!(CurveNodes::from_curve(&bent).is_none());

        // The nodes themselves still read back, which is what makes the
        // refusal about the shape between them rather than about the values.
        assert_eq!(bent.points()[0], 120);
        assert_eq!(bent.points()[5], 120);

        // And a curve that does lie on its segments still round-trips.
        let drawn = CurveNodes::flat(120).interpolate();
        assert_eq!(
            CurveNodes::from_curve(&drawn),
            Some(CurveNodes::flat(120)),
            "a curve this editor drew has to load back into the editor"
        );
    }

    #[test]
    fn the_starting_ramp_clears_the_pump_floor_at_every_node() {
        let nodes = CurveNodes::starting_ramp();
        assert!(nodes.duty.iter().all(|duty| *duty >= MIN_PUMP_DUTY));
        assert_eq!(nodes.duty[CURVE_NODE_COUNT - 1], 255);
        assert!(nodes.duty.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    /// The count is refused at the one door a list of unknown length comes
    /// through, rather than by each caller that later reads the curve.
    #[test]
    fn a_list_that_is_not_forty_points_never_becomes_a_curve() {
        for actual in [0usize, 39, 41] {
            let error = TemperatureCurve::new(vec![100; actual]).unwrap_err();
            assert_eq!(
                error,
                ValidationError::CurvePointCount {
                    expected: CURVE_POINT_COUNT,
                    actual
                }
            );
        }
        assert!(TemperatureCurve::new(vec![100; CURVE_POINT_COUNT]).is_ok());
    }

    /// The same door, reached from the wire. A stored profile or a socket frame
    /// carrying a short curve is refused while it is still a frame, so nothing
    /// downstream has to ask again.
    #[test]
    fn a_curve_of_the_wrong_length_is_refused_at_deserialization() {
        let short = r#"{"points":[100,100,100]}"#;
        let error = serde_json::from_str::<TemperatureCurve>(short).unwrap_err();
        assert!(error.to_string().contains("exactly 40"), "{error}");

        // And the accepted form is byte for byte what earlier builds wrote, so
        // no profile on disk needs rewriting and no schema version moves.
        let curve = TemperatureCurve::flat(120);
        let json = serde_json::to_string(&curve).unwrap();
        assert!(json.starts_with(r#"{"points":["#), "{json}");
        assert_eq!(
            serde_json::from_str::<TemperatureCurve>(&json).unwrap(),
            curve
        );
    }

    /// A temperature that is not a number names no point. The clamped form
    /// answered index 0, which on a validated curve is its lowest duty, so an
    /// unreadable temperature would have read as the coolest one there is.
    #[test]
    fn a_temperature_that_is_not_a_number_names_no_point_and_no_duty() {
        let curve = TemperatureCurve::flat(200);
        for impossible in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                TemperatureCurve::point_index_for(impossible),
                None,
                "{impossible}"
            );
            assert_eq!(curve.duty_at(impossible), None, "{impossible}");
        }

        // A reading outside the ABI's range still names the point in force.
        assert_eq!(TemperatureCurve::point_index_for(-40.0), Some(0));
        assert_eq!(
            TemperatureCurve::point_index_for(200.0),
            Some(CURVE_POINT_COUNT - 1)
        );
        assert_eq!(curve.duty_at(31.2), Some(200));
    }

    /// The fallback duty callers reach for when nothing can be measured.
    #[test]
    fn the_highest_duty_is_the_last_point_of_a_non_decreasing_curve() {
        let curve = CurveNodes::starting_ramp().interpolate();
        assert!(validate_curve(Channel::Pump, &curve).is_ok());
        assert_eq!(curve.highest_duty(), MAX_DUTY);
        assert_eq!(
            curve.highest_duty(),
            *curve.points().iter().max().unwrap(),
            "a validated curve never decreases, so its last point is its highest"
        );
    }

    #[test]
    fn curve_temperature_range_matches_the_kernel_abi() {
        assert_eq!(TemperatureCurve::temperature_at(0), CURVE_FIRST_TEMP_C);
        assert_eq!(
            TemperatureCurve::temperature_at(CURVE_POINT_COUNT - 1),
            CURVE_LAST_TEMP_C
        );
        assert_eq!(CURVE_LAST_TEMP_C, 59);
    }
}
