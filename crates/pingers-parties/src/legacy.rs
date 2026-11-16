// SPDX-License-Identifier: AGPL-3.0-only
//! Moves the state this crate kept as key-value records (under `pingers-parties/v1/`) into
//! pingers-store's tables: settings, roster history, members' subscriptions, the in-flight
//! commit and a waiting profile (schema version 6); invites, waiting join requests, how this
//! device joined a Party, and joins in progress (schema version 7). The key layout and the
//! decoders here exist only to read those old records.

use pingers_crypto::{InviteSecret, JoinCode};
use pingers_mls::{MlsStorage as _, Roster, RosterEntry};
use pingers_proto::{
    DisplayName, Epoch, InvitePayload, KeyPackageBytes, LeafIndex, MailboxId, PartyId,
    SequenceNumber, SubscriptionId, WireDecode as _,
};
use pingers_store::{InviteState, JoinedVia, PartyInvite, PartyJoin, Session};
use pingers_time::UnixSeconds;
use zeroize::Zeroizing;

use crate::codec::Reader;
use crate::joining::read_approval;
use crate::settings::StoredSettings;
use crate::{PartiesError, PartyLifecycle};

const PREFIX: &[u8] = b"pingers-parties/v1/";

// Per-Party records, under `party/<id>/`.
const SETTINGS: &[u8] = b"settings";
const EPOCHS: &[u8] = b"epochs";
const ROSTER: &[u8] = b"roster/";
const PENDING_COMMIT: &[u8] = b"pending-commit";
const SUBSCRIPTIONS: &[u8] = b"subscriptions";
const PROFILE_WAITING: &[u8] = b"profile-waiting";
const INVITES: &[u8] = b"invites";
const INVITE: &[u8] = b"invite/";
const APPROVALS: &[u8] = b"approvals";
const APPROVAL: &[u8] = b"approval/";
const JOINED_VIA: &[u8] = b"joined-via";

// Records outside any Party.
const INVITE_INDEX: &[u8] = b"invite-index/";
const JOINING: &[u8] = b"joining/";

/// The old invite record's format version.
const INVITE_VERSION: u8 = 2;

impl PartyLifecycle<'_> {
    /// Moves any state still kept in this crate's old records into pingers-store's tables and
    /// deletes the records: one transaction per Party, then one for joins in progress. Running
    /// it again changes nothing. Call once when the database is opened, before other Party
    /// operations.
    ///
    /// # Errors
    ///
    /// [`PartiesError::Store`] or [`PartiesError::Records`]; [`PartiesError::Malformed`] if an
    /// old record does not decode (that transaction is rolled back).
    pub fn upgrade_records(&self) -> Result<(), PartiesError> {
        for row in self.session.parties().list()? {
            self.session
                .transaction(|session| upgrade_party(session, &row.id))?;
        }
        self.session.transaction(upgrade_joins)
    }
}

/// The full key of a Party record under `suffix`.
fn party_record_key(party: &PartyId, suffix: &[u8]) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    key.extend_from_slice(b"party/");
    key.extend_from_slice(party.as_bytes());
    key.push(b'/');
    key.extend_from_slice(suffix);
    key
}

/// The full key of a record outside any Party, under `kind` and an id.
fn record_key(kind: &[u8], id: &[u8]) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    key.extend_from_slice(kind);
    key.extend_from_slice(id);
    key
}

