// SPDX-License-Identifier: AGPL-3.0-only
//! Moves per-Party state this crate kept as key-value records (under `pingers-parties/v1/`)
//! into pingers-store's tables (schema version 5): settings, roster history, members'
//! subscriptions, the in-flight commit and a waiting profile. Invite and join records stay
//! records. The decoders here exist only to read those old records.

use pingers_mls::{MlsStorage as _, Roster, RosterEntry};
use pingers_proto::{Epoch, LeafIndex, PartyId, SubscriptionId};
use pingers_store::Session;

use crate::codec::Reader;
use crate::records::party_record_key;
use crate::settings::StoredSettings;
use crate::{PartiesError, PartyLifecycle};

const SETTINGS: &[u8] = b"settings";
const EPOCHS: &[u8] = b"epochs";
const ROSTER: &[u8] = b"roster/";
const PENDING_COMMIT: &[u8] = b"pending-commit";
const SUBSCRIPTIONS: &[u8] = b"subscriptions";
const PROFILE_WAITING: &[u8] = b"profile-waiting";

impl PartyLifecycle<'_> {
    /// Moves any per-Party state still kept in this crate's old records into pingers-store's
    /// tables and deletes the records, one transaction per Party. Running it again changes
    /// nothing. Call once when the database is opened, before other Party operations.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Store`] or [`PartiesError::Records`]; [`PartiesError::Malformed`] if an
    /// old record does not decode (that Party's transaction is rolled back).
    pub fn upgrade_records(&self) -> Result<(), PartiesError> {
        for row in self.session.parties().list()? {
            self.session
                .transaction(|session| upgrade_party(session, &row.id))?;
        }
        Ok(())
    }
}

fn take(session: &Session, key: &[u8]) -> Result<Option<Vec<u8>>, PartiesError> {
    let Some(value) = session.read(key)? else {
        return Ok(None);
    };
    session.delete(key)?;
    Ok(Some(value.to_vec()))
}

fn upgrade_party(session: &Session, party: &PartyId) -> Result<(), PartiesError> {
    let key = |suffix: &[u8]| party_record_key(party, suffix);
    if let Some(bytes) = take(session, &key(SETTINGS))? {
        StoredSettings::decode(&bytes)?.write(session, party)?;
    }
    if let Some(bytes) = take(session, &key(EPOCHS))? {
        // Oldest first, so the newest ends up newest in the table too.
        for epoch in decode_epochs(&bytes)?.into_iter().rev() {
            let mut roster_key = key(ROSTER);
            roster_key.extend_from_slice(&epoch.get().to_be_bytes());
            if let Some(bytes) = take(session, &roster_key)? {
                session
                    .rosters()
                    .record(party, epoch, &decode_roster(&bytes)?)?;
            }
        }
    }
    if let Some(bytes) = take(session, &key(PENDING_COMMIT))? {
        // The same encoding: the table holds it opaquely.
        session.pending_commits().put(party, &bytes)?;
    }
    if let Some(bytes) = take(session, &key(SUBSCRIPTIONS))? {
        let mut reader = Reader::new(&bytes, "subscriptions");
        for _ in 0..reader.count()? {
            let member = reader.key()?;
            let subscription = SubscriptionId::from_bytes(reader.fixed()?);
            session
                .member_subscriptions()
                .set(party, &member, Some(subscription))?;
        }
        reader.end()?;
    }
    if take(session, &key(PROFILE_WAITING))?.is_some() {
        session.profiles_waiting().mark(party)?;
    }
    Ok(())
}

fn decode_epochs(bytes: &[u8]) -> Result<Vec<Epoch>, PartiesError> {
    let mut reader = Reader::new(bytes, "epoch list");
    let mut epochs = Vec::new();
    for _ in 0..reader.count()? {
        epochs.push(Epoch::new(reader.u64()?));
    }
    reader.end()?;
    Ok(epochs)
}

