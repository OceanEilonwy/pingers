// SPDX-License-Identifier: AGPL-3.0-only
//! New party, its Advanced settings, and a Party's own screen. Also who runs what: the operator
//! overlaps that decide between full and reduced privacy.

use pingers_geo::{Fence, Meters, SquareMeters};
use pingers_proto::{GroupId, PartyId, SpotId};

use crate::ScreenError;
use crate::common::{DraftText, Host, Name, Operator, PartyColour, Person, PrivacyLevel};
use crate::snapshot::{ClientSnapshot, PartyConfig, PostQuantum, WakePushes};

/// Two services that declare the same operator (design A.2, B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Overlap {
    /// The server operator runs the privacy relay: it could see device IP addresses.
    ServerRunsRelay,
    /// The server operator runs the push relay: it could see which device each knock wakes.
    ServerRunsPushRelay,
    /// One third party runs both relays: it could tie IP addresses to wake times.
    RelayRunsPushRelay,
}

/// The operators of a Party's services, as far as they are known.
#[derive(Clone, Copy, Debug)]
pub struct Operators<'a> {
    /// The server's.
    pub server: Option<&'a Operator>,
    /// The privacy relay's.
    pub relay: Option<&'a Operator>,
    /// The push relay's.
    pub push_relay: Option<&'a Operator>,
}

impl<'a> Operators<'a> {
    /// The operators of a configured Party.
    #[must_use]
    pub const fn of(config: &'a PartyConfig) -> Self {
        Self {
            server: config.server.operator.as_ref(),
            relay: config.relay.operator.as_ref(),
            push_relay: config.push_relay.operator.as_ref(),
        }
    }
}

fn same(a: Option<&Operator>, b: Option<&Operator>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a == b)
}

/// Which services share an operator. The push relay counts only while wake-up pushes are on:
/// with none registered, it never hears of this device.
#[must_use]
pub fn overlaps(operators: Operators<'_>, wake_pushes: WakePushes) -> Vec<Overlap> {
    let pushes = matches!(wake_pushes, WakePushes::On);
    let mut found = Vec::new();
    if same(operators.server, operators.relay) {
        found.push(Overlap::ServerRunsRelay);
    }
    if pushes && same(operators.server, operators.push_relay) {
        found.push(Overlap::ServerRunsPushRelay);
    }
    // When the server's operator runs both relays, the two overlaps above already say it; the
    // relays sharing an operator adds nothing.
    let server_runs_both = found.len() == 2;
    if pushes && !server_runs_both && same(operators.relay, operators.push_relay) {
        found.push(Overlap::RelayRunsPushRelay);
    }
    found
}

/// Which services share an operator for a configured Party. A privacy relay that publishes
/// no operator (an OHTTP relay never does) is counted as the server's: the cautious reading,
/// so a Party is never called fully private on an operator nobody declared.
#[must_use]
pub fn party_overlaps(config: &PartyConfig, wake_pushes: WakePushes) -> Vec<Overlap> {
    let declared = Operators::of(config);
    overlaps(
        Operators {
            relay: declared.relay.or(declared.server),
            ..declared
        },
        wake_pushes,
    )
}

/// Full privacy when no two services share an operator.
#[must_use]
pub const fn privacy_level(overlaps: &[Overlap]) -> PrivacyLevel {
    if overlaps.is_empty() {
        PrivacyLevel::Full
    } else {
        PrivacyLevel::Reduced
    }
}

/// Whether a server or relay answered the check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reachability {
    /// Still checking.
    Checking,
    /// It answered, declaring this operator.
    Reachable(Operator),
    /// It did not answer, or the text is not a host.
    Unreachable,
}

impl Reachability {
    fn operator(&self) -> Option<&Operator> {
        match self {
            Self::Reachable(operator) => Some(operator),
            Self::Checking | Self::Unreachable => None,
        }
    }
}

/// A server or relay field on Advanced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceDraft {
    /// What is typed.
    pub host: DraftText,
    /// What the check found.
    pub check: Reachability,
}

/// The push relay choice on Advanced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushRelayChoice {
    /// "Pingers (official app)".
    Official,
    /// "Custom URL (UnifiedPush or own build)", with what is typed.
    Custom(DraftText),
}

/// Which server a new Party goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerChoice {
    /// "The app's server", built in.
    BuiltIn,
    /// "Another server", typed in.
    Another,
}