fn take(session: &Session, key: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, PartiesError> {
    let Some(value) = session.read(key)? else {
        return Ok(None);
    };
    session.delete(key)?;
    Ok(Some(value))
}

fn upgrade_party(session: &Session, party: &PartyId) -> Result<(), PartiesError> {
    let key = |suffix: &[u8]| party_record_key(party, suffix);
    let keyed = |suffix: &[u8], id: &[u8]| {
        let mut key = key(suffix);
        key.extend_from_slice(id);
        key
    };
    if let Some(bytes) = take(session, &key(SETTINGS))? {
        // The Party's server is its row's since the store moved every Party onto a server
        // (schema version 10); the old record's copy is not trusted to be spelt the same.
        let mut settings = StoredSettings::decode(&bytes)?;
        settings.server = crate::lifecycle::party_row(session, party)?.server;
        settings.write(session, party)?;
    }
    if let Some(bytes) = take(session, &key(EPOCHS))? {
        // Oldest first, so the newest ends up newest in the table too.
        for epoch in decode_epochs(&bytes)?.into_iter().rev() {
            if let Some(bytes) = take(session, &keyed(ROSTER, &epoch.get().to_be_bytes()))? {
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
    // Invites before the requests that came through them.
    if let Some(bytes) = take(session, &key(INVITES))? {
        for mailbox in decode_mailbox_list(&bytes)? {
            session.delete(&record_key(INVITE_INDEX, mailbox.as_bytes()))?;
            if let Some(bytes) = take(session, &keyed(INVITE, mailbox.as_bytes()))? {
                session
                    .party_invites()
                    .insert(&decode_invite(*party, mailbox, &bytes)?)?;
            }
        }
    }
    if let Some(bytes) = take(session, &key(APPROVALS))? {
        for reply in decode_mailbox_list(&bytes)? {
            let Some(bytes) = take(session, &keyed(APPROVAL, reply.as_bytes()))? else {
                continue;
            };
            let mut reader = Reader::new(&bytes, "join approval");
            let approval = read_approval(&mut reader)?;
            reader.end()?;
            // A request whose invite is gone could never be let in.
            if session.party_invites().get(&approval.invite)?.is_some() {
                session.join_approvals().put(party, &approval)?;
            }
        }
    }
    if let Some(bytes) = take(session, &key(JOINED_VIA))? {
        session.joined_via().put(party, decode_via(&bytes)?)?;
    }
    Ok(())
}

fn upgrade_joins(session: &Session) -> Result<(), PartiesError> {
    let prefix = record_key(JOINING, &[]);
    for key in session.kv_keys_with_prefix(&prefix)? {
        let mut reader = Reader::new(key.get(prefix.len()..).unwrap_or_default(), "join key");
        let reply = MailboxId::from_bytes(reader.fixed()?);
        reader.end()?;
        if let Some(bytes) = take(session, &key)?
            && let Some(join) = decode_joining(reply, &bytes)?
        {
            session.party_joins().put(&join)?;
        }
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

fn decode_mailbox_list(bytes: &[u8]) -> Result<Vec<MailboxId>, PartiesError> {
    let mut reader = Reader::new(bytes, "mailbox list");
    let mut list = Vec::new();
    for _ in 0..reader.count()? {
        list.push(MailboxId::from_bytes(reader.fixed()?));
    }
    reader.end()?;
    Ok(list)
}

fn decode_invite(
    party: PartyId,
    mailbox: MailboxId,
    bytes: &[u8],
) -> Result<PartyInvite, PartiesError> {
    let mut reader = Reader::new(bytes, "invite record");
    if reader.byte()? != INVITE_VERSION {
        return Err(reader.malformed());
    }
    let secret = InviteSecret::from_bytes(Zeroizing::new(reader.fixed()?));
    let shared_at = UnixSeconds::new(reader.u64()?);
    let expires = UnixSeconds::new(reader.u64()?);
    let state = match reader.byte()? {
        0 => InviteState::Open,
        1 => InviteState::Used {
            admitted: MailboxId::from_bytes(reader.fixed()?),
        },
        2 => InviteState::Revoked,
        _ => return Err(reader.malformed()),
    };
    let read_position = match reader.byte()? {
        0 => None,
        1 => Some(SequenceNumber::new(reader.u64()?)),
        _ => return Err(reader.malformed()),
    };
    reader.end()?;
    Ok(PartyInvite {
        mailbox,
        party,
        secret,
        shared_at,
        expires,
        state,
        read_position,
    })
}

fn decode_via(bytes: &[u8]) -> Result<JoinedVia, PartiesError> {
    let mut reader = Reader::new(bytes, "joined via");
    let inviter = LeafIndex::new(reader.u32()?);
    let code = JoinCode::from_value(reader.u64()?).ok_or_else(|| reader.malformed())?;
    reader.end()?;
    Ok(JoinedVia { inviter, code })
}

/// An old join record, or `None` for one whose link is in the layout from before links carried
/// the server's key pins (pinning its server blindly is what pins exist to avoid): that join is
/// dropped, and the joiner opens a new link.
fn decode_joining(reply: MailboxId, bytes: &[u8]) -> Result<Option<PartyJoin>, PartiesError> {
    let mut reader = Reader::new(bytes, "join record");
    let Ok(payload) = InvitePayload::decode(reader.variable()?) else {
        return Ok(None);
    };
    let name = DisplayName::new(reader.text()?)?;
    let subscription = match reader.byte()? {
        0 => None,
        1 => Some(SubscriptionId::from_bytes(reader.fixed()?)),
        _ => return Err(reader.malformed()),
    };
    let key_package = KeyPackageBytes::from_bytes(reader.variable()?.to_vec());
    let quantum_resistant = match reader.byte()? {
        0 => None,
        1 => Some(KeyPackageBytes::from_bytes(reader.variable()?.to_vec())),
        _ => return Err(reader.malformed()),
    };
    let asked_at = UnixSeconds::new(reader.u64()?);
    reader.end()?;
    Ok(Some(PartyJoin {
        reply,
        payload,
        name,
        subscription,
        key_package,
        quantum_resistant,
        asked_at,
    }))
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
            server: ServerId::new(crate::test_support::SERVER.into())?,
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
            server: ServerId::new(crate::test_support::SERVER.into())?,
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

    fn friends(session: &Session, party: PartyId) -> Result<(), TestError> {
        session.parties().insert(&Party {
            id: party,
            group: GroupId::from_bytes([5; 32]),
            name: PartyName::new("Friends".into())?,
            own_leaf: LeafIndex::new(0),
            created_at: UnixSeconds::new(1),
            location_visibility: None,
            server: ServerId::new(crate::test_support::SERVER.into())?,
        })?;
        Ok(())
    }

    fn mailbox_list(list: &[MailboxId]) -> Result<Vec<u8>, TestError> {
        let mut writer = Writer::new();
        writer.count(list.len())?;
        for mailbox in list {
            writer.fixed(mailbox.as_bytes());
        }
        Ok(writer.finish())
    }

    fn old_invite(secret: u8, used_by: Option<MailboxId>, read: Option<u64>) -> Vec<u8> {
        let mut writer = Writer::new();
        writer.byte(INVITE_VERSION);
        writer.fixed(&[secret; 32]);
        writer.u64(10);
        writer.u64(20);
        match used_by {
            None => writer.byte(0),
            Some(reply) => {
                writer.byte(1);
                writer.fixed(reply.as_bytes());
            }
        }
        match read {
            None => writer.byte(0),
            Some(position) => {
                writer.byte(1);
                writer.u64(position);
            }
        }
        writer.finish()
    }

    /// An old request from `reply` through `invite`, as its record held it.
    fn old_approval(reply: MailboxId, invite: MailboxId) -> Result<Vec<u8>, TestError> {
        let approval = crate::joining::Approval {
            invite,
            asked_at: UnixSeconds::new(15),
            request: pingers_proto::JoinRequest {
                key_package: KeyPackageBytes::from_bytes(vec![1; 3]),
                quantum_resistant_key_package: None,
                display_name: DisplayName::new("Ben".into())?,
                reply_mailbox: reply,
                party_subscription: None,
            },
        };
        Ok(crate::joining::encode_approval(&approval)?)
    }

    /// Writes old invite records for `party`, with their list and index entries.
    fn write_old_invites(
        session: &Session,
        party: &PartyId,
        invites: &[(MailboxId, Vec<u8>)],
    ) -> Result<(), TestError> {
        let mailboxes: Vec<MailboxId> = invites.iter().map(|(mailbox, _)| *mailbox).collect();
        write(
            session,
            &party_record_key(party, INVITES),
            &mailbox_list(&mailboxes)?,
        )?;
        for (mailbox, record) in invites {
            let mut key = party_record_key(party, INVITE);
            key.extend_from_slice(mailbox.as_bytes());
            write(session, &key, record)?;
            write(
                session,
                &record_key(INVITE_INDEX, mailbox.as_bytes()),
                party.as_bytes(),
            )?;
        }
        Ok(())
    }

    #[test]
    fn old_invite_and_join_records_move_into_the_tables_once() -> Result<(), TestError> {
        let device = Device::new(1)?;
        let session = &device.session;
        let party = PartyId::from_bytes([4; 16]);
        friends(session, party)?;
        let key = |suffix: &[u8], id: &[u8]| {
            let mut key = party_record_key(&party, suffix);
            key.extend_from_slice(id);
            key
        };
        let (open, used, gone) = (
            MailboxId::from_bytes([6; 16]),
            MailboxId::from_bytes([7; 16]),
            MailboxId::from_bytes([8; 16]),
        );
        let (ben, cy) = (
            MailboxId::from_bytes([9; 16]),
            MailboxId::from_bytes([10; 16]),
        );
        let cancelled = MailboxId::from_bytes([11; 16]);
        let mut revoked = old_invite(3, None, None);
        // The status byte, after the version, the secret and two times.
        if let Some(status) = revoked.get_mut(49) {
            *status = 2;
        }
        write_old_invites(
            session,
            &party,
            &[
                (open, old_invite(1, None, Some(3))),
                (used, old_invite(2, Some(ben), None)),
                (cancelled, revoked),
            ],
        )?;
        // Cy's request came through an invite whose record is gone: it is dropped.
        let approvals = [(ben, open), (cy, gone)];
        let replies: Vec<MailboxId> = approvals.iter().map(|(reply, _)| *reply).collect();
        write(session, &key(APPROVALS, &[]), &mailbox_list(&replies)?)?;
        for (reply, invite) in approvals {
            write(
                session,
                &key(APPROVAL, reply.as_bytes()),
                &old_approval(reply, invite)?,
            )?;
        }
        let mut via = Writer::new();
        via.u32(2);
        via.u64(123_456_789_012);
        write(session, &key(JOINED_VIA, &[]), &via.finish())?;

        let lifecycle = device.parties();
        lifecycle.upgrade_records()?;
        let check = || -> Result<(), TestError> {
            let invites = session.party_invites().list(&party)?;
            let states: Vec<(MailboxId, InviteState, Option<u64>)> = invites
                .iter()
                .map(|invite| {
                    (
                        invite.mailbox,
                        invite.state,
                        invite.read_position.map(SequenceNumber::get),
                    )
                })
                .collect();
            assert_eq!(
                states,
                vec![
                    (open, InviteState::Open, Some(3)),
                    (used, InviteState::Used { admitted: ben }, None),
                    (cancelled, InviteState::Revoked, None),
                ]
            );
            assert_eq!(
                invites.first().map(|invite| *invite.secret.expose_bytes()),
                Some([1; 32])
            );
            let waiting = session.join_approvals().list(&party)?;
            assert_eq!(waiting.len(), 1);
            assert_eq!(
                waiting
                    .first()
                    .map(|approval| approval.request.reply_mailbox),
                Some(ben)
            );
            assert_eq!(
                session
                    .joined_via()
                    .get(&party)?
                    .map(|via| (via.inviter, via.code.value())),
                Some((LeafIndex::new(2), 123_456_789_012))
            );
            assert!(session.kv_keys_with_prefix(PREFIX)?.is_empty());
            Ok(())
        };
        check()?;
        lifecycle.upgrade_records()?;
        check()?;
        Ok(())
    }

    #[test]
    fn an_old_join_whose_link_carries_no_key_pins_is_dropped() -> Result<(), TestError> {
        let device = Device::new(1)?;
        let session = &device.session;
        let reply = MailboxId::from_bytes([10; 16]);
        // A link payload in no layout this build reads, as one from before key pins is.
        let mut joining = Writer::new();
        joining.variable(&[0x80])?;
        joining.variable(b"Di")?;
        joining.byte(0);
        joining.variable(&[6; 4])?;
        joining.byte(0);
        joining.u64(30);
        write(
            session,
            &record_key(JOINING, reply.as_bytes()),
            &joining.finish(),
        )?;
        device.parties().upgrade_records()?;
        assert_eq!(session.party_joins().get(&reply)?, None);
        assert!(session.kv_keys_with_prefix(PREFIX)?.is_empty());
        Ok(())
    }

    #[test]
    fn an_old_join_in_progress_moves_into_its_table_once() -> Result<(), TestError> {
        use pingers_proto::{InviterFingerprint, SecretBytes, ServerId, WireEncode as _};

        let device = Device::new(1)?;
        let session = &device.session;
        let reply = MailboxId::from_bytes([10; 16]);
        let payload = InvitePayload {
            server: ServerId::new("https://pingers.example".into())?,
            invite_mailbox: MailboxId::from_bytes([6; 16]),
            invite_secret: SecretBytes::new([3; 32]),
            inviter_fingerprint: InviterFingerprint::from_bytes([4; 16]),
            expires: UnixSeconds::new(900),
            gateway_pins: vec![pingers_proto::KeyConfigHash::from_bytes([8; 32])],
            push_relay_pins: Vec::new(),
            map_host_pins: Vec::new(),
            registration_code: None,
        };
        let mut joining = Writer::new();
        joining.variable(&payload.encode()?)?;
        joining.variable(b"Di")?;
        joining.byte(1);
        joining.fixed(&[5; 16]);
        joining.variable(&[6; 4])?;
        joining.byte(1);
        joining.variable(&[7; 5])?;
        joining.u64(30);
        write(
            session,
            &record_key(JOINING, reply.as_bytes()),
            &joining.finish(),
        )?;
        // A join made without wake-up pushes, on a build without X-Wing.
        let bare = MailboxId::from_bytes([11; 16]);
        let mut joining = Writer::new();
        joining.variable(&payload.encode()?)?;
        joining.variable(b"Ed")?;
        joining.byte(0);
        joining.variable(&[6; 4])?;
        joining.byte(0);
        joining.u64(31);
        write(
            session,
            &record_key(JOINING, bare.as_bytes()),
            &joining.finish(),
        )?;
        let lifecycle = device.parties();
        lifecycle.upgrade_records()?;
        let check = || -> Result<(), TestError> {
            let join = session
                .party_joins()
                .get(&reply)?
                .ok_or(PartiesError::UnknownJoin)?;
            assert_eq!(join.name.as_str(), "Di");
            assert_eq!(join.payload, payload);
            assert_eq!(join.subscription, Some(SubscriptionId::from_bytes([5; 16])));
            assert_eq!(
                join.quantum_resistant,
                Some(KeyPackageBytes::from_bytes(vec![7; 5]))
            );
            let bare = session
                .party_joins()
                .get(&bare)?
                .ok_or(PartiesError::UnknownJoin)?;
            assert_eq!(bare.subscription, None);
            assert_eq!(bare.quantum_resistant, None);
            assert!(session.kv_keys_with_prefix(PREFIX)?.is_empty());
            Ok(())
        };
        check()?;
        lifecycle.upgrade_records()?;
        check()?;
        Ok(())
    }

    #[test]
    fn a_corrupt_old_invite_rolls_its_party_back() -> Result<(), TestError> {
        let device = Device::new(1)?;
        let session = &device.session;
        let party = PartyId::from_bytes([4; 16]);
        friends(session, party)?;
        let mailbox = MailboxId::from_bytes([6; 16]);
        let mut invite_key = party_record_key(&party, INVITE);
        invite_key.extend_from_slice(mailbox.as_bytes());
        let mut wrong_version = old_invite(1, None, None);
        if let Some(version) = wrong_version.first_mut() {
            *version = INVITE_VERSION.saturating_add(1);
        }
        write(
            session,
            &party_record_key(&party, INVITES),
            &mailbox_list(&[mailbox])?,
        )?;
        write(session, &invite_key, &wrong_version)?;
        assert!(matches!(
            device.parties().upgrade_records(),
            Err(PartiesError::Malformed { .. })
        ));
        assert!(session.party_invites().list(&party)?.is_empty());
        assert!(
            session
                .read(&invite_key)
                .map_err(PartiesError::from)?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn a_join_key_that_names_no_mailbox_is_malformed() -> Result<(), TestError> {
        let device = Device::new(1)?;
        write(&device.session, &record_key(JOINING, &[1; 15]), &[0])?;
        assert!(matches!(
            device.parties().upgrade_records(),
            Err(PartiesError::Malformed { .. })
        ));
        Ok(())
    }
}
