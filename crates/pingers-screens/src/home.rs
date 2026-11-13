// SPDX-License-Identifier: AGPL-3.0-only
//! Home: the sharing summary, the live Meetup, the Parties, and an alert that arrived while the
//! app was open.

use pingers_gate::{GateDecision, LocationSharing, SuppressReason};
use pingers_proto::{GroupId, PartyId};

use crate::common::{Initials, Name, PartyColour, Person};
use crate::snapshot::{ClientSnapshot, Meetup, PauseInFocus};

/// The Home screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    /// The summary card at the top.
    pub sharing: SharingSummary,
    /// The live Meetup, if this device is in one.
    pub live_meetup: Option<LiveMeetupCard>,
    /// The Parties.
    pub parties: Vec<PartyRow>,
    /// A co-presence alert that arrived while the app was open.
    pub alert: Option<CoPresenceAlert>,
}

/// Whether this device is sharing its presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sharing {
    /// Sharing.
    On,
    /// The sharing switch is off.
    Off,
    /// The switch is on, but the gate suppresses sharing for this reason.
    Paused(SuppressReason),
}

impl Sharing {
    /// Sharing as the gate and the switch decide it.
    #[must_use]
    pub const fn from_gate(switch: LocationSharing, gate: GateDecision) -> Self {
        match (switch, gate) {
            (LocationSharing::Off, _)
            | (_, GateDecision::Suppress(SuppressReason::LocationSharingOff)) => Self::Off,
            (LocationSharing::On, GateDecision::Suppress(reason)) => Self::Paused(reason),
            (LocationSharing::On, GateDecision::Share) => Self::On,
        }
    }
}

/// The summary card: "Sharing on · 3 parties", "Pauses in Focus · 2 exclusion zones".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharingSummary {
    /// Whether sharing is on.
    pub sharing: Sharing,
    /// How many Parties.
    pub party_count: usize,
    /// "Pauses in Focus".
    pub pause_in_focus: PauseInFocus,
    /// How many exclusion zones.
    pub exclusion_zones: usize,
}

/// The live Meetup card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveMeetupCard {
    /// The Meetup.
    pub meetup: GroupId,
    /// Its Party's name.
    pub party_name: Name,
    /// Its Spot's name.
    pub spot_name: Name,
    /// Everyone in it, others first and this device last, as the avatar stack shows them.
    pub people: Vec<Person>,
}

/// A Party in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartyRow {
    /// The Party.
    pub id: PartyId,
    /// Its name.
    pub name: Name,
    /// Its avatar letters.
    pub initials: Initials,
    /// Its colour.
    pub colour: PartyColour,
    /// How many members.
    pub members: usize,
    /// How many Spots.
    pub spots: usize,
    /// Whether one of its Meetups is live with this device in it.
    pub live: bool,
}

/// A co-presence alert: "Ben and Chloé are at Riverside Park too."
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoPresenceAlert {
    /// The Meetup to open.
    pub meetup: GroupId,
    /// Its Party's name.
    pub party_name: Name,
    /// The others in it.
    pub others: Vec<Name>,
    /// Its Spot's name.
    pub spot_name: Name,
}

/// The names a Meetup card or alert shows: the Party's and the Spot's. `None` if the snapshot
/// no longer has the Party or the Spot (it was just removed): the card is then left out rather
/// than shown half-named.
pub(crate) fn meetup_names(snapshot: &ClientSnapshot, meetup: &Meetup) -> Option<MeetupNames> {
    let party = snapshot.party(meetup.party).ok()?;
    let spot = party.spot(meetup.spot)?;
    Some(MeetupNames {
        party: party.name.clone(),
        spot: spot.name.clone(),
    })
}

pub(crate) struct MeetupNames {
    pub(crate) party: Name,
    pub(crate) spot: Name,
}

/// The co-presence alert in the snapshot, if any.
#[must_use]
pub fn alert(snapshot: &ClientSnapshot) -> Option<CoPresenceAlert> {
    let meetup = snapshot.meetup(snapshot.alert?).ok()?;
    let names = meetup_names(snapshot, meetup)?;
    Some(CoPresenceAlert {
        meetup: meetup.id,
        party_name: names.party,
        others: meetup.others.iter().map(|peer| peer.name.clone()).collect(),
        spot_name: names.spot,
    })
}

