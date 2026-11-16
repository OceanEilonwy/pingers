// SPDX-License-Identifier: AGPL-3.0-only
//! `knock_key`: what a knock leaves behind until its Welcome arrives.

use pingers_crypto::GrantSecretKey;
use pingers_proto::{MailboxId, PartyId, Subscription, WireDecode as _, WireEncode as _};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{read_array, read_secret, read_time, time};
use crate::{Session, StoreError};

impl Session {
    /// The grant secrets and Meetup subscriptions of knocks waiting for a Welcome
    /// (`knock_key` table).
    #[must_use]
    pub fn knock_keys(&self) -> KnockKeys<'_> {
        KnockKeys { session: self }
    }
}

/// What this device keeps of a knock it sent until the Welcome arrives in the knock's reply
/// mailbox (design B.5, B.6.1). Not `Clone` or `Debug`: it holds a grant secret.
pub struct KnockKey {
    /// The knock's reply mailbox, where the Welcome arrives.
    pub reply_mailbox: MailboxId,
    /// The Party whose Spot was knocked on.
    pub party: PartyId,
    /// The grant secret whose public half is in the knock's Meetup KeyPackage.
    pub grant_key: GrantSecretKey,
    /// The Meetup subscription the knock carried, if wake-up pushes are on: it becomes this
    /// device's own in the Meetup.
    pub subscription: Option<Subscription>,
    /// When the knock was made, for expiry.
    pub created_at: UnixSeconds,
}

/// Repository for the `knock_key` table.
pub struct KnockKeys<'session> {
    session: &'session Session,
}

const COLUMNS: &str = "reply_mailbox, party_id, gk_sk, subscription, created_at";

fn knock_key_from_row(row: &Row<'_>) -> Result<KnockKey, StoreError> {
    let subscription = match row.get_ref(3)? {
        rusqlite::types::ValueRef::Null => None,
        rusqlite::types::ValueRef::Blob(bytes) => Some(Subscription::decode(bytes).map_err(
            |_| StoreError::Corrupt {
                field: "knock_key.subscription",
            },
        )?),
        rusqlite::types::ValueRef::Integer(_)
        | rusqlite::types::ValueRef::Real(_)
        | rusqlite::types::ValueRef::Text(_) => {
            return Err(StoreError::Corrupt {
                field: "knock_key.subscription",
            });
        }
    };
    Ok(KnockKey {
        reply_mailbox: MailboxId::from_bytes(read_array(row, 0, "knock_key.reply_mailbox")?),
        party: PartyId::from_bytes(read_array(row, 1, "knock_key.party_id")?),
        grant_key: GrantSecretKey::from_bytes(read_secret(row, 2, "knock_key.gk_sk")?),
        subscription,
        created_at: read_time(row, 4, "knock_key.created_at")?,
    })
}

impl KnockKeys<'_> {
    /// Keeps what a knock needs until its Welcome; a row for the same reply mailbox is
    /// replaced (a reply mailbox is used for one knock only).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist; [`StoreError::Internal`] if the
    /// subscription does not encode.
    pub fn put(&self, knock: &KnockKey) -> Result<(), StoreError> {
        let subscription = knock
            .subscription
            .as_ref()
            .map(Subscription::encode)
            .transpose()
            .map_err(|_| StoreError::Internal)?;
        self.session.with(|connection| {
            connection.execute(
                &format!(
                    "INSERT OR REPLACE INTO knock_key ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5)"
                ),
                params![
                    knock.reply_mailbox.as_bytes(),
                    knock.party.as_bytes(),
                    knock.grant_key.expose_bytes(),
                    subscription,
                    time(knock.created_at, "knock_key.created_at")?,
                ],
            )?;
            Ok(())
        })
    }

    /// What the knock answered in `reply_mailbox` left, if it is still kept.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, reply_mailbox: &MailboxId) -> Result<Option<KnockKey>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!("SELECT {COLUMNS} FROM knock_key WHERE reply_mailbox = ?1"),
                    [reply_mailbox.as_bytes()],
                    |row| Ok(knock_key_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Forgets the knock answered in `reply_mailbox` (joined, or never to be joined). Absent
    /// is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`].
    pub fn delete(&self, reply_mailbox: &MailboxId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM knock_key WHERE reply_mailbox = ?1",
                [reply_mailbox.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Forgets every knock made before `cutoff`, whose Welcome can no longer come; returns how
    /// many.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] for a cutoff past `i64::MAX`; [`StoreError::Database`].
    pub fn delete_before(&self, cutoff: UnixSeconds) -> Result<usize, StoreError> {
        let cutoff = time(cutoff, "knock_key.created_at")?;
        self.session.with(|connection| {
            Ok(connection.execute("DELETE FROM knock_key WHERE created_at < ?1", [cutoff])?)
        })
    }
}

#[cfg(test)]
#[path = "knock_tests.rs"]
mod tests;
