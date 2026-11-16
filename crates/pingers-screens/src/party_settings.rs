// SPDX-License-Identifier: AGPL-3.0-only
//! A Party's own settings (canvas `PartySettings`, `PartySettingsShared`), reached from the
//! row at the bottom of its screen, and the screens it opens: Safety codes (`SafetyCode`) and
//! Leave party (`LeaveParty`).
//!
//! Party settings shows this Party's server, privacy relay and push relay with the operators
//! they publish. When two share an operator the page says Reduced privacy: a phone in Parties
//! on several servers can see it on one Party and not another.

use pingers_proto::{LeafIndex, PartyId};
use pingers_time::UnixSeconds;

use crate::ScreenError;
use crate::common::{Host, Me, Name, Operator, Person, PrivacyLevel};
use crate::party::{Overlap, party_overlaps, privacy_level};
use crate::snapshot::{
    ClientSnapshot, KeyTrust, PostQuantum, PushRelayKind, RelayStatus, SafetyCheck, SafetyCode,
    WakePushes,
};

/// Whether a service's operator is also the server's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SharedOperator {
    /// Its own operator, or the server itself.
    No,
    /// The server's operator runs it too.
    WithServer,
}

/// A server or relay row: where it is and who publishes they run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceRow {
    /// Where it is, once known.
    pub host: Option<Host>,
    /// Who publishes they run it; `None` if it publishes nothing (or not yet).
    pub operator: Option<Operator>,
    /// Whether the server's operator runs it too.
    pub shared: SharedOperator,
}

/// The privacy relay check: the last answer, and when it came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayCheck {
    /// The answer.
    pub status: RelayStatus,
    /// When this device last checked, in this process; `None` before the first check.
    pub checked_at: Option<UnixSeconds>,
}

/// "Checked with 2 of 5 members".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafetySummary {
    /// Members checked.
    pub checked: usize,
    /// Members other than this device.
    pub others: usize,
}

/// The Party settings screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartySettings {
    /// The Party.
    pub party: PartyId,
    /// Its name.
    pub name: Name,
    /// How many members, this device included.
    pub members: usize,
    /// Full or reduced.
    pub privacy: PrivacyLevel,
    /// Which services share an operator.
    pub overlaps: Vec<Overlap>,
    /// The server.
    pub server: ServiceRow,
    /// The privacy relay.
    pub relay: ServiceRow,
    /// The relay check: this Party's relay, to this Party's server.
    pub relay_check: RelayCheck,
    /// The push relay.
    pub push_relay: ServiceRow,
    /// Wake-up pushes, which decide whether the push relay counts.
    pub wake_pushes: WakePushes,
    /// Safety codes checked.
    pub safety: SafetySummary,
    /// How this device came to trust the server's keys.
    pub keys: KeyTrust,
    /// The Party's MLS suite.
    pub post_quantum: PostQuantum,
}

fn shared(server: Option<&Operator>, other: Option<&Operator>) -> SharedOperator {
    match (server, other) {
        (Some(server), Some(other)) if server == other => SharedOperator::WithServer,
        _ => SharedOperator::No,
    }
}

/// The Party settings screen. `relay_checked_at` is when the shell last ran the relay check.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn party_settings(
    snapshot: &ClientSnapshot,
    party: PartyId,
    relay_checked_at: Option<UnixSeconds>,
) -> Result<PartySettings, ScreenError> {
    let found = snapshot.party(party)?;
    let config = &found.config;
    let wake_pushes = snapshot.settings.wake_pushes;
    let overlaps = party_overlaps(config, wake_pushes);
    let server_operator = config.server.operator.as_ref();
    let others: Vec<_> = found
        .members
        .iter()
        .filter(|member| member.me == Me::Other)
        .collect();
    Ok(PartySettings {
        party,
        name: found.name.clone(),
        members: found.members.len(),
        privacy: privacy_level(&overlaps),
        server: ServiceRow {
            host: Some(config.server.host.clone()),
            operator: config.server.operator.clone(),
            shared: SharedOperator::No,
        },
        relay: ServiceRow {
            host: Some(config.relay.host.clone()),
            operator: config.relay.operator.clone(),
            shared: if overlaps.contains(&Overlap::ServerRunsRelay) {
                SharedOperator::WithServer
            } else {
                SharedOperator::No
            },
        },
        relay_check: RelayCheck {
            status: config.relay_status,
            checked_at: relay_checked_at,
        },
        push_relay: ServiceRow {
            host: match &config.push_relay.kind {
                PushRelayKind::Custom(host) => Some(host.clone()),
                PushRelayKind::Official => config.push_relay.host.clone(),
            },
            operator: config.push_relay.operator.clone(),
            shared: shared(server_operator, config.push_relay.operator.as_ref()),
        },
        overlaps,
        wake_pushes,
        safety: SafetySummary {
            checked: others
                .iter()
                .filter(|member| member.checked == SafetyCheck::Checked)
                .count(),
            others: others.len(),
        },
        keys: config.keys,
        post_quantum: config.post_quantum,
    })
}

