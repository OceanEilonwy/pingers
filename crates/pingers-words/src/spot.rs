// SPDX-License-Identifier: AGPL-3.0-only
//! The Spot editor and Draw area, in words.

use pingers_screens::draw::{DrawArea, DrawHint, PointIndex, ShapeProblem};
use pingers_screens::spot::{ShapeKind, SpotEditor};

use crate::Language;
use crate::common::{area, distance, plural};

/// The Spot editor, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotEditorText {
    /// "Riverside Park", or "New spot" while unnamed.
    pub title: String,
    /// "Search a place".
    pub search: String,
    /// "Spot name".
    pub name_label: String,
    /// "New spot", the name field's placeholder.
    pub name_placeholder: String,
    /// "Circle".
    pub circle: String,
    /// "Draw area".
    pub draw_area: String,
    /// "Radius", for a circle.
    pub radius_label: String,
    /// "300 m".
    pub radius: String,
    /// "Radius 300 m, drag to resize", the handle's spoken label.
    pub radius_handle: String,
    /// "Drawn area", for an area.
    pub area_label: String,
    /// "0.4 ha" once drawn.
    pub area: Option<String>,
    /// "Edit area".
    pub edit_area: String,
    /// "Encrypted for Climbing crew. …"
    pub encrypted: String,
    /// "Save spot".
    pub save: String,
}

/// The Spot editor, in words.
#[must_use]
pub fn spot_editor(language: Language, view: &SpotEditor) -> SpotEditorText {
    let Language::English = language;
    let radius = distance(language, view.radius);
    SpotEditorText {
        title: view
            .name
            .name()
            .map_or_else(|| "New spot".to_owned(), |name| name.as_str().to_owned()),
        search: "Search a place".into(),
        name_label: "Spot name".into(),
        name_placeholder: "New spot".into(),
        circle: "Circle".into(),
        draw_area: DRAW_AREA.into(),
        radius_label: "Radius".into(),
        radius_handle: format!("Radius {radius}, drag to resize"),
        radius,
        area_label: "Drawn area".into(),
        area: match view.shape {
            ShapeKind::Area => view.area.map(|size| area(language, size)),
            ShapeKind::Circle => None,
        },
        edit_area: "Edit area".into(),
        encrypted: format!(
            "Encrypted for {}. The server stores this Spot as unreadable data and never learns \
             where it is.",
            view.party_name.as_str()
        ),
        save: "Save spot".into(),
    }
}

const DRAW_AREA: &str = "Draw area";

/// The Draw area screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawAreaText {
    /// "Draw area · Riverside Park".
    pub title: String,
    /// "4 points".
    pub points: String,
    /// "Shape open", or "≈ 0.4 ha" once closed and usable.
    pub status: String,
    /// What to do next.
    pub hint: String,
    /// Each point's spoken label: "Point 1, tap to close the shape", "Point 2".
    pub point_labels: Vec<String>,
    /// Each "+"'s spoken label: "Add a point between 1 and 2".
    pub midpoint_labels: Vec<String>,
    /// "Undo".
    pub undo: String,
    /// "Delete point".
    pub delete: String,
    /// "Clear".
    pub clear: String,
    /// "Use this area", or "Close the shape to continue".
    pub use_area: String,
}

/// What a shape problem asks of the user.
#[must_use]
pub fn problem(language: Language, problem: ShapeProblem) -> String {
    let Language::English = language;
    match problem {
        ShapeProblem::Crossing => "Edges cross. Drag a point so the outline doesn't cross itself.",
        ShapeProblem::NoArea => "The points are in a line. Drag one to make an area.",
        ShapeProblem::TooLarge => "This area is too large for a Spot. Drag the points closer.",
    }
    .into()
}

fn number(index: PointIndex) -> usize {
    index.get().saturating_add(1)
}

