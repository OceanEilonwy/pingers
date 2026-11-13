// SPDX-License-Identifier: AGPL-3.0-only
//! The compass, in words.

use pingers_screens::compass::{CompassTarget, CompassView};

use crate::Language;
use crate::common::{direction, distance, list};
use crate::meetup::meet_point_name;

/// A target chip, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetText {
    /// "the bandstand", "Ben".
    pub label: String,
    /// "Chloé: location private, ask to see", for a locked member.
    pub locked: Option<String>,
}

/// The compass screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompassText {
    /// The chips.
    pub targets: Vec<TargetText>,
    /// "the bandstand", the selected target.
    pub target: String,
    /// "85 m", when known.
    pub distance: Option<String>,
    /// "About 1 min walk · north-east", when known.
    pub walk: Option<String>,
    /// "Arrow pointing north-east, 42 degrees, toward the bandstand", the arrow's spoken label.
    pub arrow_label: Option<String>,
    /// "GPS ±6 m", when a fix is held.
    pub gps: Option<String>,
    /// "N".
    pub north: String,
    /// "Phone taps when you face the right way."
    pub taps: String,
    /// "Ben sees you're heading over.", when anyone does.
    pub watchers: Option<String>,
    /// "Close compass".
    pub close: String,
}

fn target(language: Language, target: &CompassTarget) -> TargetText {
    let Language::English = language;
    match target {
        CompassTarget::MeetPoint(name) => TargetText {
            label: meet_point_name(language, name),
            locked: None,
        },
        CompassTarget::Member { person, locked } => {
            let name = person.name.as_str();
            TargetText {
                label: name.to_owned(),
                locked: locked.then(|| format!("{name}: location private, ask to see")),
            }
        }
    }
}

/// The compass screen, in words.
#[must_use]
pub fn compass(language: Language, view: &CompassView) -> CompassText {
    let Language::English = language;
    let selected = target(language, &view.target);
    let watchers: Vec<String> = view
        .watchers
        .iter()
        .map(|name| name.as_str().to_owned())
        .collect();
    CompassText {
        targets: view
            .targets
            .iter()
            .map(|chip| target(language, chip))
            .collect(),
        distance: view
            .reading
            .map(|reading| distance(language, reading.distance)),
        walk: view.reading.map(|reading| {
            format!(
                "About {} min walk · {}",
                reading.walk.get(),
                direction(language, reading.direction)
            )
        }),
        arrow_label: view.reading.map(|reading| {
            format!(
                "Arrow pointing {}, {:.0} degrees, toward {}",
                direction(language, reading.direction),
                reading.bearing.degrees(),
                selected.label
            )
        }),
        target: selected.label,
        gps: view
            .gps_accuracy
            .map(|accuracy| format!("GPS ±{}", distance(language, accuracy))),
        north: "N".into(),
        taps: "Phone taps when you face the right way.".into(),
        watchers: match watchers.len() {
            0 => None,
            1 => Some(format!(
                "{} sees you're heading over.",
                list(language, &watchers)
            )),
            _ => Some(format!(
                "{} see you're heading over.",
                list(language, &watchers)
            )),
        },
        close: "Close compass".into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::compass::{CompassSelection, compass as screen};
    use pingers_screens::fixtures::{self, FixtureError};

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn the_compass_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = compass(
            EN,
            &screen(
                &snapshot,
                fixtures::meetup_id(),
                CompassSelection::MeetPoint,
            )?,
        );
        let chips: Vec<_> = text
            .targets
            .iter()
            .map(|chip| (chip.label.as_str(), chip.locked.as_deref()))
            .collect();
        assert_eq!(
            chips,
            [
                ("the bandstand", None),
                ("Ben", None),
                ("Chloé", Some("Chloé: location private, ask to see"))
            ]
        );
        assert_eq!(text.target, "the bandstand");
        assert_eq!(text.distance.as_deref(), Some("85 m"));
        assert_eq!(text.walk.as_deref(), Some("About 1 min walk · north-east"));
        assert_eq!(
            text.arrow_label.as_deref(),
            Some("Arrow pointing north-east, 42 degrees, toward the bandstand")
        );
        assert_eq!(text.gps.as_deref(), Some("GPS ±6 m"));
        assert_eq!(
            text.watchers.as_deref(),
            Some("Ben sees you're heading over.")
        );
        assert_eq!(
            (text.north.as_str(), text.close.as_str()),
            ("N", "Close compass")
        );
        assert_eq!(text.taps, "Phone taps when you face the right way.");
        Ok(())
    }

    #[test]
    fn watchers_and_unknowns() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.own_position = None;
        for meetup in &mut snapshot.meetups {
            meetup.sharing_with = vec![fixtures::ben(), fixtures::chloe()];
        }
        let text = compass(
            EN,
            &screen(
                &snapshot,
                fixtures::meetup_id(),
                CompassSelection::MeetPoint,
            )?,
        );
        assert_eq!(
            text.watchers.as_deref(),
            Some("Ben and Chloé see you're heading over.")
        );
        assert_eq!(
            (text.distance, text.walk, text.arrow_label, text.gps),
            (None, None, None, None)
        );
        for meetup in &mut snapshot.meetups {
            meetup.sharing_with.clear();
        }
        let text = compass(
            EN,
            &screen(
                &snapshot,
                fixtures::meetup_id(),
                CompassSelection::MeetPoint,
            )?,
        );
        assert_eq!(text.watchers, None);
        Ok(())
    }
}