/// A member on Safety codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyRow {
    /// Who.
    pub person: Person,
    /// Checked or not.
    pub checked: SafetyCheck,
    /// Whether their code is the one shown.
    pub selected: bool,
}

/// The code shown at the top of Safety codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownCode {
    /// Whose.
    pub person: Person,
    /// The 60 digits.
    pub code: SafetyCode,
    /// Whether it was checked already.
    pub checked: SafetyCheck,
}

/// The Safety codes screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyCodes {
    /// The Party's name.
    pub party_name: Name,
    /// The code shown, once the core computed it.
    pub shown: Option<ShownCode>,
    /// Every other member.
    pub members: Vec<SafetyRow>,
}

/// Whose code Safety codes opens on: the first member not checked yet, else the first.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn first_to_check(
    snapshot: &ClientSnapshot,
    party: PartyId,
) -> Result<Option<LeafIndex>, ScreenError> {
    let found = snapshot.party(party)?;
    let others = found.members.iter().filter(|member| member.me == Me::Other);
    let first = others.clone().next().map(|member| member.leaf);
    Ok(others
        .clone()
        .find(|member| member.checked == SafetyCheck::NotChecked)
        .map(|member| member.leaf)
        .or(first))
}

/// The Safety codes screen, showing `code` for the member at `selected`.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`]; [`ScreenError::UnknownMember`] if `selected` is not another
/// member.
pub fn safety_codes(
    snapshot: &ClientSnapshot,
    party: PartyId,
    selected: Option<(LeafIndex, SafetyCode)>,
) -> Result<SafetyCodes, ScreenError> {
    let found = snapshot.party(party)?;
    let others: Vec<_> = found
        .members
        .iter()
        .filter(|member| member.me == Me::Other)
        .collect();
    let shown = match selected {
        Some((leaf, code)) => {
            let member = others
                .iter()
                .find(|member| member.leaf == leaf)
                .ok_or(ScreenError::UnknownMember)?;
            Some(ShownCode {
                person: Person::new(member.leaf, &member.name, member.me),
                code,
                checked: member.checked,
            })
        }
        None => None,
    };
    let shown_leaf = shown.as_ref().map(|shown| shown.person.leaf);
    Ok(SafetyCodes {
        party_name: found.name.clone(),
        members: others
            .iter()
            .map(|member| SafetyRow {
                person: Person::new(member.leaf, &member.name, member.me),
                checked: member.checked,
                selected: shown_leaf == Some(member.leaf),
            })
            .collect(),
        shown,
    })
}

/// The Leave party sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveParty {
    /// The Party.
    pub party: PartyId,
    /// Its name.
    pub name: Name,
    /// How many Spots it has.
    pub spots: usize,
    /// The Spot of the Meetup this device is in with this Party, which leaving ends.
    pub meetup_at: Option<Name>,
}

