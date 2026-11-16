// SPDX-License-Identifier: AGPL-3.0-only
//! What your server sees, generated from the live configuration.
//!
//! [`facts`] decides which statements are true for a [`PrivacyConfiguration`]: what the server
//! can see, what it never sees, and notes about the relays. [`server_sees`] writes them. The
//! release default (three operators, wake-up pushes on, membership proofs, relay working) gives
//! exactly the canvas's two "can see" and five "never sees" lines (and the note about invite
//! links opened in a browser, which is always true); each departure from it
//! changes the lists as design A.2 says it changes what the server learns.

use pingers_screens::party::Overlap;
use pingers_screens::server::{PrivacyConfiguration, Proofs, ServerSees};
use pingers_screens::snapshot::{Hosting, RelayStatus, WakePushes};

use crate::Language;
use crate::common::privacy_level;

/// Something the server can see.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanSee {
    /// Anonymous check-ins, not whose or which Spot. Always.
    CheckIns,
    /// That a Meetup forms and its size, not who or where. Always.
    MeetupForming,
    /// Device IP addresses: its operator also runs the privacy relay.
    InternetAddress,
    /// Which phone each knock wakes: its operator also runs the push relay.
    PhonesWoken,
    /// Each day's batch of Privacy Pass tokens: the fallback is active.
    TokenBatches,
}

/// Something the server never sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NeverSees {
    /// Where anyone or any Spot is. Always.
    Locations,
    /// Who met whom, or which phone a knock woke.
    WhoMetWhom,
    /// Who met whom; with no wake-up pushes, no phone is woken at all.
    WhoMetWhomNothingWoken,
    /// Party names and rosters. Always.
    Parties,
    /// Chats, meet points, shared locations. Always.
    Content,
    /// Pauses, Focus, exclusion zones. Always.
    Pauses,
}

/// Something about the relays the user should know alongside the lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Note {
    /// One third party runs both relays.
    RelaysShareOperator,
    /// No privacy relay reachable: anonymous traffic waits.
    RelayUnreachable,
    /// A relay key does not match its pin: anonymous traffic stopped.
    RelayKeyMismatch,
    /// An invite link opened in a browser loads this server's landing page, whose script a
    /// dishonest server could change to read the invite. Always.
    BrowserLinks,
}

/// Which statements are true for a configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// "It can see".
    pub can_see: Vec<CanSee>,
    /// "It never sees".
    pub never_sees: Vec<NeverSees>,
    /// Notes under the lists.
    pub notes: Vec<Note>,
}

/// Which statements are true for a configuration.
#[must_use]
pub fn facts(configuration: &PrivacyConfiguration) -> Facts {
    let overlaps = &configuration.overlaps;
    let mut can_see = vec![CanSee::CheckIns, CanSee::MeetupForming];
    if overlaps.contains(&Overlap::ServerRunsRelay) {
        can_see.push(CanSee::InternetAddress);
    }
    // Overlaps involving the push relay are only reported while wake-up pushes are on.
    let woken = overlaps.contains(&Overlap::ServerRunsPushRelay);
    if woken {
        can_see.push(CanSee::PhonesWoken);
    }
    if matches!(configuration.proofs, Proofs::PrivacyPassOnly) {
        can_see.push(CanSee::TokenBatches);
    }
    let mut never_sees = vec![NeverSees::Locations];
    match (woken, configuration.wake_pushes) {
        (true, _) => {}
        (false, WakePushes::On) => never_sees.push(NeverSees::WhoMetWhom),
        (false, WakePushes::NoWakePushes) => never_sees.push(NeverSees::WhoMetWhomNothingWoken),
    }
    never_sees.extend([NeverSees::Parties, NeverSees::Content, NeverSees::Pauses]);
    let mut notes = Vec::new();
    if overlaps.contains(&Overlap::RelayRunsPushRelay) {
        notes.push(Note::RelaysShareOperator);
    }
    match configuration.relay {
        RelayStatus::Reachable => {}
        RelayStatus::Unreachable => notes.push(Note::RelayUnreachable),
        RelayStatus::KeyMismatch => notes.push(Note::RelayKeyMismatch),
    }
    notes.push(Note::BrowserLinks);
    Facts {
        can_see,
        never_sees,
        notes,
    }
}

