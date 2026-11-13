// SPDX-License-Identifier: AGPL-3.0-only
//! Where mailboxes and their messages live (design C.1 `mailbox`, `mailbox_msg`).

use std::collections::HashMap;

use pingers_proto::{MailboxId, MailboxMessage, Padded, SealedPush, SequenceNumber};
use pingers_time::UnixSeconds;

use crate::StorageError;

/// A registered mailbox as the server keeps it. Nothing in it names an account or device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMailbox {
    /// The mailbox id.
    pub id: MailboxId,
    /// The sealed push to forward on a post; `None` if it never had one or the relay
    /// answered 410.
    pub push: Option<SealedPush>,
    /// When the mailbox and its messages are deleted.
    pub expires_at: UnixSeconds,
}

impl StoredMailbox {
    /// Whether the mailbox has expired at `now`.
    #[must_use]
    pub fn expired(&self, now: UnixSeconds) -> bool {
        now >= self.expires_at
    }
}

/// Whether a delete found something to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removal {
    /// It was stored and is gone.
    Removed,
    /// Nothing was stored under the id.
    Absent,
}

/// Mailbox persistence. pingers-mailbox holds the rules; implementations only store.
///
/// Messages belong to their mailbox: they are numbered from 1 per mailbox and deleted with it.
/// Implementations are synchronous; an async server calls them from `spawn_blocking`.
pub trait MailboxStorage {
    /// Stores a new mailbox.
    ///
    /// # Errors
    ///
    /// [`StorageError::Duplicate`] if a mailbox with this id is stored, expired or not.
    fn insert_mailbox(&mut self, mailbox: &StoredMailbox) -> Result<(), StorageError>;

    /// The mailbox with this id, expired or not.
    ///
    /// # Errors
    ///
    /// A backend error, or [`StorageError::Corrupt`] for an unreadable row.
    fn mailbox(&self, id: &MailboxId) -> Result<Option<StoredMailbox>, StorageError>;

    /// Deletes a mailbox and its messages.
    ///
    /// # Errors
    ///
    /// A backend error.
    fn delete_mailbox(&mut self, id: &MailboxId) -> Result<Removal, StorageError>;

    /// Forgets a mailbox's sealed push, keeping the mailbox and its messages.
    ///
    /// # Errors
    ///
    /// A backend error. Clearing an absent mailbox is not an error.
    fn clear_push(&mut self, id: &MailboxId) -> Result<(), StorageError>;

    /// Appends a message and returns its sequence number: one more than the last one given in
    /// this mailbox, starting at 1.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no such mailbox is stored;
    /// [`StorageError::SequenceExhausted`] if the numbers ran out.
    fn append_message(
        &mut self,
        id: &MailboxId,
        body: &Padded,
    ) -> Result<SequenceNumber, StorageError>;

    /// How many messages the mailbox holds.
    ///
    /// # Errors
    ///
    /// A backend error.
    fn message_count(&self, id: &MailboxId) -> Result<usize, StorageError>;

    /// The mailbox's messages after `after` (all of them for `None`), oldest first.
    ///
    /// # Errors
    ///
    /// A backend error, or [`StorageError::Corrupt`] for an unreadable row.
    fn messages_after(
        &self,
        id: &MailboxId,
        after: Option<SequenceNumber>,
    ) -> Result<Vec<MailboxMessage>, StorageError>;

    /// Deletes every mailbox, with its messages, that has expired at `now`.
    ///
    /// # Errors
    ///
    /// A backend error.
    fn delete_expired(&mut self, now: UnixSeconds) -> Result<(), StorageError>;
}

/// Mailboxes in memory: for tests, and for a server that accepts losing them on restart.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    mailboxes: HashMap<MailboxId, Entry>,
}

#[derive(Debug)]
struct Entry {
    mailbox: StoredMailbox,
    last_sequence: u64,
    messages: Vec<MailboxMessage>,
}

impl MemoryStorage {
    /// Empty storage.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl MailboxStorage for MemoryStorage {
    fn insert_mailbox(&mut self, mailbox: &StoredMailbox) -> Result<(), StorageError> {
        if self.mailboxes.contains_key(&mailbox.id) {
            return Err(StorageError::Duplicate);
        }
        self.mailboxes.insert(
            mailbox.id,
            Entry {
                mailbox: mailbox.clone(),
                last_sequence: 0,
                messages: Vec::new(),
            },
        );
        Ok(())
    }

    fn mailbox(&self, id: &MailboxId) -> Result<Option<StoredMailbox>, StorageError> {
        Ok(self.mailboxes.get(id).map(|entry| entry.mailbox.clone()))
    }

    fn delete_mailbox(&mut self, id: &MailboxId) -> Result<Removal, StorageError> {
        Ok(match self.mailboxes.remove(id) {
            Some(_) => Removal::Removed,
            None => Removal::Absent,
        })
    }

    fn clear_push(&mut self, id: &MailboxId) -> Result<(), StorageError> {
        if let Some(entry) = self.mailboxes.get_mut(id) {
            entry.mailbox.push = None;
        }
        Ok(())
    }

