// SPDX-License-Identifier: AGPL-3.0-only
//! What your server sees. This module gathers the live configuration and the active fallbacks
//! into a [`PrivacyConfiguration`]; pingers-words writes the "It can see" and "It never sees"
//! lists from it.

use pingers_client::Fallback;
use pingers_proto::PartyId;

use crate::ScreenError;
use crate::common::{Host, PrivacyLevel};
use crate::party::{Overlap, party_overlaps, privacy_level};
use crate::snapshot::{
    ClientSnapshot, Hosting, PostQuantum, PushRelayKind, RelayStatus, WakePushes,
};

/// How this device proves it may post anonymously (design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Proofs {
    /// RLN membership proofs, with Privacy Pass only in app extensions.
    MembershipProofs,
    /// The Privacy Pass fallback for everything: the server sees each day's token batch.
    PrivacyPassOnly,
}

/// Whose push relay wakes this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushRelaySource {
    /// The official app publisher's.
    Official,
    /// A custom one.
    Custom,
}

/// Everything that changes what the server (and the relays) can learn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyConfiguration {
    /// Services that share an operator, from the declared operators and the client's
    /// `SameOperator` fallback.
    pub overlaps: Vec<Overlap>,
    /// "No wake-up pushes".
    pub wake_pushes: WakePushes,
    /// Membership proofs, or the Privacy Pass fallback.
    pub proofs: Proofs,
    /// Whether anonymous traffic is flowing.
    pub relay: RelayStatus,
    /// Whose push relay.
    pub push_relay: PushRelaySource,
    /// The Party's MLS suite.
    pub post_quantum: PostQuantum,
}

/// The What your server sees screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSees {
    /// The server.
    pub server: Host,
    /// How it is run.
    pub hosting: Hosting,
    /// Full or reduced privacy.
    pub privacy: PrivacyLevel,
    /// What the lists are written from.
    pub configuration: PrivacyConfiguration,
}

/// The What your server sees screen for a Party.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn server_sees(snapshot: &ClientSnapshot, party: PartyId) -> Result<ServerSees, ScreenError> {
    let party = snapshot.party(party)?;
    let wake_pushes = snapshot.settings.wake_pushes;
    let mut found = party_overlaps(&party.config, wake_pushes);
    let same_operator = snapshot.fallbacks.contains(&Fallback::SameOperator);
    if same_operator
        && matches!(wake_pushes, WakePushes::On)
        && !found.contains(&Overlap::ServerRunsPushRelay)
    {
        found.push(Overlap::ServerRunsPushRelay);
    }
    let proofs = if snapshot.fallbacks.contains(&Fallback::PrivacyPassOnly) {
        Proofs::PrivacyPassOnly
    } else {
        Proofs::MembershipProofs
    };
    Ok(ServerSees {
        server: party.config.server.host.clone(),
        hosting: party.config.hosting,
        privacy: privacy_level(&found),
        configuration: PrivacyConfiguration {
            overlaps: found,
            wake_pushes,
            proofs,
            relay: snapshot.relay,
            push_relay: match party.config.push_relay.kind {
                PushRelayKind::Official => PushRelaySource::Official,
                PushRelayKind::Custom(_) => PushRelaySource::Custom,
            },
            post_quantum: party.config.post_quantum,
        },
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::common::Operator;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn the_canvas_setup_is_full_privacy() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = server_sees(&snapshot, fixtures::climbing_crew())?;
        assert_eq!(view.server.as_str(), "pingers.flat7.org");
        assert_eq!(view.hosting, Hosting::SelfHosted);
        assert_eq!(view.privacy, PrivacyLevel::Full);
        assert_eq!(
            view.configuration,
            PrivacyConfiguration {
                overlaps: Vec::new(),
                wake_pushes: WakePushes::On,
                proofs: Proofs::MembershipProofs,
                relay: RelayStatus::Reachable,
                push_relay: PushRelaySource::Official,
                post_quantum: PostQuantum::Off,
            }
        );
        Ok(())
    }

    #[test]
    fn the_same_operator_fallback_counts_once() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.fallbacks = vec![Fallback::SameOperator];
        let view = server_sees(&snapshot, fixtures::climbing_crew())?;
        assert_eq!(view.configuration.overlaps, [Overlap::ServerRunsPushRelay]);
        assert_eq!(view.privacy, PrivacyLevel::Reduced);
        for party in &mut snapshot.parties {
            party.config.push_relay.operator = Some(Operator::new("Flat 7")?);
        }
        let view = server_sees(&snapshot, fixtures::climbing_crew())?;
        assert_eq!(view.configuration.overlaps, [Overlap::ServerRunsPushRelay]);
        Ok(())
    }

    #[test]
    fn without_wake_pushes_the_push_relay_does_not_count() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.fallbacks = vec![Fallback::SameOperator];
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        let view = server_sees(&snapshot, fixtures::climbing_crew())?;
        assert_eq!(view.configuration.overlaps, []);
        assert_eq!(view.configuration.wake_pushes, WakePushes::NoWakePushes);
        assert_eq!(view.privacy, PrivacyLevel::Full);
        Ok(())
    }

    #[test]
    fn fallbacks_and_status_reach_the_configuration() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.fallbacks = vec![Fallback::PrivacyPassOnly];
        snapshot.relay = RelayStatus::KeyMismatch;
        for party in &mut snapshot.parties {
            party.config.push_relay.kind = PushRelayKind::Custom(Host::new("up.example")?);
            party.config.post_quantum = PostQuantum::On;
        }
        let configuration = server_sees(&snapshot, fixtures::climbing_crew())?.configuration;
        assert_eq!(configuration.proofs, Proofs::PrivacyPassOnly);
        assert_eq!(configuration.relay, RelayStatus::KeyMismatch);
        assert_eq!(configuration.push_relay, PushRelaySource::Custom);
        assert_eq!(configuration.post_quantum, PostQuantum::On);
        assert_eq!(
            server_sees(&snapshot, pingers_proto::PartyId::from_bytes([9; 16])),
            Err(ScreenError::UnknownParty)
        );
        Ok(())
    }
}