/// A "can see" line.
#[must_use]
pub fn can_see(language: Language, fact: CanSee) -> String {
    let Language::English = language;
    match fact {
        CanSee::CheckIns => {
            "Anonymous check-ins every few minutes, without knowing whose phone or which Spot"
        }
        CanSee::MeetupForming => {
            "That a meetup is starting somewhere, and how many people are in it, but not who, or \
             which Party or Spot"
        }
        CanSee::InternetAddress => {
            "Your internet address, because the same operator runs your privacy relay"
        }
        CanSee::PhonesWoken => {
            "Which phone each alert wakes, because the same operator runs your push relay. That \
             is enough to work out who met whom, but still not where"
        }
        CanSee::TokenBatches => {
            "When your account collects its daily batch of anonymous passes, which this phone \
             uses while membership proofs aren't available"
        }
    }
    .into()
}

/// A "never sees" line.
#[must_use]
pub fn never_sees(language: Language, fact: NeverSees) -> String {
    let Language::English = language;
    match fact {
        NeverSees::Locations => "Where you are, or where any Spot is",
        NeverSees::WhoMetWhom => "Who met whom, or which phone it woke",
        NeverSees::WhoMetWhomNothingWoken => {
            "Who met whom. With no wake-up pushes, no phone is woken at all"
        }
        NeverSees::Parties => "Party names, or who's in them",
        NeverSees::Content => "What's inside your chats, meet points and shared locations",
        NeverSees::Pauses => "Whether you're paused, in Focus, or in an exclusion zone",
    }
    .into()
}

/// A note under the lists.
#[must_use]
pub fn note(language: Language, note: Note) -> String {
    let Language::English = language;
    match note {
        Note::RelaysShareOperator => {
            "Your privacy relay and push relay are run by the same operator. Together they could \
             link your internet address to when your phone is woken."
        }
        Note::RelayUnreachable => {
            "No privacy relay is reachable. Check-ins wait until one is; nothing anonymous is \
             sent without it."
        }
        Note::RelayKeyMismatch => {
            "The privacy relay's key doesn't match the one your party agreed on. Pingers has \
             stopped anonymous traffic until it does."
        }
        Note::BrowserLinks => {
            "An invite link opened in a browser loads a page from this server, and a dishonest \
             server could change that page to read the invite. Open invite links in Pingers."
        }
    }
    .into()
}

/// The What your server sees screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSeesText {
    /// "What your server sees".
    pub title: String,
    /// The server's host.
    pub server: String,
    /// "Self-hosted", for a self-hosted server.
    pub hosting: Option<String>,
    /// "Full privacy", "Reduced privacy".
    pub privacy: String,
    /// "It can see".
    pub can_see_heading: String,
    /// The lines under it.
    pub can_see: Vec<String>,
    /// "It never sees".
    pub never_sees_heading: String,
    /// The lines under it.
    pub never_sees: Vec<String>,
    /// Notes about the relays.
    pub notes: Vec<String>,
    /// The link to Security details.
    pub security_link: String,
}

/// The What your server sees screen, in words.
#[must_use]
pub fn server_sees(language: Language, view: &ServerSees) -> ServerSeesText {
    let Language::English = language;
    let facts = facts(&view.configuration);
    ServerSeesText {
        title: "What your server sees".into(),
        server: view.server.as_str().to_owned(),
        hosting: match view.hosting {
            Hosting::SelfHosted => Some("Self-hosted".into()),
            Hosting::Public => None,
        },
        privacy: privacy_level(language, view.privacy),
        can_see_heading: "It can see".into(),
        can_see: facts
            .can_see
            .into_iter()
            .map(|fact| can_see(language, fact))
            .collect(),
        never_sees_heading: "It never sees".into(),
        never_sees: facts
            .never_sees
            .into_iter()
            .map(|fact| never_sees(language, fact))
            .collect(),
        notes: facts
            .notes
            .into_iter()
            .map(|fact| note(language, fact))
            .collect(),
        security_link: crate::security::TITLE.into(),
    }
}

