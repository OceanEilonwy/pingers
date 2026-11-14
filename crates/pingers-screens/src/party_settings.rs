// SPDX-License-Identifier: AGPL-3.0-only
//! A Party's settings page: the server it lives on, the privacy relay and push relay its
//! requests go through, who declares to run each, whether the relay answers, and how this
//! device came to trust the server's keys. These are per Party: two Parties can be on
//! different servers. The device-wide sections (name, wake-ups, quiet hours, zones, maps) are
//! the Settings screen's (`settings.rs`).

use pingers_proto::{PartyId, ServerId};

use crate::ScreenError;
use crate::common::{Host, Operator};
use crate::snapshot::{
    ClientSnapshot, KeyTrust, PostQuantum, PushRelayKind, RelayStatus, WakePushes,
};

/// One service a Party uses, as its settings page lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceRow {
    /// Where it is.
    pub host: Host,
    /// Who declares to run it.
    pub operator: Operator,
}

/// The push relay row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelayRow {
    /// The official relay, or another one by host.
    pub kind: PushRelayKind,
    /// Who declares to run it.
    pub operator: Operator,
    /// "No wake-up pushes" is on: the relay is not used at all.
    pub wake_pushes: WakePushes,
}

/// A Party's settings page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartySettingsView {
    /// The Party.
    pub party: PartyId,
    /// The server's origin.
    pub origin: ServerId,
    /// The server.
    pub server: ServiceRow,
    /// The privacy relay.
    pub relay: ServiceRow,
    /// Whether the server answered through the relay lately.
    pub relay_status: RelayStatus,
    /// The push relay.
    pub push_relay: PushRelayRow,
    /// How this device came to trust the server's keys.
    pub keys: KeyTrust,
    /// Whether the Party uses the quantum-resistant suite.
    pub post_quantum: PostQuantum,
    /// The server and the push relay declare the same operator (the "Reduced privacy" warning
    /// of "Who runs your server"); never with wake-up pushes off.
    pub same_operator: bool,
}

/// The settings page of Party `id`.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn party_settings(
    snapshot: &ClientSnapshot,
    id: PartyId,
) -> Result<PartySettingsView, ScreenError> {
    let party = snapshot.party(id)?;
    let config = &party.config;
    let wake_pushes = snapshot.settings.wake_pushes;
    Ok(PartySettingsView {
        party: id,
        origin: config.origin.clone(),
        server: ServiceRow {
            host: config.server.host.clone(),
            operator: config.server.operator.clone(),
        },
        relay: ServiceRow {
            host: config.relay.host.clone(),
            operator: config.relay.operator.clone(),
        },
        relay_status: config.relay_status,
        push_relay: PushRelayRow {
            kind: config.push_relay.kind.clone(),
            operator: config.push_relay.operator.clone(),
            wake_pushes,
        },
        keys: config.keys,
        post_quantum: config.post_quantum,
        same_operator: wake_pushes == WakePushes::On
            && config.server.operator == config.push_relay.operator,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn each_party_shows_its_own_server() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let first = snapshot.parties.first().map(|party| party.id);
        let second = snapshot.parties.get(1).map(|party| party.id);
        let (Some(first), Some(second)) = (first, second) else {
            return Err(FixtureError::Missing);
        };
        if let Some(party) = snapshot.parties.get_mut(1) {
            party.config.origin = ServerId::new("https://other.example".to_owned())?;
            party.config.server.host = Host::new("other.example")?;
            party.config.keys = KeyTrust::FromInvite;
            party.config.relay_status = RelayStatus::Unreachable;
        }
        let one = party_settings(&snapshot, first)?;
        let two = party_settings(&snapshot, second)?;
        assert_eq!(one.server.host.as_str(), "pingers.flat7.org");
        assert_eq!(two.server.host.as_str(), "other.example");
        assert_eq!(two.origin.as_str(), "https://other.example");
        assert_eq!(two.keys, KeyTrust::FromInvite);
        assert_eq!(two.relay_status, RelayStatus::Unreachable);
        assert_ne!(one.relay_status, two.relay_status);
        Ok(())
    }

    #[test]
    fn the_same_operator_warning_needs_wake_pushes() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let id = snapshot
            .parties
            .first()
            .map(|party| party.id)
            .ok_or(FixtureError::Missing)?;
        if let Some(party) = snapshot.parties.first_mut() {
            party.config.push_relay.operator = party.config.server.operator.clone();
        }
        snapshot.settings.wake_pushes = WakePushes::On;
        assert!(party_settings(&snapshot, id)?.same_operator);
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        assert!(!party_settings(&snapshot, id)?.same_operator);
        Ok(())
    }

    #[test]
    fn an_unknown_party_has_no_settings() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        assert!(matches!(
            party_settings(&snapshot, PartyId::from_bytes([0xff; 16])),
            Err(ScreenError::UnknownParty)
        ));
        Ok(())
    }
}
