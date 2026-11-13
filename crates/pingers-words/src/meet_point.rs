// SPDX-License-Identifier: AGPL-3.0-only
//! The meet point editor, in words.

use pingers_screens::meet_point::{MeetPointMode, MeetPointView};

use crate::Language;
use crate::common::distance;

/// A mode choice, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeText {
    /// The mode.
    pub mode: MeetPointMode,
    /// "Pin on map".
    pub label: String,
}

/// The meet point editor, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetPointText {
    /// "Meet point".
    pub title: String,
    /// "Drag map to move pin", in Pin mode.
    pub drag_hint: Option<String>,
    /// The three modes.
    pub modes: Vec<ModeText>,
    /// "Label (optional)".
    pub label_placeholder: String,
    /// "Ben 120 m", "Chloé: private".
    pub distances: Vec<String>,
    /// "Share meet point".
    pub share: String,
}

/// The meet point editor, in words.
#[must_use]
pub fn meet_point(language: Language, view: &MeetPointView) -> MeetPointText {
    let Language::English = language;
    MeetPointText {
        title: "Meet point".into(),
        drag_hint: matches!(view.mode, MeetPointMode::Pin).then(|| "Drag map to move pin".into()),
        modes: vec![
            ModeText {
                mode: MeetPointMode::Pin,
                label: "Pin on map".into(),
            },
            ModeText {
                mode: MeetPointMode::AtMe,
                label: "At me".into(),
            },
            ModeText {
                mode: MeetPointMode::Midpoint,
                label: "Midpoint".into(),
            },
        ],
        label_placeholder: "Label (optional)".into(),
        distances: view
            .distances
            .iter()
            .map(|row| match row.distance {
                Some(far) => format!("{} {}", row.person.name.as_str(), distance(language, far)),
                None => format!("{}: private", row.person.name.as_str()),
            })
            .collect(),
        share: "Share meet point".into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::common::DraftText;
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::meet_point::{MeetPointDraft, meet_point as screen};

    use super::*;

    const EN: Language = Language::English;

    fn text(mode: MeetPointMode) -> Result<MeetPointText, FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let draft = MeetPointDraft {
            mode,
            label: DraftText::default(),
            pin: fixtures::sam_position()?,
        };
        Ok(meet_point(
            EN,
            &screen(&snapshot, fixtures::meetup_id(), &draft)?,
        ))
    }

    #[test]
    fn the_editor_reads_like_the_canvas() -> Result<(), FixtureError> {
        let pin = text(MeetPointMode::Pin)?;
        assert_eq!(pin.title, "Meet point");
        assert_eq!(pin.drag_hint.as_deref(), Some("Drag map to move pin"));
        let labels: Vec<_> = pin.modes.iter().map(|mode| mode.label.as_str()).collect();
        assert_eq!(labels, ["Pin on map", "At me", "Midpoint"]);
        assert_eq!(pin.distances, ["Ben 120 m", "Chloé: private"]);
        assert_eq!(pin.label_placeholder, "Label (optional)");
        assert_eq!(pin.share, "Share meet point");
        assert_eq!(text(MeetPointMode::Midpoint)?.drag_hint, None);
        Ok(())
    }
}