    fn append_message(
        &mut self,
        id: &MailboxId,
        body: &Padded,
    ) -> Result<SequenceNumber, StorageError> {
        let entry = self.mailboxes.get_mut(id).ok_or(StorageError::NotFound)?;
        let sequence = SequenceNumber::new(entry.last_sequence)
            .checked_next()
            .ok_or(StorageError::SequenceExhausted)?;
        entry.last_sequence = sequence.get();
        entry.messages.push(MailboxMessage {
            sequence,
            body: body.clone(),
        });
        Ok(sequence)
    }

    fn message_count(&self, id: &MailboxId) -> Result<usize, StorageError> {
        Ok(self
            .mailboxes
            .get(id)
            .map_or(0, |entry| entry.messages.len()))
    }

    fn messages_after(
        &self,
        id: &MailboxId,
        after: Option<SequenceNumber>,
    ) -> Result<Vec<MailboxMessage>, StorageError> {
        let Some(entry) = self.mailboxes.get(id) else {
            return Ok(Vec::new());
        };
        Ok(entry
            .messages
            .iter()
            .filter(|message| after.is_none_or(|after| message.sequence > after))
            .cloned()
            .collect())
    }

    fn delete_expired(&mut self, now: UnixSeconds) -> Result<(), StorageError> {
        self.mailboxes
            .retain(|_id, entry| !entry.mailbox.expired(now));
        Ok(())
    }
}

/// The behaviour every [`MailboxStorage`] must have, run against each implementation.
#[cfg(test)]
pub(crate) mod contract {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::{PushHandle, RelayId, RelayRef, SealedPushBlob, SizeClass, Urgency};

    use super::*;

    pub(crate) fn mailbox(fill: u8, expires_at: u64, push: Option<SealedPush>) -> StoredMailbox {
        StoredMailbox {
            id: MailboxId::from_bytes([fill; 16]),
            push,
            expires_at: UnixSeconds::new(expires_at),
        }
    }

    pub(crate) fn sealed_push(relay: RelayRef, urgency: Urgency) -> SealedPush {
        SealedPush {
            relay,
            blob: SealedPushBlob::from_bytes([0x5a; 320]),
            urgency,
            handle: PushHandle::from_bytes([0x11; 8]),
        }
    }

    pub(crate) fn body(fill: u8) -> Result<Padded, pingers_proto::ProtoError> {
        Padded::pad(&[fill; 10], SizeClass::Bytes1024)
    }

