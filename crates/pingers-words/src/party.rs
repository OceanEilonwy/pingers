// SPDX-License-Identifier: AGPL-3.0-only
//! New party, Advanced, and a Party's screen, in words.

use pingers_screens::common::{PartyColour, PrivacyLevel};
use pingers_screens::party::{
    Advanced, DraftPrivacy, NewParty, Overlap, PartyView, Reachability, SpotShape,
};

use crate::Language;
use crate::common::{area, colour, distance, privacy_level};

/// "New party": the screen's title and Home's button.
pub const NEW_PARTY: &str = "New party";

/// A colour swatch: its value and spoken name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swatch {
    /// The colour.
    pub colour: PartyColour,
    /// "Blue".
    pub name: String,
}

/// How private a draft is, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyText {
    /// "Full privacy", "Reduced privacy", "Checking".
    pub level: String,
    /// What it means.
    pub detail: String,
}

/// A service field's check, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckText {
    /// "Server reachable", "Checking server", "Server not reachable".
    pub label: String,
    /// Whether to show the spinner.
    pub checking: bool,
}

/// The New party screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPartyText {
    /// "New party".
    pub title: String,
    /// "Party name".
    pub name_label: String,
    /// "Colour".
    pub colour_label: String,
    /// The swatches.
    pub swatches: Vec<Swatch>,
    /// "pingers.flat7.org · Full privacy".
    pub server_row: String,
    /// "You'll invite people and add Spots once the party is created."
    pub later: String,
    /// "Create party".
    pub create: String,
}

/// The Advanced screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvancedText {
    /// "Advanced".
    pub title: String,
    /// The privacy banner.
    pub privacy: PrivacyText,
    /// "Server".
    pub server_label: String,
    /// The server check.
    pub server_check: CheckText,
    /// Help under the server field.
    pub server_help: String,
    /// "Privacy relay".
    pub relay_label: String,
    /// The relay check.
    pub relay_check: CheckText,
    /// Help under the relay field.
    pub relay_help: String,
    /// "Push relay".
    pub push_label: String,
    /// "Pingers (official app)".
    pub push_official: String,
    /// "Custom URL (UnifiedPush or own build)".
    pub push_custom: String,
    /// Help under the push relay choice.
    pub push_help: String,
    /// "Post-quantum encryption".
    pub post_quantum: String,
    /// Its help line.
    pub post_quantum_help: String,
    /// "Done".
    pub done: String,
}

/// The privacy banner's level and detail.
#[must_use]
pub fn draft_privacy(language: Language, privacy: &DraftPrivacy) -> PrivacyText {
    let Language::English = language;
    match privacy {
        DraftPrivacy::Checking => PrivacyText {
            level: "Checking".into(),
            detail: "Checking who runs the server and the relays.".into(),
        },
        DraftPrivacy::Known {
            level: PrivacyLevel::Full,
            ..
        } => PrivacyText {
            level: privacy_level(language, PrivacyLevel::Full),
            detail: "Server, privacy relay and push relay are run by three different operators."
                .into(),
        },
        DraftPrivacy::Known {
            level: PrivacyLevel::Reduced,
            overlaps,
        } => PrivacyText {
            level: privacy_level(language, PrivacyLevel::Reduced),
            detail: overlaps
                .iter()
                .map(|overlap| overlap_sentence(language, *overlap))
                .collect::<Vec<_>>()
                .join(" "),
        },
    }
}

/// One shared operator, as a sentence.
#[must_use]
pub fn overlap_sentence(language: Language, overlap: Overlap) -> String {
    let Language::English = language;
    match overlap {
        Overlap::ServerRunsRelay => {
            "The server's operator also runs the privacy relay, so it could see your internet \
             address."
        }
        Overlap::ServerRunsPushRelay => {
            "The server's operator also runs the push relay, so it could see which phone each \
             alert wakes."
        }
        Overlap::RelayRunsPushRelay => {
            "One operator runs both relays, so it could link your internet address to when your \
             phone is woken."
        }
    }
    .into()
}

fn check(language: Language, what: &str, reachability: &Reachability) -> CheckText {
    let Language::English = language;
    match reachability {
        Reachability::Checking => CheckText {
            label: format!("Checking {}", what.to_lowercase()),
            checking: true,
        },
        Reachability::Reachable(_) => CheckText {
            label: format!("{what} reachable"),
            checking: false,
        },
        Reachability::Unreachable => CheckText {
            label: format!("{what} not reachable"),
            checking: false,
        },
    }
}

