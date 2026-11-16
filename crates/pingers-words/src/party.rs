// SPDX-License-Identifier: AGPL-3.0-only
//! New party, Advanced, and a Party's screen, in words.

use pingers_screens::common::{PartyColour, PrivacyLevel};
use pingers_screens::party::{
    Advanced, CodeAnswer, DraftPrivacy, NewParty, Overlap, PartyCode, PartyView, Reachability,
    ServerKeys, SpotShape,
};

use crate::Language;
use crate::common::{area, colour, distance, privacy_level};

/// "New party": the screen's title and Home's button.
pub const NEW_PARTY: &str = "New party";

/// "The app's server".
const BUILT_IN: &str = "The app's server";

/// "Another server".
const ANOTHER: &str = "Another server";

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
    /// "Server".
    pub server_label: String,
    /// "The app's server".
    pub built_in: String,
    /// "server.pingers.org · built in".
    pub built_in_detail: String,
    /// "Another server".
    pub another: String,
    /// "Enter its address, like pingers.example.org".
    pub another_detail: String,
    /// "pingers.example.org", the address field's placeholder.
    pub another_placeholder: String,
    /// Another server's keys, once looked up: what to compare and the button to trust them.
    pub keys: Option<ServerKeysText>,
    /// "Advanced".
    pub advanced: String,
    /// "Relays and encryption · Full privacy".
    pub advanced_detail: String,
    /// "You'll invite people and add Spots once the party is created."
    pub later: String,
    /// "Create party".
    pub create: String,
}

/// Another server's keys on New party, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerKeysText {
    /// "Its key: 1a2b 3c4d … Check it with whoever runs the server.", or "Keys trusted".
    pub line: String,
    /// "Trust this server's keys", while they wait for the user.
    pub trust: Option<String>,
}

/// Another server's keys, in words; `None` before they are looked up.
#[must_use]
pub fn server_keys(language: Language, keys: &ServerKeys) -> Option<ServerKeysText> {
    let Language::English = language;
    match keys {
        ServerKeys::NotLookedUp => None,
        ServerKeys::ToTrust { fingerprint } => Some(ServerKeysText {
            line: format!(
                "Its key: {fingerprint}. If you can, check it with whoever runs the server."
            ),
            trust: Some("Trust this server's keys".into()),
        }),
        ServerKeys::Trusted => Some(ServerKeysText {
            line: "Keys trusted. Pingers will refuse this server if they change.".into(),
            trust: None,
        }),
    }
}

/// The Advanced screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvancedText {
    /// "Advanced".
    pub title: String,
    /// The privacy banner.
    pub privacy: PrivacyText,
    /// "Which server", for screen readers.
    pub server_choice_label: String,
    /// "The app's server", "Another server".
    pub server_choices: [String; 2],
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
        server_label: "Server".into(),
        built_in: BUILT_IN.into(),
        built_in_detail: format!("{} · built in", view.built_in.as_str()),
        another: ANOTHER.into(),
        another_detail: "Enter its address, like pingers.example.org".into(),
        another_placeholder: "pingers.example.org".into(),
        keys: view
            .server_keys
            .as_ref()
            .and_then(|keys| server_keys(language, keys)),
        advanced: "Advanced".into(),
        advanced_detail: format!("Relays and encryption · {}", privacy.level),
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
        server_choice_label: "Which server".into(),
        server_choices: [BUILT_IN.into(), ANOTHER.into()],
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

/// "This server needs a code", in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyCodeText {
    /// "Back to New party", for screen readers.
    pub back: String,
    /// "This server needs a code".
    pub title: String,
    /// "You chose pingers.flat7.org for Climbing crew. …"
    pub body: String,
    /// "pingers.flat7.org".
    pub server: String,
    /// "Run by Flat 7".
    pub operator: String,
    /// "Registration code".
    pub code_label: String,
    /// What the server does with the code, and what the account is.
    pub code_help: String,
    /// Why the code was refused, once it was.
    pub refused: Option<String>,
    /// "No Party was created. …", once refused.
    pub nothing_created: Option<String>,
    /// "People you invite won't need a code: their invite link carries it."
    pub invitees: String,
    /// "Create party", or "Try again" once refused.
    pub create: String,
}