/// The Leave party sheet.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn leave_party(snapshot: &ClientSnapshot, party: PartyId) -> Result<LeaveParty, ScreenError> {
    let found = snapshot.party(party)?;
    let meetup_at = snapshot
        .meetups
        .iter()
        .find(|meetup| meetup.party == party)
        .and_then(|meetup| found.spot(meetup.spot))
        .map(|spot| spot.name.clone());
    Ok(LeaveParty {
        party,
        name: found.name.clone(),
        spots: found.spots.len(),
        meetup_at,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn three_operators_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let checked_at = Some(UnixSeconds::new(500));
        let view = party_settings(&snapshot, fixtures::climbing_crew(), checked_at)?;
        assert_eq!(view.name.as_str(), "Climbing crew");
        assert_eq!(view.members, 6);
        assert_eq!(view.privacy, PrivacyLevel::Full);
        assert_eq!(view.overlaps, []);
        assert_eq!(
            view.server.operator.as_ref().map(Operator::as_str),
            Some("Flat 7")
        );
        assert_eq!(
            view.relay.host.as_ref().map(Host::as_str),
            Some("relay.quietpath.net")
        );
        assert_eq!(
            view.push_relay.host.as_ref().map(Host::as_str),
            Some("push.pingers.org")
        );
        assert_eq!(view.push_relay.shared, SharedOperator::No);
        assert_eq!(
            view.relay_check,
            RelayCheck {
                status: RelayStatus::Reachable,
                checked_at
            }
        );
        assert_eq!(
            view.safety,
            SafetySummary {
                checked: 2,
                others: 5
            }
        );
        assert_eq!(view.post_quantum, PostQuantum::Off);
        Ok(())
    }

    #[test]
    fn each_party_shows_its_own_server_relay_check_and_keys() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let first = snapshot.parties.first().map(|party| party.id);
        let second = snapshot.parties.get(1).map(|party| party.id);
        let (Some(first), Some(second)) = (first, second) else {
            return Err(FixtureError::Missing);
        };
        if let Some(party) = snapshot.parties.get_mut(1) {
            party.config.origin = pingers_proto::ServerId::new("https://other.example".to_owned())?;
            party.config.server.host = Host::new("other.example")?;
            party.config.keys = KeyTrust::FromInvite;
            party.config.relay_status = RelayStatus::Unreachable;
        }
        let one = party_settings(&snapshot, first, None)?;
        let two = party_settings(&snapshot, second, None)?;
        assert_eq!(
            one.server.host.as_ref().map(Host::as_str),
            Some("pingers.flat7.org")
        );
        assert_eq!(
            two.server.host.as_ref().map(Host::as_str),
            Some("other.example")
        );
        assert_eq!(one.keys, KeyTrust::Confirmed);
        assert_eq!(two.keys, KeyTrust::FromInvite);
        assert_eq!(one.relay_check.status, RelayStatus::Reachable);
        assert_eq!(two.relay_check.status, RelayStatus::Unreachable);
        Ok(())
    }

    #[test]
    fn the_servers_operator_running_the_push_relay_is_reduced() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.config.push_relay.operator = Some(Operator::new("Flat 7")?);
        }
        let view = party_settings(&snapshot, fixtures::climbing_crew(), None)?;
        assert_eq!(view.privacy, PrivacyLevel::Reduced);
        assert_eq!(view.overlaps, [Overlap::ServerRunsPushRelay]);
        assert_eq!(view.push_relay.shared, SharedOperator::WithServer);
        assert_eq!(view.relay.shared, SharedOperator::No);
        // Turning off wake-ups stops it, for every Party on the phone.
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        let off = party_settings(&snapshot, fixtures::climbing_crew(), None)?;
        assert_eq!(off.privacy, PrivacyLevel::Full);
        Ok(())
    }

    #[test]
    fn a_relay_that_publishes_no_operator_counts_as_the_servers() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.config.relay.operator = None;
        }
        let view = party_settings(&snapshot, fixtures::climbing_crew(), None)?;
        assert_eq!(view.overlaps, [Overlap::ServerRunsRelay]);
        assert_eq!(view.relay.shared, SharedOperator::WithServer);
        assert_eq!(view.relay.operator, None);
        assert_eq!(
            party_settings(&snapshot, PartyId::from_bytes([9; 16]), None),
            Err(ScreenError::UnknownParty)
        );
        Ok(())
    }

    #[test]
    fn safety_codes_open_on_the_first_member_not_checked() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let party = fixtures::climbing_crew();
        let first = first_to_check(&snapshot, party)?.ok_or(FixtureError::Missing)?;
        let code = SafetyCode::new("48210 93755")?;
        let view = safety_codes(&snapshot, party, Some((first, code.clone())))?;
        let shown = view.shown.ok_or(FixtureError::Missing)?;
        assert_eq!(shown.person.name.as_str(), "Ana");
        assert_eq!(shown.code, code);
        assert_eq!(shown.checked, SafetyCheck::NotChecked);
        let rows: Vec<_> = view
            .members
            .iter()
            .map(|row| (row.person.name.as_str(), row.checked, row.selected))
            .collect();
        assert_eq!(
            rows,
            [
                ("Ana", SafetyCheck::NotChecked, true),
                ("Chloé", SafetyCheck::Checked, false),
                ("Ben", SafetyCheck::Checked, false),
                ("Dev", SafetyCheck::NotChecked, false),
                ("Eli", SafetyCheck::NotChecked, false),
            ]
        );
        Ok(())
    }

    #[test]
    fn only_other_members_have_safety_codes() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let party = fixtures::climbing_crew();
        let me = snapshot
            .party(party)?
            .members
            .iter()
            .find(|member| member.me == Me::Me)
            .ok_or(FixtureError::Missing)?
            .leaf;
        let code = SafetyCode::new("1")?;
        assert_eq!(
            safety_codes(&snapshot, party, Some((me, code))),
            Err(ScreenError::UnknownMember)
        );
        assert_eq!(safety_codes(&snapshot, party, None)?.shown, None);
        // Everyone checked: it opens on the first member anyway.
        for found in &mut snapshot.parties {
            for member in &mut found.members {
                member.checked = SafetyCheck::Checked;
            }
        }
        let first = first_to_check(&snapshot, party)?;
        assert_eq!(first.map(LeafIndex::get), Some(0));
        Ok(())
    }

    #[test]
    fn leaving_names_the_spots_and_the_running_meetup() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = leave_party(&snapshot, fixtures::climbing_crew())?;
        assert_eq!(view.name.as_str(), "Climbing crew");
        assert_eq!(view.spots, 3);
        assert_eq!(
            view.meetup_at.as_ref().map(Name::as_str),
            Some("Riverside Park")
        );
        let quiet = leave_party(&snapshot, fixtures::uni_flat())?;
        assert_eq!(quiet.meetup_at, None);
        Ok(())
    }
}