/// "What this party's server can see", under the server row on a Party's screen.
#[must_use]
pub fn party_subtitle(language: Language) -> String {
    let Language::English = language;
    "What this party's server can see".into()
}

/// What a reachability check found: whether this phone reached its server through the privacy
/// relay. It names no host and no address, only the outcome.
#[must_use]
pub fn relay_check(language: Language, status: RelayStatus) -> String {
    let Language::English = language;
    match status {
        RelayStatus::Reachable => "Your server answered through the privacy relay.".into(),
        RelayStatus::Unreachable => "Your server didn't answer through the privacy relay. \
                                     Check-ins wait until it does; nothing anonymous is sent \
                                     without it."
            .into(),
        // The same words as the note "What your server sees" shows for it.
        RelayStatus::KeyMismatch => note(language, Note::RelayKeyMismatch),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_client::Fallback;
    use pingers_screens::common::Operator;
    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::server::{PushRelaySource, server_sees as screen};
    use pingers_screens::snapshot::PostQuantum;

    use super::*;

    const EN: Language = Language::English;

    fn configuration(overlaps: Vec<Overlap>) -> PrivacyConfiguration {
        PrivacyConfiguration {
            overlaps,
            wake_pushes: WakePushes::On,
            proofs: Proofs::MembershipProofs,
            relay: RelayStatus::Reachable,
            push_relay: PushRelaySource::Official,
            post_quantum: PostQuantum::Off,
        }
    }

    #[test]
    fn the_release_default_reads_exactly_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = server_sees(EN, &screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(text.title, "What your server sees");
        assert_eq!(text.server, "pingers.flat7.org");
        assert_eq!(text.hosting.as_deref(), Some("Self-hosted"));
        assert_eq!(text.privacy, "Full privacy");
        assert_eq!(
            (
                text.can_see_heading.as_str(),
                text.never_sees_heading.as_str()
            ),
            ("It can see", "It never sees")
        );
        assert_eq!(
            text.can_see,
            [
                "Anonymous check-ins every few minutes, without knowing whose phone or which Spot",
                "That a meetup is starting somewhere, and how many people are in it, but not who, or which Party or Spot",
            ]
        );
        assert_eq!(
            text.never_sees,
            [
                "Where you are, or where any Spot is",
                "Who met whom, or which phone it woke",
                "Party names, or who's in them",
                "What's inside your chats, meet points and shared locations",
                "Whether you're paused, in Focus, or in an exclusion zone",
            ]
        );
        assert_eq!(text.notes, [note(EN, Note::BrowserLinks)]);
        assert_eq!(text.security_link, "How Pingers keeps you private");
        Ok(())
    }

    #[test]
    fn a_server_that_runs_the_push_relay_sees_phones_woken() {
        let facts = facts(&configuration(vec![Overlap::ServerRunsPushRelay]));
        assert_eq!(
            facts.can_see,
            [CanSee::CheckIns, CanSee::MeetupForming, CanSee::PhonesWoken]
        );
        assert!(!facts.never_sees.contains(&NeverSees::WhoMetWhom));
        assert!(
            !facts
                .never_sees
                .contains(&NeverSees::WhoMetWhomNothingWoken)
        );
        assert!(
            facts.never_sees.contains(&NeverSees::Locations),
            "still not where"
        );
    }

    #[test]
    fn the_same_operator_fallback_reaches_the_list() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.fallbacks = vec![Fallback::SameOperator];
        let text = server_sees(EN, &screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(text.privacy, "Reduced privacy");
        assert_eq!(
            text.can_see.last().map(String::as_str),
            Some(can_see(EN, CanSee::PhonesWoken).as_str())
        );
        assert_eq!(text.never_sees.len(), 4);
        Ok(())
    }

    #[test]
    fn a_server_that_runs_the_privacy_relay_sees_addresses() {
        let facts = facts(&configuration(vec![Overlap::ServerRunsRelay]));
        assert_eq!(
            facts.can_see,
            [
                CanSee::CheckIns,
                CanSee::MeetupForming,
                CanSee::InternetAddress
            ]
        );
        assert!(facts.never_sees.contains(&NeverSees::WhoMetWhom));
    }

    #[test]
    fn one_operator_running_both_relays_is_a_note() {
        let facts = facts(&configuration(vec![Overlap::RelayRunsPushRelay]));
        assert_eq!(facts.notes, [Note::RelaysShareOperator, Note::BrowserLinks]);
        assert_eq!(facts.can_see, [CanSee::CheckIns, CanSee::MeetupForming]);
    }

    #[test]
    fn no_wake_up_pushes_says_nothing_is_woken() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        // Even when the server's operator runs the push relay: no token is registered.
        snapshot.fallbacks = vec![Fallback::SameOperator];
        let text = server_sees(EN, &screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(text.privacy, "Full privacy");
        assert_eq!(text.can_see.len(), 2);
        assert_eq!(
            text.never_sees.get(1).map(String::as_str),
            Some("Who met whom. With no wake-up pushes, no phone is woken at all")
        );
        Ok(())
    }

    #[test]
    fn the_privacy_pass_fallback_shows_token_batches() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.fallbacks = vec![Fallback::PrivacyPassOnly];
        let text = server_sees(EN, &screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(
            text.privacy, "Full privacy",
            "Privacy Pass changes nothing else (A.2)"
        );
        assert_eq!(
            text.can_see.last().map(String::as_str),
            Some(
                "When your account collects its daily batch of anonymous passes, which this phone uses while membership proofs aren't available"
            )
        );
        Ok(())
    }

    #[test]
    fn relay_trouble_is_a_note() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.relay = RelayStatus::Unreachable;
        let text = server_sees(EN, &screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(
            text.notes,
            [
                note(EN, Note::RelayUnreachable),
                note(EN, Note::BrowserLinks)
            ]
        );
        snapshot.relay = RelayStatus::KeyMismatch;
        let text = server_sees(EN, &screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(
            text.notes,
            [
                note(EN, Note::RelayKeyMismatch),
                note(EN, Note::BrowserLinks)
            ]
        );
        Ok(())
    }

    #[test]
    fn a_reachability_check_says_only_how_it_went() {
        assert_eq!(
            relay_check(EN, RelayStatus::Reachable),
            "Your server answered through the privacy relay."
        );
        assert!(
            relay_check(EN, RelayStatus::Unreachable)
                .starts_with("Your server didn't answer through the privacy relay.")
        );
        assert_eq!(
            relay_check(EN, RelayStatus::KeyMismatch),
            note(EN, Note::RelayKeyMismatch)
        );
    }

    #[test]
    fn everything_at_once() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.config.hosting = Hosting::Public;
            party.config.relay.operator = Some(Operator::new("Flat 7")?);
            party.config.push_relay.operator = Some(Operator::new("Flat 7")?);
        }
        snapshot.fallbacks = vec![Fallback::PrivacyPassOnly];
        let view = screen(&snapshot, fixtures::climbing_crew())?;
        let facts = facts(&view.configuration);
        assert_eq!(
            facts.can_see,
            [
                CanSee::CheckIns,
                CanSee::MeetupForming,
                CanSee::InternetAddress,
                CanSee::PhonesWoken,
                CanSee::TokenBatches
            ]
        );
        assert_eq!(
            facts.never_sees,
            [
                NeverSees::Locations,
                NeverSees::Parties,
                NeverSees::Content,
                NeverSees::Pauses
            ]
        );
        // The server's operator runs both relays: the can-see lines already say so.
        assert_eq!(facts.notes, [Note::BrowserLinks]);
        let text = server_sees(EN, &view);
        assert_eq!(text.hosting, None);
        assert_eq!(text.can_see.len(), 5);
        Ok(())
    }

    #[test]
    fn every_line_has_words() {
        for fact in [
            CanSee::CheckIns,
            CanSee::MeetupForming,
            CanSee::InternetAddress,
            CanSee::PhonesWoken,
            CanSee::TokenBatches,
        ] {
            assert!(!can_see(EN, fact).is_empty());
        }
        assert_eq!(party_subtitle(EN), "What this party's server can see");
        assert!(
            note(EN, Note::RelaysShareOperator).starts_with("Your privacy relay and push relay")
        );
    }
}