/// The New party screen, in words.
#[must_use]
pub fn new_party(language: Language, view: &NewParty) -> NewPartyText {
    let Language::English = language;
    let privacy = draft_privacy(language, &view.privacy);
    NewPartyText {
        title: NEW_PARTY.into(),
        name_label: "Party name".into(),
        colour_label: "Colour".into(),
        swatches: PartyColour::ALL
            .iter()
            .map(|swatch| Swatch {
                colour: *swatch,
                name: colour(language, *swatch),
            })
            .collect(),
        server_row: format!("{} · {}", view.server.as_str(), privacy.level),
        later: "You'll invite people and add Spots once the party is created.".into(),
        create: "Create party".into(),
    }
}

/// The Advanced screen, in words.
#[must_use]
pub fn advanced(language: Language, view: &Advanced) -> AdvancedText {
    let Language::English = language;
    AdvancedText {
        title: "Advanced".into(),
        privacy: draft_privacy(language, &view.privacy),
        server_label: "Server".into(),
        server_check: check(language, "Server", &view.server.check),
        server_help: "The server that hosts this party. Everyone who joins uses it, and it can't \
                      be changed later."
            .into(),
        relay_label: "Privacy relay".into(),
        relay_check: check(language, "Privacy relay", &view.relay.check),
        relay_help: "Hides your IP address from the server. Suggested by the server; each member \
                     can choose their own."
            .into(),
        push_label: "Push relay".into(),
        push_official: "Pingers (official app)".into(),
        push_custom: "Custom URL (UnifiedPush or own build)".into(),
        push_help: "Sees when a phone is woken, never why or by whom.".into(),
        post_quantum: "Post-quantum encryption".into(),
        post_quantum_help: "Protects against future quantum computers. Larger messages; every \
                            member needs a recent app."
            .into(),
        done: crate::common::done(language),
    }
}

/// A Spot's shape on its row: "300 m circle", "Drawn area · 0.4 ha".
#[must_use]
pub fn spot_shape(language: Language, shape: SpotShape) -> String {
    let Language::English = language;
    match shape {
        SpotShape::Circle(radius) => format!("{} circle", distance(language, radius)),
        SpotShape::Area(size) => format!("Drawn area · {}", area(language, size)),
    }
}

/// A Spot row, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotRowText {
    /// "Riverside Park".
    pub name: String,
    /// "300 m circle".
    pub shape: String,
    /// "You're in a meetup", if so.
    pub in_meetup: Option<String>,
}

/// A Party's screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyText {
    /// "Climbing crew".
    pub title: String,
    /// "Spots".
    pub spots_heading: String,
    /// "Spots visible only to members".
    pub spots_private: String,
    /// The Spots.
    pub spots: Vec<SpotRowText>,
    /// "Add spot".
    pub add_spot: String,
    /// "You only see meetups you were part of. …"
    pub meetups_note: String,
    /// "pingers.flat7.org · Full privacy".
    pub server_row: String,
    /// "What this party's server can see".
    pub server_subtitle: String,
    /// "Invite people", the button's spoken label.
    pub invite: String,
}