    /// Why a contract check failed outside an assertion.
    #[derive(Debug, thiserror::Error)]
    pub(crate) enum ContractError {
        #[error(transparent)]
        Storage(#[from] StorageError),
        #[error(transparent)]
        Proto(#[from] pingers_proto::ProtoError),
    }

    /// Runs every check against fresh storage from `make`.
    pub(crate) fn check_all<S: MailboxStorage>(
        make: impl Fn() -> Result<S, StorageError>,
    ) -> Result<(), ContractError> {
        stored_mailbox_reads_back(make()?)?;
        duplicate_id_is_refused(make()?)?;
        sequence_numbers_start_at_one_per_mailbox(make()?)?;
        messages_after_skips_what_was_read(make()?)?;
        deleting_a_mailbox_deletes_its_messages(make()?)?;
        clearing_the_push_keeps_the_messages(make()?)?;
        expiry_deletes_only_expired_mailboxes(make()?)?;
        append_to_an_absent_mailbox_is_refused(make()?)?;
        Ok(())
    }

    fn stored_mailbox_reads_back(mut storage: impl MailboxStorage) -> Result<(), ContractError> {
        let listed = mailbox(
            1,
            100,
            Some(sealed_push(
                RelayRef::Listed(RelayId::new(7)),
                Urgency::Quiet,
            )),
        );
        let custom = mailbox(
            2,
            200,
            Some(sealed_push(
                RelayRef::Custom {
                    url: pingers_proto::RelayUrl::new(String::from("https://relay.example"))?,
                    key_config_hash: pingers_proto::KeyConfigHash::from_bytes([3; 32]),
                },
                Urgency::Alert,
            )),
        );
        let silent = mailbox(3, 300, None);
        for stored in [&listed, &custom, &silent] {
            storage.insert_mailbox(stored)?;
        }
        assert_eq!(storage.mailbox(&listed.id)?, Some(listed));
        assert_eq!(storage.mailbox(&custom.id)?, Some(custom));
        assert_eq!(storage.mailbox(&silent.id)?, Some(silent));
        assert_eq!(storage.mailbox(&MailboxId::from_bytes([9; 16]))?, None);
        Ok(())
    }

    fn duplicate_id_is_refused(mut storage: impl MailboxStorage) -> Result<(), ContractError> {
        storage.insert_mailbox(&mailbox(1, 100, None))?;
        assert!(matches!(
            storage.insert_mailbox(&mailbox(1, 500, None)),
            Err(StorageError::Duplicate)
        ));
        assert_eq!(
            storage
                .mailbox(&MailboxId::from_bytes([1; 16]))?
                .map(|stored| stored.expires_at),
            Some(UnixSeconds::new(100))
        );
        Ok(())
    }

    fn sequence_numbers_start_at_one_per_mailbox(
        mut storage: impl MailboxStorage,
    ) -> Result<(), ContractError> {
        let first = mailbox(1, 100, None);
        let second = mailbox(2, 100, None);
        storage.insert_mailbox(&first)?;
        storage.insert_mailbox(&second)?;
        assert_eq!(
            storage.append_message(&first.id, &body(1)?)?,
            SequenceNumber::new(1)
        );
        assert_eq!(
            storage.append_message(&first.id, &body(2)?)?,
            SequenceNumber::new(2)
        );
        assert_eq!(
            storage.append_message(&second.id, &body(3)?)?,
            SequenceNumber::new(1)
        );
        assert_eq!(storage.message_count(&first.id)?, 2);
        assert_eq!(storage.message_count(&second.id)?, 1);
        Ok(())
    }

    fn messages_after_skips_what_was_read(
        mut storage: impl MailboxStorage,
    ) -> Result<(), ContractError> {
        let stored = mailbox(1, 100, None);
        storage.insert_mailbox(&stored)?;
        for fill in 1..=3 {
            storage.append_message(&stored.id, &body(fill)?)?;
        }
        let all = storage.messages_after(&stored.id, None)?;
        assert_eq!(
            all.iter()
                .map(|message| message.sequence.get())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            all.first().map(|message| message.body.clone()),
            Some(body(1)?)
        );
        let later = storage.messages_after(&stored.id, Some(SequenceNumber::new(2)))?;
        assert_eq!(
            later,
            vec![MailboxMessage {
                sequence: SequenceNumber::new(3),
                body: body(3)?,
            }]
        );
        assert!(
            storage
                .messages_after(&stored.id, Some(SequenceNumber::new(3)))?
                .is_empty()
        );
        Ok(())
    }

    fn deleting_a_mailbox_deletes_its_messages(
        mut storage: impl MailboxStorage,
    ) -> Result<(), ContractError> {
        let stored = mailbox(1, 100, None);
        storage.insert_mailbox(&stored)?;
        storage.append_message(&stored.id, &body(1)?)?;
        assert_eq!(storage.delete_mailbox(&stored.id)?, Removal::Removed);
        assert_eq!(storage.delete_mailbox(&stored.id)?, Removal::Absent);
        assert_eq!(storage.mailbox(&stored.id)?, None);
        assert_eq!(storage.message_count(&stored.id)?, 0);
        // A re-registered id starts afresh.
        storage.insert_mailbox(&stored)?;
        assert!(storage.messages_after(&stored.id, None)?.is_empty());
        assert_eq!(
            storage.append_message(&stored.id, &body(2)?)?,
            SequenceNumber::new(1)
        );
        Ok(())
    }

    fn clearing_the_push_keeps_the_messages(
        mut storage: impl MailboxStorage,
    ) -> Result<(), ContractError> {
        let stored = mailbox(
            1,
            100,
            Some(sealed_push(
                RelayRef::Listed(RelayId::new(1)),
                Urgency::Alert,
            )),
        );
        storage.insert_mailbox(&stored)?;
        storage.append_message(&stored.id, &body(1)?)?;
        storage.clear_push(&stored.id)?;
        storage.clear_push(&MailboxId::from_bytes([8; 16]))?;
        assert_eq!(
            storage.mailbox(&stored.id)?,
            Some(StoredMailbox {
                push: None,
                ..stored.clone()
            })
        );
        assert_eq!(storage.message_count(&stored.id)?, 1);
        Ok(())
    }

    fn expiry_deletes_only_expired_mailboxes(
        mut storage: impl MailboxStorage,
    ) -> Result<(), ContractError> {
        let early = mailbox(1, 100, None);
        let late = mailbox(2, 101, None);
        storage.insert_mailbox(&early)?;
        storage.insert_mailbox(&late)?;
        storage.append_message(&early.id, &body(1)?)?;
        storage.delete_expired(UnixSeconds::new(99))?;
        assert!(storage.mailbox(&early.id)?.is_some());
        storage.delete_expired(UnixSeconds::new(100))?;
        assert_eq!(storage.mailbox(&early.id)?, None);
        assert_eq!(storage.message_count(&early.id)?, 0);
        assert_eq!(storage.mailbox(&late.id)?, Some(late));
        Ok(())
    }

    fn append_to_an_absent_mailbox_is_refused(
        mut storage: impl MailboxStorage,
    ) -> Result<(), ContractError> {
        assert!(matches!(
            storage.append_message(&MailboxId::from_bytes([1; 16]), &body(1)?),
            Err(StorageError::NotFound)
        ));
        Ok(())
    }

    #[test]
    fn memory_storage_meets_the_contract() -> Result<(), ContractError> {
        check_all(|| Ok(MemoryStorage::new()))
    }

    #[test]
    fn expired_is_inclusive_of_the_expiry_second() {
        let stored = mailbox(1, 100, None);
        assert!(!stored.expired(UnixSeconds::new(99)));
        assert!(stored.expired(UnixSeconds::new(100)));
    }
}