/// Another server's keys on New party: the explicit "Trust this server's keys" step. The
/// first time this device uses a server not built in, its keys are trusted only once the user
/// says so; nothing is pinned on first use.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ServerKeys {
    /// Not looked up yet (nothing typed, or still looking).
    NotLookedUp,
    /// Looked up: its gateway key, as a fingerprint to compare, waits for the user.
    ToTrust {
        /// The key's fingerprint ("1a2b 3c4d …").
        fingerprint: String,
    },
    /// Trusted, now or before (a server this device already uses).
    Trusted,
}

/// New party and Advanced, as filled in so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyDraft {
    /// The name typed.
    pub name: DraftText,
    /// The colour chosen.
    pub colour: PartyColour,
    /// Which server.
    pub server_choice: ServerChoice,
    /// The app's own server, as the build names it.
    pub built_in: Host,
    /// The server.
    pub server: ServiceDraft,
    /// Another server's keys; ignored for the app's server, which the build pins.
    pub server_keys: ServerKeys,
    /// The privacy relay.
    pub relay: ServiceDraft,
    /// The push relay.
    pub push_relay: PushRelayChoice,
    /// The operator the push relay declares, once its key config is fetched.
    pub push_relay_operator: Option<Operator>,
    /// The post-quantum switch.
    pub post_quantum: PostQuantum,
}

/// How private a draft is. Unknown while a check is still running, so the screen never claims
/// full privacy for operators it has not heard from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftPrivacy {
    /// Some operator is not known yet.
    Checking,
    /// Every operator is known.
    Known {
        /// Full or reduced.
        level: PrivacyLevel,
        /// Which services share an operator.
        overlaps: Vec<Overlap>,
    },
}

/// The New party screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewParty {
    /// The name typed.
    pub name: DraftText,
    /// The colour chosen.
    pub colour: PartyColour,
    /// Which server.
    pub server_choice: ServerChoice,
    /// The app's own server.
    pub built_in: Host,
    /// The server, as typed.
    pub server: DraftText,
    /// Another server's keys, while "Another server" is chosen.
    pub server_keys: Option<ServerKeys>,
    /// How private the setup is.
    pub privacy: DraftPrivacy,
    /// Whether "Create party" is enabled: a name, server and relay reachable, and another
    /// server's keys trusted.
    pub can_create: bool,
}

/// The Advanced screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advanced {
    /// How private the setup is.
    pub privacy: DraftPrivacy,
    /// Which server.
    pub server_choice: ServerChoice,
    /// The app's own server.
    pub built_in: Host,
    /// The server field.
    pub server: ServiceDraft,
    /// The relay field.
    pub relay: ServiceDraft,
    /// The push relay choice.
    pub push_relay: PushRelayChoice,
    /// The post-quantum switch.
    pub post_quantum: PostQuantum,
}

fn draft_privacy(draft: &PartyDraft, wake_pushes: WakePushes) -> DraftPrivacy {
    let server = draft.server.check.operator();
    let relay = draft.relay.check.operator();
    let push_relay = draft.push_relay_operator.as_ref();
    let push_known = push_relay.is_some() || matches!(wake_pushes, WakePushes::NoWakePushes);
    if server.is_none() || relay.is_none() || !push_known {
        return DraftPrivacy::Checking;
    }
    let overlaps = overlaps(
        Operators {
            server,
            relay,
            push_relay,
        },
        wake_pushes,
    );
    DraftPrivacy::Known {
        level: privacy_level(&overlaps),
        overlaps,
    }
}

/// The New party screen.
#[must_use]
pub fn new_party(draft: &PartyDraft, wake_pushes: WakePushes) -> NewParty {
    let reachable = |service: &ServiceDraft| {
        matches!(service.check, Reachability::Reachable(_))
            && Host::new(service.host.as_str()).is_ok()
    };
    NewParty {
        name: draft.name.clone(),
        colour: draft.colour,
        server_choice: draft.server_choice,
        built_in: draft.built_in.clone(),
        server: draft.server.host.clone(),
        server_keys: (draft.server_choice == ServerChoice::Another)
            .then(|| draft.server_keys.clone()),
        privacy: draft_privacy(draft, wake_pushes),
        can_create: draft.name.name().is_some()
            && reachable(&draft.server)
            && reachable(&draft.relay)
            && (draft.server_choice == ServerChoice::BuiltIn
                || draft.server_keys == ServerKeys::Trusted),
    }
}

/// The Advanced screen.
#[must_use]
pub fn advanced(draft: &PartyDraft, wake_pushes: WakePushes) -> Advanced {
    Advanced {
        privacy: draft_privacy(draft, wake_pushes),
        server_choice: draft.server_choice,
        built_in: draft.built_in.clone(),
        server: draft.server.clone(),
        relay: draft.relay.clone(),
        push_relay: draft.push_relay.clone(),
        post_quantum: draft.post_quantum,
    }
}

