// SPDX-License-Identifier: AGPL-3.0-only
//! Draw area: the point-by-point polygon editor behind a Spot's (or an exclusion zone's) drawn
//! area, as in the canvas prototype (`SpotDrawArea.dc.html`).
//!
//! The editor is a value: each action returns the next editor, and [`PolygonEditor::view`] says
//! what the screen shows. Points are coordinates; the shell turns taps into coordinates with its
//! map. Whether a closed shape is usable is pingers-geo's [`Polygon`] rule (at least three
//! points, no crossing edges, some area, not too large), so the editor never accepts a shape the
//! rest of the app would reject.

use pingers_geo::{Coordinate, GeoError, Polygon, SquareMeters};

use crate::ScreenError;
use crate::common::{Name, midpoint};

/// How many steps Undo remembers.
pub const HISTORY: usize = 50;

/// A point's position in the editor's list, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PointIndex(usize);

impl PointIndex {
    /// Wraps a position from the shell (the point or "+" that was tapped).
    #[must_use]
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// The position, from 0.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Whether the shape is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Closure {
    Open,
    Closed,
}

/// What Undo restores.
#[derive(Clone, Debug, PartialEq)]
struct Step {
    points: Vec<Coordinate>,
    closure: Closure,
}

/// The editor.
#[derive(Clone, Debug, PartialEq)]
pub struct PolygonEditor {
    points: Vec<Coordinate>,
    closure: Closure,
    selected: Option<PointIndex>,
    history: Vec<Step>,
    /// The history as it was when the current drag began, so a whole drag undoes in one step.
    drag_start: Option<Vec<Step>>,
}

/// Why a closed shape cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeProblem {
    /// Two edges cross or touch, or a point repeats.
    Crossing,
    /// All points lie on one line.
    NoArea,
    /// It spans more than pingers-geo allows.
    TooLarge,
}

impl ShapeProblem {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "GeoError is non-exhaustive; every other rejection is fixed by moving a point"
    )]
    fn of(error: GeoError) -> Self {
        match error {
            GeoError::PolygonTooLarge => Self::TooLarge,
            GeoError::Degenerate | GeoError::TooFewVertices => Self::NoArea,
            // Any other rejection (crossing edges, or a coordinate pingers-geo could not
            // project) is fixed the same way: move a point.
            _ => Self::Crossing,
        }
    }
}

/// What the editor tells the user to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrawHint {
    /// "Tap the map to drop the first point."
    FirstPoint,
    /// "Keep tapping to add points. You need at least 3."
    KeepTapping,
    /// "Tap the orange first point to close the shape."
    CloseShape,
    /// The closed shape cannot be used; move a point.
    Fix(ShapeProblem),
    /// "Point 2 selected. Drag to move it, or delete it."
    Selected(PointIndex),
    /// "Drag a point to adjust it. Tap + on an edge to add a point. Tap a point to select it."
    Adjust,
}

/// The Draw area screen.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per control, as the shell's DrawAreaState binds them"
)]
pub struct DrawArea {
    /// The Spot or zone being drawn.
    pub spot_name: Name,
    /// The points, in order.
    pub points: Vec<Coordinate>,
    /// Where a "+" sits on each edge of a closed shape: the one at `i` inserts after point `i`.
    pub midpoints: Vec<Coordinate>,
    /// Whether the shape is closed.
    pub closed: bool,
    /// The selected point.
    pub selected: Option<PointIndex>,
    /// Whether the first point can close the shape (shown larger and orange).
    pub closable: bool,
    /// Whether Undo is enabled.
    pub can_undo: bool,
    /// Whether "Delete point" is enabled: a point is selected and more than three remain.
    pub can_delete: bool,
    /// The area of a usable closed shape.
    pub area: Option<SquareMeters>,
    /// Why a closed shape cannot be used.
    pub problem: Option<ShapeProblem>,
    /// Whether "Use this area" is enabled.
    pub can_use: bool,
    /// What to do next.
    pub hint: DrawHint,
}