fn decode_roster(bytes: &[u8]) -> Result<Roster, PartiesError> {
    let mut reader = Reader::new(bytes, "roster");
    let mut entries = Vec::new();
    for _ in 0..reader.count()? {
        entries.push(RosterEntry {
            leaf: LeafIndex::new(reader.u32()?),
            signature_key: reader.key()?,
        });
    }
    reader.end()?;
    Ok(Roster::new(entries))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_crypto::AccountPublicKey;
    use pingers_proto::{GroupId, ServerId};
    use pingers_store::{Party, PartyName};
    use pingers_time::UnixSeconds;

    use super::*;
    use crate::codec::Writer;
    use crate::settings::{PostQuantum, RelayChoice};
    use crate::test_support::{Device, TestError};

    fn write(session: &Session, key: &[u8], value: &[u8]) -> Result<(), TestError> {
        session.write(key, value).map_err(PartiesError::from)?;
        Ok(())
    }

    #[test]
    fn old_records_move_into_the_tables_once() -> Result<(), TestError> {
        let device = Device::new(1)?;
        let session = &device.session;
        let party = PartyId::from_bytes([4; 16]);
        session.parties().insert(&Party {
            id: party,
            group: GroupId::from_bytes([5; 32]),
            name: PartyName::new("Friends".into())?,
            own_leaf: LeafIndex::new(0),
            created_at: UnixSeconds::new(1),
            location_visibility: None,
        })?;
        let key = |suffix: &[u8]| party_record_key(&party, suffix);
        let settings = StoredSettings {
            server: ServerId::new("https://pingers.example".into())?,
            relays: RelayChoice::ServerDefault,
            post_quantum: PostQuantum::QuantumResistant,
        };
        write(session, &key(SETTINGS), &settings.encode()?)?;
        let (alice, bob) = (
            AccountPublicKey::from_bytes([1; 32]),
            AccountPublicKey::from_bytes([2; 32]),
        );
        let mut epochs = Writer::new();
        epochs.count(2)?;
        for (epoch, keys) in [(3_u64, vec![alice, bob]), (2, vec![alice])] {
            epochs.u64(epoch);
            let mut roster = Writer::new();
            roster.count(keys.len())?;
            for (leaf, member) in (0_u32..).zip(&keys) {
                roster.u32(leaf);
                roster.key(member);
            }
            let mut roster_key = key(ROSTER);
            roster_key.extend_from_slice(&epoch.to_be_bytes());
            write(session, &roster_key, &roster.finish())?;
        }
        write(session, &key(EPOCHS), &epochs.finish())?;
        write(session, &key(PENDING_COMMIT), &[3])?;
        let mut subscriptions = Writer::new();
        subscriptions.count(1)?;
        subscriptions.key(&bob);
        subscriptions.fixed(&[9; 16]);
        write(session, &key(SUBSCRIPTIONS), &subscriptions.finish())?;
        write(session, &key(PROFILE_WAITING), &[1])?;

        let lifecycle = device.parties();
        lifecycle.upgrade_records()?;
        let check = || -> Result<(), TestError> {
            let moved = lifecycle.settings(&party)?;
            assert_eq!(moved.server.as_str(), "https://pingers.example");
            assert_eq!(moved.post_quantum, PostQuantum::QuantumResistant);
            assert_eq!(
                session.rosters().epochs(&party)?,
                vec![Epoch::new(3), Epoch::new(2)]
            );
            assert_eq!(
                session
                    .rosters()
                    .at(&party, Epoch::new(3))?
                    .map(|roster| roster.entries().len()),
                Some(2)
            );
            assert_eq!(session.pending_commits().get(&party)?, Some(vec![3]));
            assert_eq!(
                session.member_subscriptions().get(&party, &bob)?,
                Some(SubscriptionId::from_bytes([9; 16]))
            );
            assert!(session.profiles_waiting().is_waiting(&party)?);
            for suffix in [
                SETTINGS,
                EPOCHS,
                PENDING_COMMIT,
                SUBSCRIPTIONS,
                PROFILE_WAITING,
            ] {
                assert!(
                    session
                        .read(&key(suffix))
                        .map_err(PartiesError::from)?
                        .is_none()
                );
            }
            Ok(())
        };
        check()?;
        // A second run finds nothing to move and changes nothing.
        lifecycle.upgrade_records()?;
        check()?;
        Ok(())
    }

    #[test]
    fn a_corrupt_old_record_rolls_its_party_back() -> Result<(), TestError> {
        let device = Device::new(1)?;
        let session = &device.session;
        let party = PartyId::from_bytes([4; 16]);
        session.parties().insert(&Party {
            id: party,
            group: GroupId::from_bytes([5; 32]),
            name: PartyName::new("Friends".into())?,
            own_leaf: LeafIndex::new(0),
            created_at: UnixSeconds::new(1),
            location_visibility: None,
        })?;
        write(session, &party_record_key(&party, PROFILE_WAITING), &[1])?;
        write(
            session,
            &party_record_key(&party, SUBSCRIPTIONS),
            &[0, 0, 0, 1],
        )?;
        assert!(matches!(
            device.parties().upgrade_records(),
            Err(PartiesError::Malformed { .. })
        ));
        assert!(!session.profiles_waiting().is_waiting(&party)?);
        assert!(
            session
                .read(&party_record_key(&party, SUBSCRIPTIONS))
                .map_err(PartiesError::from)?
                .is_some()
        );
        Ok(())
    }
}