/// What the server said to the code last tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CodeAnswer {
    /// No code tried yet: the server only said it needs one.
    NotTried,
    /// The server refused the code; no Party was created.
    Refused,
}

/// "This server needs a code" (canvas `PartyCode`, `PartyCodeRefused`), opened when the chosen
/// server refuses to make an account without a registration code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyCode {
    /// The server chosen.
    pub server: Host,
    /// Who publishes they run it, once known.
    pub operator: Option<Operator>,
    /// The Party's name.
    pub party_name: Name,
    /// The code typed.
    pub code: DraftText,
    /// What the server said.
    pub answer: CodeAnswer,
    /// Whether "Create party" (or "Try again") is enabled: a code is typed.
    pub can_create: bool,
}

/// "This server needs a code" for a draft.
///
/// # Errors
///
/// [`ScreenError::InvalidName`] for a draft without a name; [`ScreenError::InvalidHost`] for a
/// server that is not a host.
pub fn party_code(
    draft: &PartyDraft,
    code: &DraftText,
    answer: CodeAnswer,
) -> Result<PartyCode, ScreenError> {
    let server = match draft.server_choice {
        ServerChoice::BuiltIn => draft.built_in.clone(),
        ServerChoice::Another => Host::new(draft.server.host.as_str())?,
    };
    Ok(PartyCode {
        server,
        operator: draft.server.check.operator().cloned(),
        party_name: draft.name.name().ok_or(ScreenError::InvalidName)?,
        code: code.clone(),
        answer,
        can_create: !code.as_str().trim().is_empty(),
    })
}

/// A Spot's shape as its row describes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpotShape {
    /// A circle of this radius.
    Circle(Meters),
    /// A drawn area of this size.
    Area(SquareMeters),
}

impl SpotShape {
    /// The shape of a fence.
    #[must_use]
    pub fn of(fence: &Fence) -> Self {
        match fence {
            Fence::Circle(circle) => Self::Circle(circle.radius()),
            Fence::Polygon(polygon) => Self::Area(polygon.area()),
        }
    }
}

/// A Spot in a Party's list, and on its map.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotRow {
    /// The Spot.
    pub id: SpotId,
    /// Its name.
    pub name: Name,
    /// Its shape, for the row.
    pub shape: SpotShape,
    /// Its fence, for the map.
    pub fence: Fence,
    /// Whether this device is in a Meetup there.
    pub in_meetup: bool,
}

/// A Party's screen.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyView {
    /// The Party.
    pub id: PartyId,
    /// Its name.
    pub name: Name,
    /// Its members, this device included.
    pub members: Vec<Person>,
    /// Its Spots.
    pub spots: Vec<SpotRow>,
    /// The server, for "What this party's server can see".
    pub server: Host,
    /// How private its setup is.
    pub privacy: PrivacyLevel,
    /// The Meetup this device is in for this Party, if any ("You're in a meetup").
    pub meetup: Option<GroupId>,
}