/// A Party's screen, in words.
#[must_use]
pub fn party(language: Language, view: &PartyView) -> PartyText {
    let Language::English = language;
    PartyText {
        title: view.name.as_str().to_owned(),
        spots_heading: "Spots".into(),
        spots_private: "Spots visible only to members".into(),
        spots: view
            .spots
            .iter()
            .map(|row| SpotRowText {
                name: row.name.as_str().to_owned(),
                shape: spot_shape(language, row.shape),
                in_meetup: row.in_meetup.then(|| "You're in a meetup".to_owned()),
            })
            .collect(),
        add_spot: "Add spot".into(),
        meetups_note: "You only see meetups you were part of. Members who weren't there won't \
                       know one happened."
            .into(),
        server_row: format!(
            "{} · {}",
            view.server.as_str(),
            privacy_level(language, view.privacy)
        ),
        server_subtitle: crate::server::party_subtitle(language),
        invite: crate::invite::TITLE.into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_geo::{Meters, SquareMeters};
    use pingers_screens::common::{DraftText, Operator};
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::party::{
        PartyDraft, PushRelayChoice, ServiceDraft, advanced as advanced_screen,
        new_party as new_party_screen, party as party_screen,
    };
    use pingers_screens::snapshot::{PostQuantum, WakePushes};

    use super::*;

    const EN: Language = Language::English;

    fn draft() -> Result<PartyDraft, FixtureError> {
        Ok(PartyDraft {
            name: DraftText::new("Climbing crew".into()),
            colour: PartyColour::Blue,
            server: ServiceDraft {
                host: DraftText::new("pingers.flat7.org".into()),
                check: Reachability::Reachable(Operator::new("Flat 7")?),
            },
            relay: ServiceDraft {
                host: DraftText::new("relay.quietpath.net".into()),
                check: Reachability::Checking,
            },
            push_relay: PushRelayChoice::Official,
            push_relay_operator: Some(Operator::new("Pingers")?),
            post_quantum: PostQuantum::Off,
        })
    }

    #[test]
    fn new_party_reads_like_the_canvas() -> Result<(), FixtureError> {
        let mut ready = draft()?;
        ready.relay.check = Reachability::Reachable(Operator::new("Quietpath")?);
        let text = new_party(EN, &new_party_screen(&ready, WakePushes::On));
        assert_eq!(text.title, "New party");
        assert_eq!(text.server_row, "pingers.flat7.org · Full privacy");
        assert_eq!(text.swatches.len(), 5);
        assert_eq!(
            text.swatches.get(1).map(|swatch| swatch.name.as_str()),
            Some("Orange")
        );
        assert_eq!(
            (text.name_label.as_str(), text.colour_label.as_str()),
            ("Party name", "Colour")
        );
        assert_eq!(text.create, "Create party");
        assert!(text.later.starts_with("You'll invite people"));
        Ok(())
    }

    #[test]
    fn advanced_shows_checks_and_privacy() -> Result<(), FixtureError> {
        let text = advanced(EN, &advanced_screen(&draft()?, WakePushes::On));
        assert_eq!(
            text.server_check,
            CheckText {
                label: "Server reachable".into(),
                checking: false
            }
        );
        assert_eq!(
            text.relay_check,
            CheckText {
                label: "Checking privacy relay".into(),
                checking: true
            }
        );
        assert_eq!(text.privacy.level, "Checking");
        let mut down = draft()?;
        down.relay.check = Reachability::Unreachable;
        let text = advanced(EN, &advanced_screen(&down, WakePushes::On));
        assert_eq!(text.relay_check.label, "Privacy relay not reachable");
        assert_eq!(text.title, "Advanced");
        assert_eq!(text.done, "Done");
        assert_eq!(
            text.push_help,
            "Sees when a phone is woken, never why or by whom."
        );
        Ok(())
    }

    #[test]
    fn privacy_names_each_shared_operator() {
        let full = draft_privacy(
            EN,
            &DraftPrivacy::Known {
                level: PrivacyLevel::Full,
                overlaps: Vec::new(),
            },
        );
        assert_eq!(full.level, "Full privacy");
        assert_eq!(
            full.detail,
            "Server, privacy relay and push relay are run by three different operators."
        );
        let reduced = draft_privacy(
            EN,
            &DraftPrivacy::Known {
                level: PrivacyLevel::Reduced,
                overlaps: vec![Overlap::ServerRunsRelay, Overlap::RelayRunsPushRelay],
            },
        );
        assert_eq!(reduced.level, "Reduced privacy");
        assert_eq!(
            reduced.detail,
            format!(
                "{} {}",
                overlap_sentence(EN, Overlap::ServerRunsRelay),
                overlap_sentence(EN, Overlap::RelayRunsPushRelay)
            )
        );
        assert!(
            overlap_sentence(EN, Overlap::ServerRunsPushRelay)
                .contains("which phone each alert wakes")
        );
        assert!(overlap_sentence(EN, Overlap::ServerRunsRelay).contains("internet address"));
    }

    #[test]
    fn party_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = party(EN, &party_screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(text.title, "Climbing crew");
        let rows: Vec<_> = text
            .spots
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.shape.as_str(),
                    row.in_meetup.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Riverside Park", "300 m circle", Some("You're in a meetup")),
                ("The Wall climbing gym", "Drawn area · 0.4 ha", None),
                ("Ana's place", "100 m circle", None),
            ]
        );
        assert_eq!(text.server_row, "pingers.flat7.org · Full privacy");
        assert_eq!(text.server_subtitle, "What this party's server can see");
        assert_eq!(text.invite, "Invite people");
        assert_eq!(
            (text.spots_heading.as_str(), text.add_spot.as_str()),
            ("Spots", "Add spot")
        );
        assert_eq!(text.spots_private, "Spots visible only to members");
        assert!(text.meetups_note.starts_with("You only see meetups"));
        Ok(())
    }

    #[test]
    fn spot_shapes_read_as_rows() -> Result<(), pingers_geo::GeoError> {
        assert_eq!(
            spot_shape(EN, SpotShape::Circle(Meters::from_whole(60))),
            "60 m circle"
        );
        assert_eq!(
            spot_shape(EN, SpotShape::Area(SquareMeters::new(500.0)?)),
            "Drawn area · 500 m²"
        );
        Ok(())
    }
}
