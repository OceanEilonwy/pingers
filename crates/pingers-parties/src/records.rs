// SPDX-License-Identifier: AGPL-3.0-only
//! This crate's invite and join records, kept in the store's encrypted key-value table, and
//! the roster history over pingers-store's `roster_epoch` and `roster_entry` tables.
//!
//! Interim: pingers-store has no tables for invites, waiting join requests and joins in
//! progress yet (join flow A is still changing them). Until it does, they are written through
//! the store's `MlsStorage` implementation on `Session`, under the prefix `pingers-parties/v1/`,
//! which is disjoint from pingers-mls's `openmls/v1/`. They join the session's transaction
//! exactly as MLS state does, so a commit and its rows land together. The trait cannot list
//! keys, so each collection keeps its own index record. The other per-Party state (settings,
//! rosters, subscriptions, the in-flight commit, a waiting profile) is in store tables; older
//! records of those kinds are moved there by `PartyLifecycle::upgrade_records`.

use pingers_mls::{MlsStorage as _, Roster};
use pingers_proto::{Epoch, MailboxId, PartyId};
use pingers_store::Session;
use zeroize::Zeroizing;

use crate::PartiesError;
use crate::codec::{Reader, Writer};

const PREFIX: &[u8] = b"pingers-parties/v1/";

/// A record's key.
pub(crate) enum Key<'id> {
    /// The Party's invites (index of mailbox ids).
    Invites(&'id PartyId),
    /// One invite, by its mailbox.
    Invite(&'id PartyId, &'id MailboxId),
    /// Join requests waiting for approval (index of reply mailbox ids).
    Approvals(&'id PartyId),
    /// One waiting join request, by its reply mailbox.
    Approval(&'id PartyId, &'id MailboxId),
    /// Which Party an invite mailbox belongs to.
    InviteIndex(&'id MailboxId),
    /// A join this device started, by its reply mailbox.
    Joining(&'id MailboxId),
    /// Who let this device into a Party it joined, and the code that was checked.
    JoinedVia(&'id PartyId),
}

impl Key<'_> {
    fn bytes(&self) -> Vec<u8> {
        let mut key = PREFIX.to_vec();
        match self {
            Self::Invites(party) => party_key(&mut key, party, b"invites"),
            Self::Invite(party, mailbox) => {
                party_key(&mut key, party, b"invite/");
                key.extend_from_slice(mailbox.as_bytes());
            }
            Self::Approvals(party) => party_key(&mut key, party, b"approvals"),
            Self::Approval(party, mailbox) => {
                party_key(&mut key, party, b"approval/");
                key.extend_from_slice(mailbox.as_bytes());
            }
            Self::JoinedVia(party) => party_key(&mut key, party, b"joined-via"),
            Self::InviteIndex(mailbox) => {
                key.extend_from_slice(b"invite-index/");
                key.extend_from_slice(mailbox.as_bytes());
            }
            Self::Joining(mailbox) => {
                key.extend_from_slice(b"joining/");
                key.extend_from_slice(mailbox.as_bytes());
            }
        }
        key
    }
}

fn party_key(key: &mut Vec<u8>, party: &PartyId, suffix: &[u8]) {
    key.extend_from_slice(b"party/");
    key.extend_from_slice(party.as_bytes());
    key.push(b'/');
    key.extend_from_slice(suffix);
}

/// The full key of a Party record under `suffix`, for the records `legacy` moves into tables.
pub(crate) fn party_record_key(party: &PartyId, suffix: &[u8]) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    party_key(&mut key, party, suffix);
    key
}

fn party_prefix(party: &PartyId) -> Vec<u8> {
    let mut key = PREFIX.to_vec();
    party_key(&mut key, party, b"");
    key
}

pub(crate) fn read(
    session: &Session,
    key: &Key<'_>,
) -> Result<Option<Zeroizing<Vec<u8>>>, PartiesError> {
    Ok(session.read(&key.bytes())?)
}

pub(crate) fn write(session: &Session, key: &Key<'_>, value: &[u8]) -> Result<(), PartiesError> {
    Ok(session.write(&key.bytes(), value)?)
}

pub(crate) fn delete(session: &Session, key: &Key<'_>) -> Result<(), PartiesError> {
    Ok(session.delete(&key.bytes())?)
}

/// Deletes every record of a Party, and the invite index entries that point at it.
pub(crate) fn delete_party(session: &Session, party: &PartyId) -> Result<(), PartiesError> {
    for mailbox in read_mailbox_list(session, &Key::Invites(party))? {
        delete(session, &Key::InviteIndex(&mailbox))?;
    }
    Ok(session.delete_prefix(&party_prefix(party))?)
}

// --- mailbox lists (index records) ---

pub(crate) fn read_mailbox_list(
    session: &Session,
    key: &Key<'_>,
) -> Result<Vec<MailboxId>, PartiesError> {
    let Some(bytes) = read(session, key)? else {
        return Ok(Vec::new());
    };
    let mut reader = Reader::new(&bytes, "mailbox list");
    let count = reader.count()?;
    let mut list = Vec::new();
    for _ in 0..count {
        list.push(MailboxId::from_bytes(reader.fixed()?));
    }
    reader.end()?;
    Ok(list)
}

fn write_mailbox_list(
    session: &Session,
    key: &Key<'_>,
    list: &[MailboxId],
) -> Result<(), PartiesError> {
    if list.is_empty() {
        return delete(session, key);
    }
    let mut writer = Writer::new();
    writer.count(list.len())?;
    for mailbox in list {
        writer.fixed(mailbox.as_bytes());
    }
    write(session, key, &writer.finish())
}

pub(crate) fn add_to_list(
    session: &Session,
    key: &Key<'_>,
    mailbox: MailboxId,
) -> Result<(), PartiesError> {
    let mut list = read_mailbox_list(session, key)?;
    if !list.contains(&mailbox) {
        list.push(mailbox);
    }
    write_mailbox_list(session, key, &list)
}

pub(crate) fn remove_from_list(
    session: &Session,
    key: &Key<'_>,
    mailbox: &MailboxId,
) -> Result<(), PartiesError> {
    let mut list = read_mailbox_list(session, key)?;
    list.retain(|entry| entry != mailbox);
    write_mailbox_list(session, key, &list)
}

// --- roster history ---

/// How many epochs' rosters are kept: the current one and the one before it, which is all
/// knock verification can ask for (pingers-presence matches the previous epoch for one lease
/// after a Commit that was not a removal, design B.4.7).
const EPOCHS_KEPT: usize = 2;

/// The epochs whose rosters are kept, newest first.
pub(crate) fn epochs(session: &Session, party: &PartyId) -> Result<Vec<Epoch>, PartiesError> {
    Ok(session.rosters().epochs(party)?)
}

/// Records the roster at a new epoch and forgets rosters older than the previous epoch.
pub(crate) fn record_roster(
    session: &Session,
    party: &PartyId,
    epoch: Epoch,
    roster: &Roster,
) -> Result<(), PartiesError> {
    let rosters = session.rosters();
    rosters.record(party, epoch, roster)?;
    Ok(rosters.retain_newest(party, EPOCHS_KEPT)?)
}

pub(crate) fn roster_at(
    session: &Session,
    party: &PartyId,
    epoch: Epoch,
) -> Result<Option<Roster>, PartiesError> {
    Ok(session.rosters().at(party, epoch)?)
}
