// SPDX-License-Identifier: AGPL-3.0-only
//! The Spot editor: a name, and a circle or a drawn area.

use pingers_geo::{MINIMUM_SPOT_RADIUS, Meters, Polygon, SquareMeters};
use pingers_proto::PartyId;

use crate::ScreenError;
use crate::common::{DraftText, Name};
use crate::snapshot::ClientSnapshot;

/// The largest radius the slider offers: 1 km. Larger places are better drawn as an area, and a
/// huge circle would match people who are nowhere near each other.
#[must_use]
pub fn maximum_spot_radius() -> Meters {
    Meters::from_whole(1000)
}

/// Circle or drawn area.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeKind {
    /// A circle around a point.
    Circle,
    /// A polygon drawn point by point.
    Area,
}

/// The Spot being edited, as far as the user has got.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotDraft {
    /// The name typed.
    pub name: DraftText,
    /// Which shape is chosen.
    pub shape: ShapeKind,
    /// The circle's radius from the slider (kept when switching to an area and back).
    pub radius: Meters,
    /// The drawn area, once Draw area has returned one.
    pub area: Option<Polygon>,
}

/// The Spot editor screen.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotEditor {
    /// The Party the Spot is encrypted for.
    pub party_name: Name,
    /// The name typed; empty shows the "New spot" placeholder.
    pub name: DraftText,
    /// Which shape is chosen.
    pub shape: ShapeKind,
    /// The radius, within the slider's range.
    pub radius: Meters,
    /// The slider's lower end.
    pub min_radius: Meters,
    /// The slider's upper end.
    pub max_radius: Meters,
    /// The drawn area's size, once drawn.
    pub area: Option<SquareMeters>,
    /// Whether "Save spot" is enabled: a name, and an area if Area is chosen.
    pub can_save: bool,
}

/// The Spot editor.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn spot_editor(
    snapshot: &ClientSnapshot,
    party: PartyId,
    draft: &SpotDraft,
) -> Result<SpotEditor, ScreenError> {
    let party = snapshot.party(party)?;
    let maximum = maximum_spot_radius();
    let radius = draft.radius.max(MINIMUM_SPOT_RADIUS);
    let radius = if radius > maximum { maximum } else { radius };
    let shape_ready = match draft.shape {
        ShapeKind::Circle => true,
        ShapeKind::Area => draft.area.is_some(),
    };
    Ok(SpotEditor {
        party_name: party.name.clone(),
        name: draft.name.clone(),
        shape: draft.shape,
        radius,
        min_radius: MINIMUM_SPOT_RADIUS,
        max_radius: maximum,
        area: draft.area.as_ref().map(Polygon::area),
        can_save: shape_ready && draft.name.name().is_some(),
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    fn draft(shape: ShapeKind, radius: u32, area: Option<Polygon>) -> SpotDraft {
        SpotDraft {
            name: DraftText::new("Riverside Park".into()),
            shape,
            radius: Meters::from_whole(radius),
            area,
        }
    }

    #[test]
    fn a_circle_spot_is_ready_with_a_name() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = spot_editor(
            &snapshot,
            fixtures::climbing_crew(),
            &draft(ShapeKind::Circle, 300, None),
        )?;
        assert_eq!(view.party_name.as_str(), "Climbing crew");
        assert_eq!(view.radius, Meters::from_whole(300));
        assert_eq!(
            (view.min_radius, view.max_radius),
            (MINIMUM_SPOT_RADIUS, maximum_spot_radius())
        );
        assert_eq!(view.area, None);
        assert!(view.can_save);
        let mut unnamed = draft(ShapeKind::Circle, 300, None);
        unnamed.name = DraftText::default();
        assert!(!spot_editor(&snapshot, fixtures::climbing_crew(), &unnamed)?.can_save);
        Ok(())
    }

    #[test]
    fn the_radius_stays_on_the_slider() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let small = spot_editor(
            &snapshot,
            fixtures::climbing_crew(),
            &draft(ShapeKind::Circle, 40, None),
        )?;
        assert_eq!(small.radius, MINIMUM_SPOT_RADIUS);
        let large = spot_editor(
            &snapshot,
            fixtures::climbing_crew(),
            &draft(ShapeKind::Circle, 5000, None),
        )?;
        assert_eq!(large.radius, maximum_spot_radius());
        Ok(())
    }

    #[test]
    fn an_area_spot_needs_a_drawn_area() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let undrawn = spot_editor(
            &snapshot,
            fixtures::climbing_crew(),
            &draft(ShapeKind::Area, 300, None),
        )?;
        assert!(!undrawn.can_save);
        let square = fixtures::square(fixtures::sam_position()?, 100.0)?;
        let drawn = spot_editor(
            &snapshot,
            fixtures::climbing_crew(),
            &draft(ShapeKind::Area, 300, Some(square)),
        )?;
        assert!(drawn.can_save);
        let area = drawn.area.ok_or(FixtureError::Missing)?;
        assert!((area.get() - 10_000.0).abs() < 100.0);
        assert_eq!(drawn.shape, ShapeKind::Area);
        Ok(())
    }
}