/// The Home screen.
#[must_use]
pub fn home(snapshot: &ClientSnapshot) -> Home {
    let settings = &snapshot.settings;
    let live_meetup = snapshot.meetups.iter().find_map(|meetup| {
        let names = meetup_names(snapshot, meetup)?;
        let mut people = meetup.people();
        people.rotate_left(1);
        Some(LiveMeetupCard {
            meetup: meetup.id,
            party_name: names.party,
            spot_name: names.spot,
            people,
        })
    });
    let parties = snapshot
        .parties
        .iter()
        .map(|party| PartyRow {
            id: party.id,
            initials: party.name.initials(),
            name: party.name.clone(),
            colour: party.colour,
            members: party.members.len(),
            spots: party.spots.len(),
            live: snapshot
                .meetups
                .iter()
                .any(|meetup| meetup.party == party.id),
        })
        .collect();
    Home {
        sharing: SharingSummary {
            sharing: Sharing::from_gate(settings.location_sharing, snapshot.gate),
            party_count: snapshot.parties.len(),
            pause_in_focus: settings.pause_in_focus,
            exclusion_zones: snapshot.zones.len(),
        },
        live_meetup,
        parties,
        alert: alert(snapshot),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn home_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let home = home(&snapshot);
        assert_eq!(home.sharing.sharing, Sharing::On);
        assert_eq!(home.sharing.party_count, 3);
        assert_eq!(home.sharing.exclusion_zones, 2);
        assert_eq!(home.sharing.pause_in_focus, PauseInFocus::On);
        let rows: Vec<_> = home
            .parties
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.initials.as_str(),
                    row.members,
                    row.spots,
                    row.live,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("Climbing crew", "CC", 6, 3, true),
                ("Uni flat", "UF", 4, 2, false),
                ("Saturday 5-a-side", "S5", 11, 1, false),
            ]
        );
        let card = home.live_meetup.ok_or(FixtureError::Missing)?;
        assert_eq!(card.party_name.as_str(), "Climbing crew");
        assert_eq!(card.spot_name.as_str(), "Riverside Park");
        let names: Vec<_> = card
            .people
            .iter()
            .map(|person| person.name.as_str())
            .collect();
        assert_eq!(names, ["Ben", "Chloé", "Sam"]);
        assert_eq!(home.alert, None);
        Ok(())
    }

    #[test]
    fn alert_names_the_others_and_the_spot() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.alert = Some(fixtures::meetup_id());
        let alert = home(&snapshot).alert.ok_or(FixtureError::Missing)?;
        assert_eq!(alert.meetup, fixtures::meetup_id());
        let others: Vec<_> = alert.others.iter().map(Name::as_str).collect();
        assert_eq!(others, ["Ben", "Chloé"]);
        assert_eq!(alert.spot_name.as_str(), "Riverside Park");
        assert_eq!(alert.party_name.as_str(), "Climbing crew");
        Ok(())
    }

    #[test]
    fn a_meetup_whose_party_is_gone_shows_no_card() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.alert = Some(fixtures::meetup_id());
        snapshot
            .parties
            .retain(|party| party.id != fixtures::climbing_crew());
        let home = home(&snapshot);
        assert_eq!(home.live_meetup, None);
        assert_eq!(home.alert, None);
        Ok(())
    }

    #[test]
    fn a_meetup_whose_spot_is_gone_shows_no_card() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.spots.clear();
        }
        assert_eq!(home(&snapshot).live_meetup, None);
        Ok(())
    }

    #[test]
    fn sharing_is_off_paused_or_on() {
        use LocationSharing::{Off, On};
        let focus = GateDecision::Suppress(SuppressReason::Focus);
        let off = GateDecision::Suppress(SuppressReason::LocationSharingOff);
        assert_eq!(Sharing::from_gate(On, GateDecision::Share), Sharing::On);
        assert_eq!(
            Sharing::from_gate(On, focus),
            Sharing::Paused(SuppressReason::Focus)
        );
        assert_eq!(Sharing::from_gate(Off, focus), Sharing::Off);
        assert_eq!(Sharing::from_gate(Off, GateDecision::Share), Sharing::Off);
        assert_eq!(Sharing::from_gate(On, off), Sharing::Off);
    }
}