/// A Party's screen.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn party(snapshot: &ClientSnapshot, id: PartyId) -> Result<PartyView, ScreenError> {
    let party = snapshot.party(id)?;
    let meetup = snapshot.meetups.iter().find(|meetup| meetup.party == id);
    let overlaps = party_overlaps(&party.config, snapshot.settings.wake_pushes);
    Ok(PartyView {
        id,
        name: party.name.clone(),
        members: party
            .members
            .iter()
            .map(|member| Person::new(member.leaf, &member.name, member.me))
            .collect(),
        spots: party
            .spots
            .iter()
            .map(|spot| SpotRow {
                id: spot.id,
                name: spot.name.clone(),
                shape: SpotShape::of(&spot.fence),
                fence: spot.fence.clone(),
                in_meetup: meetup.is_some_and(|meetup| meetup.spot == spot.id),
            })
            .collect(),
        server: party.config.server.host.clone(),
        privacy: privacy_level(&overlaps),
        meetup: meetup.map(|meetup| meetup.id),
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::common::Me;
    use crate::fixtures::{self, FixtureError};

    fn operator(name: &str) -> Result<Operator, ScreenError> {
        Ok(Operator::new(name)?)
    }

    fn draft() -> Result<PartyDraft, ScreenError> {
        Ok(PartyDraft {
            name: DraftText::new("Climbing crew".into()),
            colour: PartyColour::Blue,
            server_choice: ServerChoice::Another,
            built_in: Host::new("server.pingers.org")?,
            server: ServiceDraft {
                host: DraftText::new("pingers.flat7.org".into()),
                check: Reachability::Reachable(operator("Flat 7")?),
            },
            server_keys: ServerKeys::Trusted,
            relay: ServiceDraft {
                host: DraftText::new("relay.quietpath.net".into()),
                check: Reachability::Reachable(operator("Quietpath")?),
            },
            push_relay: PushRelayChoice::Official,
            push_relay_operator: Some(operator("Pingers")?),
            post_quantum: PostQuantum::Off,
        })
    }

    #[test]
    fn three_operators_are_full_privacy() -> Result<(), ScreenError> {
        let screen = new_party(&draft()?, WakePushes::On);
        assert_eq!(
            screen.privacy,
            DraftPrivacy::Known {
                level: PrivacyLevel::Full,
                overlaps: Vec::new()
            }
        );
        assert!(screen.can_create);
        assert_eq!(screen.server.as_str(), "pingers.flat7.org");
        Ok(())
    }

    #[test]
    fn another_server_needs_its_keys_trusted_before_create() -> Result<(), ScreenError> {
        let mut draft = draft()?;
        draft.server_keys = ServerKeys::ToTrust {
            fingerprint: "1a2b 3c4d".into(),
        };
        let screen = new_party(&draft, WakePushes::On);
        assert!(!screen.can_create, "keys shown, not yet trusted");
        assert_eq!(screen.server_keys, Some(draft.server_keys.clone()));
        draft.server_keys = ServerKeys::NotLookedUp;
        assert!(!new_party(&draft, WakePushes::On).can_create);
        // The app's server is pinned by the build: no step, whatever the field holds.
        draft.server_choice = ServerChoice::BuiltIn;
        let built_in = new_party(&draft, WakePushes::On);
        assert!(built_in.can_create);
        assert_eq!(built_in.server_keys, None);
        Ok(())
    }

    #[test]
    fn each_shared_operator_is_named() -> Result<(), ScreenError> {
        let flat = operator("flat 7")?;
        let quiet = operator("Quietpath")?;
        let pingers = operator("Pingers")?;
        let check = |server: &Operator, relay: &Operator, push: &Operator, pushes| {
            overlaps(
                Operators {
                    server: Some(server),
                    relay: Some(relay),
                    push_relay: Some(push),
                },
                pushes,
            )
        };
        assert_eq!(
            check(&flat, &flat, &pingers, WakePushes::On),
            [Overlap::ServerRunsRelay]
        );
        assert_eq!(
            check(&flat, &quiet, &flat, WakePushes::On),
            [Overlap::ServerRunsPushRelay]
        );
        assert_eq!(
            check(&flat, &quiet, &quiet, WakePushes::On),
            [Overlap::RelayRunsPushRelay]
        );
        assert_eq!(
            check(&flat, &flat, &flat, WakePushes::On),
            [Overlap::ServerRunsRelay, Overlap::ServerRunsPushRelay]
        );
        // Without wake-up pushes the push relay never hears of this device.
        assert_eq!(check(&flat, &quiet, &flat, WakePushes::NoWakePushes), []);
        assert_eq!(check(&flat, &quiet, &quiet, WakePushes::NoWakePushes), []);
        assert_eq!(
            check(&flat, &flat, &flat, WakePushes::NoWakePushes),
            [Overlap::ServerRunsRelay]
        );
        assert_eq!(
            privacy_level(&[Overlap::RelayRunsPushRelay]),
            PrivacyLevel::Reduced
        );
        assert_eq!(privacy_level(&[]), PrivacyLevel::Full);
        Ok(())
    }

    #[test]
    fn privacy_is_unknown_while_a_check_runs() -> Result<(), ScreenError> {
        let mut checking = draft()?;
        checking.relay.check = Reachability::Checking;
        let screen = new_party(&checking, WakePushes::On);
        assert_eq!(screen.privacy, DraftPrivacy::Checking);
        assert!(!screen.can_create);

        let mut server_checking = draft()?;
        server_checking.server.check = Reachability::Checking;
        assert_eq!(
            advanced(&server_checking, WakePushes::On).privacy,
            DraftPrivacy::Checking
        );

        let mut push_unknown = draft()?;
        push_unknown.push_relay_operator = None;
        assert_eq!(
            advanced(&push_unknown, WakePushes::On).privacy,
            DraftPrivacy::Checking
        );
        // The push relay's operator does not matter without wake-up pushes.
        assert!(matches!(
            advanced(&push_unknown, WakePushes::NoWakePushes).privacy,
            DraftPrivacy::Known {
                level: PrivacyLevel::Full,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn create_needs_a_name_and_both_services() -> Result<(), ScreenError> {
        let mut unnamed = draft()?;
        unnamed.name = DraftText::new("   ".into());
        assert!(!new_party(&unnamed, WakePushes::On).can_create);
        let mut down = draft()?;
        down.server.check = Reachability::Unreachable;
        assert!(!new_party(&down, WakePushes::On).can_create);
        let mut bad_host = draft()?;
        bad_host.relay.host = DraftText::new("not a host".into());
        assert!(!new_party(&bad_host, WakePushes::On).can_create);
        Ok(())
    }

    #[test]
    fn advanced_shows_the_draft() -> Result<(), ScreenError> {
        let mut quantum = draft()?;
        quantum.post_quantum = PostQuantum::On;
        quantum.push_relay = PushRelayChoice::Custom(DraftText::new("https://up.example".into()));
        let screen = advanced(&quantum, WakePushes::On);
        assert_eq!(screen.post_quantum, PostQuantum::On);
        assert_eq!(screen.push_relay, quantum.push_relay);
        assert_eq!(screen.server, quantum.server);
        assert_eq!(screen.relay, quantum.relay);
        Ok(())
    }

    #[test]
    fn the_code_screen_names_the_server_and_its_operator() -> Result<(), ScreenError> {
        let typed = DraftText::new("spring-otter-4471".into());
        let view = party_code(&draft()?, &typed, CodeAnswer::NotTried)?;
        assert_eq!(view.server.as_str(), "pingers.flat7.org");
        assert_eq!(view.operator, Some(operator("Flat 7")?));
        assert_eq!(view.party_name.as_str(), "Climbing crew");
        assert!(view.can_create);
        let refused = party_code(&draft()?, &DraftText::new("  ".into()), CodeAnswer::Refused)?;
        assert_eq!(refused.answer, CodeAnswer::Refused);
        assert!(!refused.can_create);
        let mut built_in = draft()?;
        built_in.server_choice = ServerChoice::BuiltIn;
        built_in.server.check = Reachability::Checking;
        let view = party_code(&built_in, &typed, CodeAnswer::NotTried)?;
        assert_eq!(view.server.as_str(), "server.pingers.org");
        assert_eq!(view.operator, None);
        let mut unnamed = draft()?;
        unnamed.name = DraftText::new(String::new());
        assert_eq!(
            party_code(&unnamed, &typed, CodeAnswer::NotTried),
            Err(ScreenError::InvalidName)
        );
        Ok(())
    }

    #[test]
    fn party_lists_members_and_spots() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = party(&snapshot, fixtures::climbing_crew())?;
        assert_eq!(view.name.as_str(), "Climbing crew");
        assert_eq!(view.members.len(), 6);
        assert_eq!(
            view.members
                .iter()
                .filter(|person| person.me == Me::Me)
                .count(),
            1
        );
        let rows: Vec<_> = view
            .spots
            .iter()
            .map(|row| (row.name.as_str(), row.in_meetup))
            .collect();
        assert_eq!(
            rows,
            [
                ("Riverside Park", true),
                ("The Wall climbing gym", false),
                ("Ana's place", false)
            ]
        );
        assert_eq!(
            view.spots.first().map(|row| row.shape),
            Some(SpotShape::Circle(Meters::from_whole(300)))
        );
        let SpotShape::Area(area) = view
            .spots
            .get(1)
            .map(|row| row.shape)
            .ok_or(FixtureError::Missing)?
        else {
            return Err(FixtureError::Missing);
        };
        assert!((area.get() - 4000.0).abs() < 40.0, "{}", area.get());
        assert_eq!(view.server.as_str(), "pingers.flat7.org");
        assert_eq!(view.privacy, PrivacyLevel::Full);
        assert_eq!(view.meetup, Some(fixtures::meetup_id()));

        let quiet = party(&snapshot, fixtures::uni_flat())?;
        assert_eq!(quiet.meetup, None);
        assert!(quiet.spots.iter().all(|row| !row.in_meetup));
        assert_eq!(
            party(&snapshot, PartyId::from_bytes([9; 16])),
            Err(ScreenError::UnknownParty)
        );
        Ok(())
    }

    #[test]
    fn a_shared_operator_reduces_a_partys_privacy() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.config.relay.operator = Some(Operator::new("Flat 7")?);
        }
        assert_eq!(
            party(&snapshot, fixtures::climbing_crew())?.privacy,
            PrivacyLevel::Reduced
        );
        Ok(())
    }
}