/// The hint line.
#[must_use]
pub fn hint(language: Language, hint: DrawHint) -> String {
    let Language::English = language;
    match hint {
        DrawHint::FirstPoint => "Tap the map to drop the first point.".into(),
        DrawHint::KeepTapping => "Keep tapping to add points. You need at least 3.".into(),
        DrawHint::CloseShape => "Tap the orange first point to close the shape.".into(),
        DrawHint::Fix(found) => problem(language, found),
        DrawHint::Selected(index) => format!(
            "Point {} selected. Drag to move it, or delete it.",
            number(index)
        ),
        DrawHint::Adjust => "Drag a point to adjust it. Tap + on an edge to add a point. Tap a \
                             point to select it."
            .into(),
    }
}

/// The Draw area screen, in words.
#[must_use]
pub fn draw_area(language: Language, view: &DrawArea) -> DrawAreaText {
    let Language::English = language;
    let count = view.points.len();
    let status = match (view.closed, view.area) {
        (true, Some(size)) => format!("≈ {}", area(language, size)),
        (true, None) => "Shape needs fixing".into(),
        (false, _) => "Shape open".into(),
    };
    let point_labels = (0..count)
        .map(|index| {
            if index == 0 && view.closable {
                "Point 1, tap to close the shape".to_owned()
            } else {
                format!("Point {}", index.saturating_add(1))
            }
        })
        .collect();
    let midpoint_labels = (0..view.midpoints.len())
        .map(|index| {
            let next = if index.saturating_add(1) == count {
                1
            } else {
                index.saturating_add(2)
            };
            format!("Add a point between {} and {next}", index.saturating_add(1))
        })
        .collect();
    DrawAreaText {
        title: format!("{DRAW_AREA} · {}", view.spot_name.as_str()),
        points: plural(language, count, "point", "points"),
        status,
        hint: hint(language, view.hint),
        point_labels,
        midpoint_labels,
        undo: "Undo".into(),
        delete: "Delete point".into(),
        clear: "Clear".into(),
        use_area: if view.closed {
            "Use this area".into()
        } else {
            "Close the shape to continue".into()
        },
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_geo::Meters;
    use pingers_screens::common::{DraftText, Name};
    use pingers_screens::draw::PolygonEditor;
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::spot::{SpotDraft, spot_editor as editor_screen};

    use super::*;

    const EN: Language = Language::English;

    fn editor(name: &str, shape: ShapeKind, area: bool) -> Result<SpotEditorText, FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let draft = SpotDraft {
            name: DraftText::new(name.into()),
            shape,
            radius: Meters::from_whole(300),
            area: if area {
                Some(fixtures::square(fixtures::sam_position()?, 63.25)?)
            } else {
                None
            },
        };
        Ok(spot_editor(
            EN,
            &editor_screen(&snapshot, fixtures::climbing_crew(), &draft)?,
        ))
    }

    #[test]
    fn the_spot_editor_reads_like_the_canvas() -> Result<(), FixtureError> {
        let text = editor("Riverside Park", ShapeKind::Circle, false)?;
        assert_eq!(text.title, "Riverside Park");
        assert_eq!(text.radius, "300 m");
        assert_eq!(text.radius_handle, "Radius 300 m, drag to resize");
        assert_eq!(text.area, None);
        assert_eq!(
            text.encrypted,
            "Encrypted for Climbing crew. The server stores this Spot as unreadable data and never learns where it is."
        );
        assert_eq!(
            (
                text.circle.as_str(),
                text.draw_area.as_str(),
                text.save.as_str()
            ),
            ("Circle", "Draw area", "Save spot")
        );
        assert_eq!(
            (text.search.as_str(), text.name_label.as_str()),
            ("Search a place", "Spot name")
        );
        assert_eq!(
            (
                text.radius_label.as_str(),
                text.area_label.as_str(),
                text.edit_area.as_str()
            ),
            ("Radius", "Drawn area", "Edit area")
        );
        assert_eq!(text.name_placeholder, "New spot");
        Ok(())
    }

    #[test]
    fn an_unnamed_area_spot() -> Result<(), FixtureError> {
        let text = editor("  ", ShapeKind::Area, true)?;
        assert_eq!(text.title, "New spot");
        assert_eq!(text.area.as_deref(), Some("0.4 ha"));
        assert_eq!(
            editor("x", ShapeKind::Circle, true)?.area,
            None,
            "a circle shows no area"
        );
        Ok(())
    }

    fn name() -> Result<Name, FixtureError> {
        Ok(Name::new("Riverside Park")?)
    }

    #[test]
    fn draw_area_reads_like_the_canvas() -> Result<(), FixtureError> {
        let origin = fixtures::sam_position()?;
        let mut editor = PolygonEditor::new();
        let text = draw_area(EN, &editor.view(&name()?));
        assert_eq!(text.title, "Draw area · Riverside Park");
        assert_eq!(
            (text.points.as_str(), text.status.as_str()),
            ("0 points", "Shape open")
        );
        assert_eq!(text.hint, "Tap the map to drop the first point.");
        assert_eq!(text.use_area, "Close the shape to continue");
        editor = editor.tap_map(origin);
        let text = draw_area(EN, &editor.view(&name()?));
        assert_eq!(text.points, "1 point");
        assert_eq!(
            text.hint,
            "Keep tapping to add points. You need at least 3."
        );
        for (north, east) in [(0.0, 63.25), (63.25, 63.25), (63.25, 0.0)] {
            editor = editor.tap_map(fixtures::offset(origin, north, east)?);
        }
        let text = draw_area(EN, &editor.view(&name()?));
        assert_eq!(text.hint, "Tap the orange first point to close the shape.");
        assert_eq!(
            text.point_labels,
            [
                "Point 1, tap to close the shape",
                "Point 2",
                "Point 3",
                "Point 4"
            ]
        );
        let closed = editor.tap_point(PointIndex::new(0));
        let text = draw_area(EN, &closed.view(&name()?));
        assert_eq!(text.status, "≈ 0.4 ha");
        assert_eq!(text.points, "4 points");
        assert_eq!(
            text.hint,
            "Drag a point to adjust it. Tap + on an edge to add a point. Tap a point to select it."
        );
        assert_eq!(
            text.point_labels.first().map(String::as_str),
            Some("Point 1")
        );
        assert_eq!(
            text.midpoint_labels,
            [
                "Add a point between 1 and 2",
                "Add a point between 2 and 3",
                "Add a point between 3 and 4",
                "Add a point between 4 and 1"
            ]
        );
        assert_eq!(text.use_area, "Use this area");
        assert_eq!(
            (
                text.undo.as_str(),
                text.delete.as_str(),
                text.clear.as_str()
            ),
            ("Undo", "Delete point", "Clear")
        );
        let selected = closed.tap_point(PointIndex::new(1));
        assert_eq!(
            draw_area(EN, &selected.view(&name()?)).hint,
            "Point 2 selected. Drag to move it, or delete it."
        );
        Ok(())
    }

    #[test]
    fn a_crossing_shape_says_how_to_fix_it() -> Result<(), FixtureError> {
        let origin = fixtures::sam_position()?;
        let mut editor = PolygonEditor::new();
        for (north, east) in [(0.0, 0.0), (0.0, 100.0), (100.0, 0.0), (100.0, 100.0)] {
            editor = editor.tap_map(fixtures::offset(origin, north, east)?);
        }
        let text = draw_area(EN, &editor.tap_point(PointIndex::new(0)).view(&name()?));
        assert_eq!(text.status, "Shape needs fixing");
        assert_eq!(text.hint, problem(EN, ShapeProblem::Crossing));
        assert!(problem(EN, ShapeProblem::NoArea).contains("in a line"));
        assert!(problem(EN, ShapeProblem::TooLarge).contains("too large"));
        Ok(())
    }
}