impl Default for PolygonEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl PolygonEditor {
    /// An empty editor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            points: Vec::new(),
            closure: Closure::Open,
            selected: None,
            history: Vec::new(),
            drag_start: None,
        }
    }

    /// An editor holding an existing area, closed, with nothing to undo.
    #[must_use]
    pub fn editing(polygon: &Polygon) -> Self {
        Self {
            points: polygon.vertices().to_vec(),
            closure: Closure::Closed,
            ..Self::new()
        }
    }

    /// The points, in order.
    #[must_use]
    pub fn points(&self) -> &[Coordinate] {
        &self.points
    }

    fn closed(&self) -> bool {
        matches!(self.closure, Closure::Closed)
    }

    fn closable(&self) -> bool {
        !self.closed() && self.points.len() >= 3
    }

    /// The history with the current shape added, keeping the last [`HISTORY`] steps.
    fn remembered(&self) -> Vec<Step> {
        let mut history = self.history.clone();
        history.push(Step {
            points: self.points.clone(),
            closure: self.closure,
        });
        let excess = history.len().saturating_sub(HISTORY);
        history.drain(..excess);
        history
    }

    /// A tap on the map, away from any point: adds a point to an open shape, or clears the
    /// selection of a closed one.
    #[must_use]
    pub fn tap_map(mut self, at: Coordinate) -> Self {
        if !self.closed() {
            self.history = self.remembered();
            self.points.push(at);
        }
        self.selected = None;
        self
    }

    /// A tap on a point: the first point closes an open shape of three or more; on a closed
    /// shape it selects the point, or clears the selection if it was selected.
    #[must_use]
    pub fn tap_point(mut self, index: PointIndex) -> Self {
        if self.closable() && index.get() == 0 {
            self.history = self.remembered();
            self.closure = Closure::Closed;
            self.selected = None;
        } else if self.closed() && index.get() < self.points.len() {
            self.selected = if self.selected == Some(index) {
                None
            } else {
                Some(index)
            };
        } else {
            self.selected = None;
        }
        self
    }

    /// Moves a point while it is dragged. The whole drag undoes as one step; call
    /// [`PolygonEditor::drag_end`] when the finger lifts.
    #[must_use]
    pub fn drag(mut self, index: PointIndex, to: Coordinate) -> Self {
        let remembered = self.remembered();
        let Some(point) = self.points.get_mut(index.get()) else {
            return self;
        };
        *point = to;
        if self.drag_start.is_none() {
            self.drag_start = Some(remembered);
        }
        self
    }

    /// Ends a drag, making it one Undo step.
    #[must_use]
    pub fn drag_end(mut self) -> Self {
        if let Some(history) = self.drag_start.take() {
            self.history = history;
        }
        self
    }

    /// Inserts a point halfway along the edge after `after` of a closed shape, and selects it.
    #[must_use]
    pub fn insert_midpoint(mut self, after: PointIndex) -> Self {
        if !self.closed() {
            return self;
        }
        let Some(start) = self.points.get(after.get()).copied() else {
            return self;
        };
        let next = after.get().saturating_add(1);
        let Some(end) = self.points.get(next).or(self.points.first()).copied() else {
            return self;
        };
        self.history = self.remembered();
        self.points.insert(next, midpoint(start, end));
        self.selected = Some(PointIndex(next));
        self
    }

    /// Deletes the selected point, if more than three remain.
    #[must_use]
    pub fn delete_selected(mut self) -> Self {
        let Some(selected) = self.selected else {
            return self;
        };
        if self.points.len() <= 3 || selected.get() >= self.points.len() {
            return self;
        }
        self.history = self.remembered();
        self.points.remove(selected.get());
        self.selected = None;
        self
    }

    /// Goes back one step.
    #[must_use]
    pub fn undo(mut self) -> Self {
        if let Some(step) = self.history.pop() {
            self.points = step.points;
            self.closure = step.closure;
            self.selected = None;
        }
        self
    }

    /// Removes every point. Undo brings them back.
    #[must_use]
    pub fn clear(mut self) -> Self {
        self.history = self.remembered();
        self.points.clear();
        self.closure = Closure::Open;
        self.selected = None;
        self
    }

    /// The shape as a polygon, for "Use this area".
    ///
    /// # Errors
    ///
    /// [`ScreenError::ShapeOpen`] while the shape is open; [`ScreenError::Shape`] if pingers-geo
    /// rejects it.
    pub fn polygon(&self) -> Result<Polygon, ScreenError> {
        if !self.closed() {
            return Err(ScreenError::ShapeOpen);
        }
        Ok(Polygon::new(self.points.clone())?)
    }

    /// The edges' midpoints of a closed shape; none while it is open.
    fn midpoints(&self) -> Vec<Coordinate> {
        if !self.closed() {
            return Vec::new();
        }
        self.points
            .iter()
            .zip(self.points.iter().cycle().skip(1))
            .map(|(start, end)| midpoint(*start, *end))
            .collect()
    }

    /// What the screen shows.
    #[must_use]
    pub fn view(&self, spot_name: &Name) -> DrawArea {
        let checked = self.closed().then(|| self.polygon());
        let area = match &checked {
            Some(Ok(polygon)) => Some(polygon.area()),
            Some(Err(_)) | None => None,
        };
        let problem = match checked {
            Some(Err(ScreenError::Shape(error))) => Some(ShapeProblem::of(error)),
            Some(Err(_)) => Some(ShapeProblem::Crossing),
            Some(Ok(_)) | None => None,
        };
        let hint = if self.points.is_empty() {
            DrawHint::FirstPoint
        } else if !self.closed() && self.points.len() < 3 {
            DrawHint::KeepTapping
        } else if !self.closed() {
            DrawHint::CloseShape
        } else if let Some(problem) = problem {
            DrawHint::Fix(problem)
        } else if let Some(selected) = self.selected {
            DrawHint::Selected(selected)
        } else {
            DrawHint::Adjust
        };
        DrawArea {
            spot_name: spot_name.clone(),
            points: self.points.clone(),
            midpoints: self.midpoints(),
            closed: self.closed(),
            selected: self.selected,
            closable: self.closable(),
            can_undo: !self.history.is_empty(),
            can_delete: self.closed() && self.selected.is_some() && self.points.len() > 3,
            area,
            problem,
            can_use: area.is_some(),
            hint,
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    fn origin() -> Result<Coordinate, GeoError> {
        fixtures::sam_position()
    }

    /// A point `north` and `east` metres from the origin.
    fn point(north: f64, east: f64) -> Result<Coordinate, GeoError> {
        fixtures::offset(origin()?, north, east)
    }

    fn name() -> Result<Name, ScreenError> {
        Ok(Name::new("Riverside Park")?)
    }

    /// An open shape of the given points.
    fn drawn(points: &[(f64, f64)]) -> Result<PolygonEditor, GeoError> {
        let mut editor = PolygonEditor::new();
        for (north, east) in points {
            editor = editor.tap_map(point(*north, *east)?);
        }
        Ok(editor)
    }

    /// A closed 100 m square: south-west, south-east, north-east, north-west.
    fn square() -> Result<PolygonEditor, GeoError> {
        Ok(
            drawn(&[(0.0, 0.0), (0.0, 100.0), (100.0, 100.0), (100.0, 0.0)])?
                .tap_point(PointIndex::new(0)),
        )
    }

    #[test]
    fn an_empty_editor_asks_for_the_first_point() -> Result<(), FixtureError> {
        let view = PolygonEditor::new().view(&name()?);
        assert_eq!(view.hint, DrawHint::FirstPoint);
        assert!(!view.closed && !view.closable && !view.can_undo && !view.can_use);
        assert_eq!(view.spot_name, name()?);
        assert_eq!(PolygonEditor::default(), PolygonEditor::new());
        Ok(())
    }

    #[test]
    fn taps_add_points_until_the_shape_can_close() -> Result<(), FixtureError> {
        let two = drawn(&[(0.0, 0.0), (0.0, 100.0)])?;
        let view = two.view(&name()?);
        assert_eq!(
            (view.points.len(), view.hint, view.closable),
            (2, DrawHint::KeepTapping, false)
        );
        assert!(view.can_undo && view.midpoints.is_empty() && view.area.is_none());
        let three = two.tap_map(point(100.0, 100.0)?);
        let view = three.view(&name()?);
        assert_eq!(
            (view.hint, view.closable, view.closed),
            (DrawHint::CloseShape, true, false)
        );
        Ok(())
    }

    #[test]
    fn tapping_the_first_point_closes_the_shape() -> Result<(), FixtureError> {
        let view = square()?.view(&name()?);
        assert!(view.closed && !view.closable && view.can_use);
        assert_eq!(view.hint, DrawHint::Adjust);
        let area = view.area.ok_or(FixtureError::Missing)?;
        assert!((area.get() - 10_000.0).abs() < 100.0, "{}", area.get());
        assert_eq!(view.problem, None);
        assert_eq!(square()?.polygon()?.vertices().len(), 4);
        Ok(())
    }

    #[test]
    fn only_the_first_point_closes_and_only_with_three() -> Result<(), FixtureError> {
        let three = drawn(&[(0.0, 0.0), (0.0, 100.0), (100.0, 100.0)])?;
        assert!(
            !three
                .clone()
                .tap_point(PointIndex::new(1))
                .view(&name()?)
                .closed
        );
        let two = drawn(&[(0.0, 0.0), (0.0, 100.0)])?;
        assert!(!two.tap_point(PointIndex::new(0)).view(&name()?).closed);
        assert_eq!(three.polygon(), Err(ScreenError::ShapeOpen));
        Ok(())
    }

    #[test]
    fn taps_on_the_map_do_not_add_to_a_closed_shape() -> Result<(), FixtureError> {
        let selected = square()?.tap_point(PointIndex::new(2));
        let tapped = selected.tap_map(point(500.0, 500.0)?);
        let view = tapped.view(&name()?);
        assert_eq!((view.points.len(), view.selected), (4, None));
        Ok(())
    }

    #[test]
    fn tapping_a_point_of_a_closed_shape_toggles_its_selection() -> Result<(), FixtureError> {
        let selected = square()?.tap_point(PointIndex::new(1));
        let view = selected.view(&name()?);
        assert_eq!(view.selected, Some(PointIndex::new(1)));
        assert_eq!(view.hint, DrawHint::Selected(PointIndex::new(1)));
        let other = selected.clone().tap_point(PointIndex::new(3));
        assert_eq!(other.view(&name()?).selected, Some(PointIndex::new(3)));
        let cleared = selected.clone().tap_point(PointIndex::new(1));
        assert_eq!(cleared.view(&name()?).selected, None);
        let missing = selected.tap_point(PointIndex::new(4));
        assert_eq!(missing.view(&name()?).selected, None);
        Ok(())
    }

    #[test]
    fn a_drag_moves_a_point_and_undoes_in_one_step() -> Result<(), FixtureError> {
        let closed = square()?;
        let dragging = closed
            .clone()
            .drag(PointIndex::new(2), point(120.0, 110.0)?)
            .drag(PointIndex::new(2), point(150.0, 150.0)?);
        let view = dragging.view(&name()?);
        assert_eq!(view.points.get(2).copied(), Some(point(150.0, 150.0)?));
        let lifted = dragging.drag_end();
        let undone = lifted.clone().undo();
        assert_eq!(undone.points(), closed.points());
        assert!(undone.view(&name()?).closed);
        // A second drag starts its own step.
        let again = lifted
            .drag(PointIndex::new(0), point(-10.0, -10.0)?)
            .drag_end()
            .undo();
        assert_eq!(again.points().get(2).copied(), Some(point(150.0, 150.0)?));
        assert_eq!(again.points().first().copied(), Some(point(0.0, 0.0)?));
        Ok(())
    }

    #[test]
    fn dragging_a_missing_point_changes_nothing() -> Result<(), FixtureError> {
        let closed = square()?;
        let dragged = closed
            .clone()
            .drag(PointIndex::new(9), point(1.0, 1.0)?)
            .drag_end();
        assert_eq!(dragged, closed);
        assert_eq!(closed.clone().drag_end(), closed);
        Ok(())
    }

    #[test]
    fn plus_inserts_the_midpoint_and_selects_it() -> Result<(), FixtureError> {
        let closed = square()?;
        let view = closed.view(&name()?);
        assert_eq!(view.midpoints.len(), 4);
        // The last edge runs back to the first point.
        let last = view
            .midpoints
            .get(3)
            .copied()
            .ok_or(FixtureError::Missing)?;
        assert!((last.latitude() - point(50.0, 0.0)?.latitude()).abs() < 1e-9);
        assert!((last.longitude() - origin()?.longitude()).abs() < 1e-9);

        let inserted = closed.clone().insert_midpoint(PointIndex::new(0));
        let view = inserted.view(&name()?);
        assert_eq!(view.points.len(), 5);
        assert_eq!(view.selected, Some(PointIndex::new(1)));
        let added = view.points.get(1).copied().ok_or(FixtureError::Missing)?;
        assert!((added.longitude() - point(0.0, 50.0)?.longitude()).abs() < 1e-9);

        let wrapped = closed.clone().insert_midpoint(PointIndex::new(3));
        assert_eq!(wrapped.view(&name()?).selected, Some(PointIndex::new(4)));
        assert_eq!(wrapped.points().len(), 5);
        assert_eq!(closed.clone().insert_midpoint(PointIndex::new(4)), closed);
        assert_eq!(inserted.undo().points(), closed.points());
        Ok(())
    }

    #[test]
    fn plus_needs_a_closed_shape() -> Result<(), FixtureError> {
        let open = drawn(&[(0.0, 0.0), (0.0, 100.0), (100.0, 100.0)])?;
        assert_eq!(open.clone().insert_midpoint(PointIndex::new(0)), open);
        Ok(())
    }

    #[test]
    fn delete_only_above_three_points() -> Result<(), FixtureError> {
        let selected = square()?.tap_point(PointIndex::new(1));
        assert!(selected.view(&name()?).can_delete);
        let deleted = selected.delete_selected();
        let view = deleted.view(&name()?);
        assert_eq!((view.points.len(), view.selected), (3, None));
        assert!(!view.can_delete);
        let three = deleted.tap_point(PointIndex::new(0));
        assert!(!three.view(&name()?).can_delete);
        assert_eq!(three.clone().delete_selected(), three);
        let nothing_selected = square()?;
        assert_eq!(nothing_selected.clone().delete_selected(), nothing_selected);
        Ok(())
    }

    #[test]
    fn undo_steps_back_and_clear_can_be_undone() -> Result<(), FixtureError> {
        let closed = square()?;
        let reopened = closed.clone().undo();
        let view = reopened.view(&name()?);
        assert!(!view.closed && view.points.len() == 4);
        let cleared = closed.clone().clear();
        let view = cleared.view(&name()?);
        assert!(view.points.is_empty() && !view.closed && view.can_undo);
        assert_eq!(view.hint, DrawHint::FirstPoint);
        assert_eq!(cleared.undo().points(), closed.points());
        let empty = PolygonEditor::new();
        assert_eq!(empty.clone().undo(), empty);
        Ok(())
    }

    #[test]
    fn undo_clears_the_selection() -> Result<(), FixtureError> {
        let inserted = square()?.insert_midpoint(PointIndex::new(1));
        assert!(inserted.view(&name()?).selected.is_some());
        assert_eq!(inserted.undo().view(&name()?).selected, None);
        Ok(())
    }

    #[test]
    fn history_keeps_the_last_fifty_steps() -> Result<(), FixtureError> {
        let mut editor = PolygonEditor::new();
        for step in 0..60_u32 {
            editor = editor.tap_map(point(f64::from(step), 0.0)?);
        }
        for _ in 0..HISTORY {
            editor = editor.undo();
        }
        assert_eq!(editor.points().len(), 10);
        assert!(!editor.view(&name()?).can_undo);
        Ok(())
    }

    #[test]
    fn a_crossing_shape_closes_but_cannot_be_used() -> Result<(), FixtureError> {
        // A bow tie: the second and fourth edges cross.
        let bow_tie = drawn(&[(0.0, 0.0), (0.0, 100.0), (100.0, 0.0), (100.0, 100.0)])?
            .tap_point(PointIndex::new(0));
        let view = bow_tie.view(&name()?);
        assert!(view.closed && !view.can_use);
        assert_eq!(view.problem, Some(ShapeProblem::Crossing));
        assert_eq!(view.hint, DrawHint::Fix(ShapeProblem::Crossing));
        assert_eq!(view.area, None);
        assert_eq!(
            bow_tie.polygon(),
            Err(ScreenError::Shape(GeoError::SelfIntersecting))
        );
        // Moving a point untangles it.
        let fixed = bow_tie
            .drag(PointIndex::new(2), point(100.0, 100.0)?)
            .drag(PointIndex::new(3), point(100.0, 0.0)?)
            .drag_end();
        assert!(fixed.view(&name()?).can_use);
        Ok(())
    }

    #[test]
    fn a_flat_shape_has_no_area() -> Result<(), FixtureError> {
        let line = drawn(&[(0.0, 0.0), (0.0, 100.0), (0.0, 200.0)])?.tap_point(PointIndex::new(0));
        assert_eq!(line.view(&name()?).problem, Some(ShapeProblem::NoArea));
        Ok(())
    }

    #[test]
    fn a_huge_shape_is_too_large() -> Result<(), FixtureError> {
        let huge =
            drawn(&[(0.0, 0.0), (0.0, 150_000.0), (150_000.0, 0.0)])?.tap_point(PointIndex::new(0));
        assert_eq!(huge.view(&name()?).problem, Some(ShapeProblem::TooLarge));
        Ok(())
    }

    #[test]
    fn editing_an_existing_area_starts_closed() -> Result<(), FixtureError> {
        let polygon = fixtures::square(origin()?, 80.0)?;
        let editor = PolygonEditor::editing(&polygon);
        let view = editor.view(&name()?);
        assert!(view.closed && view.can_use && !view.can_undo);
        assert_eq!(view.points.as_slice(), polygon.vertices());
        Ok(())
    }

    #[test]
    fn problems_name_what_geo_rejected() {
        assert_eq!(
            ShapeProblem::of(GeoError::PolygonTooLarge),
            ShapeProblem::TooLarge
        );
        assert_eq!(ShapeProblem::of(GeoError::Degenerate), ShapeProblem::NoArea);
        assert_eq!(
            ShapeProblem::of(GeoError::TooFewVertices),
            ShapeProblem::NoArea
        );
        assert_eq!(
            ShapeProblem::of(GeoError::SelfIntersecting),
            ShapeProblem::Crossing
        );
    }
}