/// "This server needs a code", in words.
#[must_use]
pub fn party_code(language: Language, view: &PartyCode) -> PartyCodeText {
    let Language::English = language;
    let operator = view
        .operator
        .as_ref()
        .map(|operator| operator.as_str().to_owned());
    let refused = view.answer == CodeAnswer::Refused;
    PartyCodeText {
        back: "Back to New party".into(),
        title: "This server needs a code".into(),
        body: format!(
            "You chose {} for {}. It only lets people with a code from whoever runs it create \
             Parties there.",
            view.server.as_str(),
            view.party_name.as_str()
        ),
        server: view.server.as_str().to_owned(),
        operator: operator.as_ref().map_or_else(
            || "Who runs it isn't published yet".to_owned(),
            |operator| format!("Run by {operator}"),
        ),
        code_label: "Registration code".into(),
        code_help: "This server sees the code once, when it sets up your account there. The \
                    account is a random key made on this phone: no name, phone number or email."
            .into(),
        refused: refused.then(|| match &operator {
            Some(operator) => format!(
                "The server didn't accept this code. Check it for typing mistakes, or ask \
                 {operator} for a new one."
            ),
            None => "The server didn't accept this code. Check it for typing mistakes, or ask \
                     whoever runs it for a new one."
                .to_owned(),
        }),
        nothing_created: refused.then(|| {
            "No Party was created. You can try again, or go back and pick another server.".into()
        }),
        invitees: "People you invite won't need a code: their invite link carries it.".into(),
        create: if refused { "Try again" } else { "Create party" }.into(),
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
    /// "Party settings: server, invites, leave".
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
        server_subtitle: "Party settings: server, invites, leave".into(),
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
        CodeAnswer, PartyDraft, PushRelayChoice, ServerChoice, ServerKeys, ServiceDraft,
        advanced as advanced_screen, new_party as new_party_screen, party as party_screen,
        party_code as party_code_screen,
    };
    use pingers_screens::snapshot::{PostQuantum, WakePushes};

    use super::*;

    const EN: Language = Language::English;

    fn draft() -> Result<PartyDraft, FixtureError> {
        Ok(PartyDraft {
            name: DraftText::new("Climbing crew".into()),
            colour: PartyColour::Blue,
            server_choice: ServerChoice::BuiltIn,
            built_in: pingers_screens::common::Host::new("server.pingers.org")?,
            server: ServiceDraft {
                host: DraftText::new("pingers.flat7.org".into()),
                check: Reachability::Reachable(Operator::new("Flat 7")?),
            },
            server_keys: ServerKeys::NotLookedUp,
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
    fn another_server_s_keys_are_shown_then_trusted() -> Result<(), FixtureError> {
        let mut another = draft()?;
        another.server_choice = ServerChoice::Another;
        assert_eq!(
            new_party(EN, &new_party_screen(&another, WakePushes::On)).keys,
            None
        );
        another.server_keys = ServerKeys::ToTrust {
            fingerprint: "1a2b 3c4d".into(),
        };
        let shown = new_party(EN, &new_party_screen(&another, WakePushes::On))
            .keys
            .ok_or(FixtureError::Missing)?;
        assert!(shown.line.contains("1a2b 3c4d"));
        assert_eq!(shown.trust.as_deref(), Some("Trust this server's keys"));
        another.server_keys = ServerKeys::Trusted;
        let trusted = new_party(EN, &new_party_screen(&another, WakePushes::On))
            .keys
            .ok_or(FixtureError::Missing)?;
        assert!(trusted.line.starts_with("Keys trusted"));
        assert_eq!(trusted.trust, None);
        // The app's server has no step.
        another.server_choice = ServerChoice::BuiltIn;
        assert_eq!(
            new_party(EN, &new_party_screen(&another, WakePushes::On)).keys,
            None
        );
        Ok(())
    }

    #[test]
    fn new_party_reads_like_the_canvas() -> Result<(), FixtureError> {
        let mut ready = draft()?;
        ready.relay.check = Reachability::Reachable(Operator::new("Quietpath")?);
        let text = new_party(EN, &new_party_screen(&ready, WakePushes::On));
        assert_eq!(text.title, "New party");
        assert_eq!(text.server_label, "Server");
        assert_eq!(text.built_in, "The app's server");
        assert_eq!(text.built_in_detail, "server.pingers.org · built in");
        assert_eq!(text.another, "Another server");
        assert_eq!(
            text.another_detail,
            "Enter its address, like pingers.example.org"
        );
        assert_eq!(text.advanced_detail, "Relays and encryption · Full privacy");
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
        assert_eq!(
            text.server_subtitle,
            "Party settings: server, invites, leave"
        );
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

    #[test]
    fn the_code_screen_reads_like_the_canvas() -> Result<(), FixtureError> {
        let mut chosen = draft()?;
        chosen.server_choice = ServerChoice::Another;
        let typed = DraftText::new("spring-otter-4471".into());
        let text = party_code(
            EN,
            &party_code_screen(&chosen, &typed, CodeAnswer::NotTried)?,
        );
        assert_eq!(text.title, "This server needs a code");
        assert_eq!(
            text.body,
            "You chose pingers.flat7.org for Climbing crew. It only lets people with a code from \
             whoever runs it create Parties there."
        );
        assert_eq!(text.operator, "Run by Flat 7");
        assert_eq!(text.code_label, "Registration code");
        assert_eq!((text.refused, text.nothing_created), (None, None));
        assert_eq!(text.create, "Create party");
        let refused = party_code(
            EN,
            &party_code_screen(&chosen, &typed, CodeAnswer::Refused)?,
        );
        assert_eq!(
            refused.refused.as_deref(),
            Some(
                "The server didn't accept this code. Check it for typing mistakes, or ask Flat 7 \
                 for a new one."
            )
        );
        assert_eq!(
            refused.nothing_created.as_deref(),
            Some("No Party was created. You can try again, or go back and pick another server.")
        );
        assert_eq!(refused.create, "Try again");
        Ok(())
    }
}
