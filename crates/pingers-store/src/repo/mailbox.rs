// SPDX-License-Identifier: AGPL-3.0-only
//! `mailbox` and `subscription`.

use pingers_crypto::{MailboxReadKey, mailbox_id};
use pingers_proto::{GroupId, MailboxId, PushHandle, SubscriptionId};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};

use crate::codec::{read_array, read_optional_array, read_secret, read_time, time};
use crate::repo::expect_one;
use crate::server::Scope;
use crate::{ServerSession, StoreError};

impl ServerSession<'_> {
    /// This device's anonymous mailboxes on this server (`mailbox` table).
    #[must_use]
    pub fn mailboxes(&self) -> Mailboxes<'_> {
        Mailboxes {
            session: self.scope(),
        }
    }

    /// This device's sealed-push subscriptions in this server's groups, one per group
    /// (`subscription` table).
    #[must_use]
    pub fn subscriptions(&self) -> Subscriptions<'_> {
        Subscriptions {
            session: self.scope(),
        }
    }
}

/// What a mailbox is for (design B.4.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MailboxPurpose {
    /// Receives knocks on this device's board records.
    Knock,
    /// Receives the answer to a knock this device sent.
    Reply,
    /// Receives join requests for an invite.
    Invite,
}

impl MailboxPurpose {
    const fn code(self) -> i64 {
        match self {
            Self::Knock => 0,
            Self::Reply => 1,
            Self::Invite => 2,
        }
    }
}

/// Whether a mailbox is waiting in the pool or has been handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MailboxState {
    /// Registered and unused.
    Pooled,
    /// Named in a record, knock or invite.
    InUse,
}

impl MailboxState {
    const fn code(self) -> i64 {
        match self {
            Self::Pooled => 0,
            Self::InUse => 1,
        }
    }
}

/// One of this device's mailboxes (design C.2 `mailbox`). Its id is derived from the read key,
/// so the two cannot disagree.
pub struct Mailbox {
    /// The capability to read it.
    pub read_key: MailboxReadKey,
    /// What it is for.
    pub purpose: MailboxPurpose,
    /// Pooled or in use.
    pub state: MailboxState,
    /// The handle in its sealed push token, or `None` if it was registered without one
    /// (`has_push` is false).
    pub push: Option<PushHandle>,
    /// When the server forgets it.
    pub expires_at: UnixSeconds,
}

impl Mailbox {
    /// The mailbox id, `SHA-256("pingers/v1/mbox" || read_key)[..16]`.
    #[must_use]
    pub fn id(&self) -> MailboxId {
        mailbox_id(&self.read_key)
    }
}

/// Repository for the `mailbox` table.
pub struct Mailboxes<'session> {
    session: Scope<'session>,
}

const MAILBOX_COLUMNS: &str = "mailbox_id, read_key, purpose, state, handle, expires_at";

fn mailbox_from_row(row: &Row<'_>) -> Result<Mailbox, StoreError> {
    let mailbox = Mailbox {
        read_key: MailboxReadKey::from_bytes(read_secret(row, 1, "mailbox.read_key")?),
        purpose: match row.get::<_, i64>(2)? {
            0 => MailboxPurpose::Knock,
            1 => MailboxPurpose::Reply,
            2 => MailboxPurpose::Invite,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "mailbox.purpose",
                });
            }
        },
        state: match row.get::<_, i64>(3)? {
            0 => MailboxState::Pooled,
            1 => MailboxState::InUse,
            _ => {
                return Err(StoreError::Corrupt {
                    field: "mailbox.state",
                });
            }
        },
        push: read_optional_array(row, 4, "mailbox.handle")?.map(PushHandle::from_bytes),
        expires_at: read_time(row, 5, "mailbox.expires_at")?,
    };
    if mailbox.id().as_bytes() == &read_array::<16>(row, 0, "mailbox.mailbox_id")? {
        Ok(mailbox)
    } else {
        Err(StoreError::Corrupt {
            field: "mailbox.mailbox_id",
        })
    }
}

impl Mailboxes<'_> {
    /// Adds a mailbox.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if it, or another mailbox with the same push handle, exists.
    pub fn insert(&self, mailbox: &Mailbox) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO mailbox (mailbox_id, read_key, purpose, state, has_push, handle, \
                 expires_at, server_key) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    mailbox.id().as_bytes(),
                    mailbox.read_key.expose_bytes(),
                    mailbox.purpose.code(),
                    mailbox.state.code(),
                    i64::from(mailbox.push.is_some()),
                    mailbox.push.as_ref().map(PushHandle::as_bytes),
                    time(mailbox.expires_at, "mailbox.expires_at")?,
                    self.session.key(),
                ],
            )?;
            Ok(())
        })
    }

    /// The mailbox with this id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, id: &MailboxId) -> Result<Option<Mailbox>, StoreError> {
        self.query_one("mailbox_id", id.as_bytes())
    }

    /// The mailbox a content-free push with this handle is about (design B.7).
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn by_handle(&self, handle: &PushHandle) -> Result<Option<Mailbox>, StoreError> {
        self.query_one("handle", handle.as_bytes())
    }

    fn query_one(&self, column: &str, value: &[u8]) -> Result<Option<Mailbox>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!(
                        "SELECT {MAILBOX_COLUMNS} FROM mailbox \
                         WHERE {column} = ?1 AND server_key = ?2"
                    ),
                    params![value, self.session.key()],
                    |row| Ok(mailbox_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Takes a pooled, unexpired mailbox for `purpose` (the one expiring last, so it stays
    /// usable longest) and marks it in use, atomically.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn take_pooled(
        &self,
        purpose: MailboxPurpose,
        now: UnixSeconds,
    ) -> Result<Option<Mailbox>, StoreError> {
        self.session.transaction(|session| {
            let taken = session.with(|connection| {
                connection
                    .query_row(
                        &format!(
                            "SELECT {MAILBOX_COLUMNS} FROM mailbox WHERE purpose = ?1 \
                             AND state = ?2 AND expires_at > ?3 AND server_key = ?4 \
                             ORDER BY expires_at DESC, mailbox_id LIMIT 1"
                        ),
                        params![
                            purpose.code(),
                            MailboxState::Pooled.code(),
                            time(now, "now")?,
                            session.key()
                        ],
                        |row| Ok(mailbox_from_row(row)),
                    )
                    .optional()?
                    .transpose()
            })?;
            let Some(mut mailbox) = taken else {
                return Ok(None);
            };
            self.set_state(&mailbox.id(), MailboxState::InUse)?;
            mailbox.state = MailboxState::InUse;
            Ok(Some(mailbox))
        })
    }

    /// How many unexpired pooled mailboxes there are for `purpose`, to decide on a refill.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the query fails.
    pub fn pooled_count(
        &self,
        purpose: MailboxPurpose,
        now: UnixSeconds,
    ) -> Result<u32, StoreError> {
        self.session.with(|connection| {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM mailbox WHERE purpose = ?1 AND state = ?2 \
                 AND expires_at > ?3 AND server_key = ?4",
                params![
                    purpose.code(),
                    MailboxState::Pooled.code(),
                    time(now, "now")?,
                    self.session.key()
                ],
                |row| row.get(0),
            )?;
            u32::try_from(count).map_err(|_| StoreError::Corrupt {
                field: "mailbox count",
            })
        })
    }

    /// Moves a mailbox to `state`.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such mailbox.
    pub fn set_state(&self, id: &MailboxId, state: MailboxState) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "UPDATE mailbox SET state = ?2 WHERE mailbox_id = ?1 AND server_key = ?3",
                    params![id.as_bytes(), state.code(), self.session.key()],
                )?,
                "mailbox",
            )
        })
    }

    /// Deletes a mailbox.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if there is no such mailbox.
    pub fn delete(&self, id: &MailboxId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            expect_one(
                connection.execute(
                    "DELETE FROM mailbox WHERE mailbox_id = ?1 AND server_key = ?2",
                    params![id.as_bytes(), self.session.key()],
                )?,
                "mailbox",
            )
        })
    }

    /// Deletes every mailbox that expired at or before `now`; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_expired(&self, now: UnixSeconds) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM mailbox WHERE expires_at <= ?1 AND server_key = ?2",
                params![time(now, "now")?, self.session.key()],
            )?)
        })
    }
}

/// This device's sealed-push subscription to one group's log (design C.2 `subscription`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredSubscription {
    /// The group.
    pub group: GroupId,
    /// The subscription's id on the server.
    pub id: SubscriptionId,
    /// The handle in its sealed push token.
    pub handle: PushHandle,
    /// When the server forgets it.
    pub expires_at: UnixSeconds,
}

/// Repository for the `subscription` table.
pub struct Subscriptions<'session> {
    session: Scope<'session>,
}

fn subscription_from_row(row: &Row<'_>) -> Result<StoredSubscription, StoreError> {
    Ok(StoredSubscription {
        group: GroupId::from_bytes(read_array(row, 0, "subscription.group_id")?),
        id: SubscriptionId::from_bytes(read_array(row, 1, "subscription.sub_id")?),
        handle: PushHandle::from_bytes(read_array(row, 2, "subscription.handle")?),
        expires_at: read_time(row, 3, "subscription.expires_at")?,
    })
}

impl Subscriptions<'_> {
    /// Stores the group's subscription, replacing the previous one (renewal).
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if another group's subscription has the same id or handle.
    pub fn put(&self, subscription: &StoredSubscription) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO subscription (group_id, sub_id, handle, expires_at, \
                 server_key) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    subscription.group.as_bytes(),
                    subscription.id.as_bytes(),
                    subscription.handle.as_bytes(),
                    time(subscription.expires_at, "subscription.expires_at")?,
                    self.session.key(),
                ],
            )?;
            Ok(())
        })
    }

    /// The group's subscription.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, group: &GroupId) -> Result<Option<StoredSubscription>, StoreError> {
        self.query_one("group_id", group.as_bytes())
    }

    /// The subscription a content-free push with this handle is about.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn by_handle(&self, handle: &PushHandle) -> Result<Option<StoredSubscription>, StoreError> {
        self.query_one("handle", handle.as_bytes())
    }

    fn query_one(
        &self,
        column: &str,
        value: &[u8],
    ) -> Result<Option<StoredSubscription>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!(
                        "SELECT group_id, sub_id, handle, expires_at FROM subscription \
                         WHERE {column} = ?1 AND server_key = ?2"
                    ),
                    params![value, self.session.key()],
                    |row| Ok(subscription_from_row(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Deletes the group's subscription; deleting an absent one is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete(&self, group: &GroupId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM subscription WHERE group_id = ?1 AND server_key = ?2",
                params![group.as_bytes(), self.session.key()],
            )?;
            Ok(())
        })
    }

    /// Deletes every subscription that expired at or before `now`; returns how many.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_expired(&self, now: UnixSeconds) -> Result<usize, StoreError> {
        self.session.with(|connection| {
            Ok(connection.execute(
                "DELETE FROM subscription WHERE expires_at <= ?1 AND server_key = ?2",
                params![time(now, "now")?, self.session.key()],
            )?)
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use zeroize::Zeroizing;

    use super::*;
    use crate::test_support::{Fixture, TestError};

    fn mailbox(fill: u8, purpose: MailboxPurpose, expires_at: u64) -> Mailbox {
        Mailbox {
            read_key: MailboxReadKey::from_bytes(Zeroizing::new([fill; 32])),
            purpose,
            state: MailboxState::Pooled,
            push: Some(PushHandle::from_bytes([fill; 8])),
            expires_at: UnixSeconds::new(expires_at),
        }
    }

    fn summary(
        mailbox: &Mailbox,
    ) -> (
        MailboxId,
        MailboxPurpose,
        MailboxState,
        Option<PushHandle>,
        UnixSeconds,
        [u8; 32],
    ) {
        (
            mailbox.id(),
            mailbox.purpose,
            mailbox.state,
            mailbox.push,
            mailbox.expires_at,
            *mailbox.read_key.expose_bytes(),
        )
    }

    #[test]
    fn mailboxes_round_trip_by_id_and_handle() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let mailboxes = server.mailboxes();
        let with_push = mailbox(1, MailboxPurpose::Reply, 500);
        let mut without_push = mailbox(2, MailboxPurpose::Invite, 600);
        without_push.push = None;
        without_push.state = MailboxState::InUse;
        mailboxes.insert(&with_push)?;
        mailboxes.insert(&without_push)?;
        assert!(mailboxes.insert(&with_push).is_err());
        let by_id = mailboxes.get(&without_push.id())?;
        assert_eq!(by_id.as_ref().map(summary), Some(summary(&without_push)));
        let by_handle = mailboxes.by_handle(&PushHandle::from_bytes([1; 8]))?;
        assert_eq!(by_handle.as_ref().map(summary), Some(summary(&with_push)));
        assert!(
            mailboxes
                .by_handle(&PushHandle::from_bytes([2; 8]))?
                .is_none()
        );
        assert!(mailboxes.get(&MailboxId::from_bytes([0; 16]))?.is_none());
        Ok(())
    }

    #[test]
    fn take_pooled_hands_out_the_longest_lived_unexpired_mailbox_once() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let mailboxes = server.mailboxes();
        let now = UnixSeconds::new(1_000);
        mailboxes.insert(&mailbox(1, MailboxPurpose::Knock, 1_500))?;
        mailboxes.insert(&mailbox(2, MailboxPurpose::Knock, 2_000))?;
        mailboxes.insert(&mailbox(3, MailboxPurpose::Knock, 1_000))?;
        mailboxes.insert(&mailbox(4, MailboxPurpose::Reply, 9_000))?;
        assert_eq!(mailboxes.pooled_count(MailboxPurpose::Knock, now)?, 2);
        let first = mailboxes.take_pooled(MailboxPurpose::Knock, now)?;
        let expected = mailbox(2, MailboxPurpose::Knock, 2_000);
        assert_eq!(first.as_ref().map(Mailbox::id), Some(expected.id()));
        assert_eq!(first.map(|taken| taken.state), Some(MailboxState::InUse));
        let stored = mailboxes.get(&expected.id())?;
        assert_eq!(stored.map(|stored| stored.state), Some(MailboxState::InUse));
        let second = mailboxes.take_pooled(MailboxPurpose::Knock, now)?;
        assert_eq!(
            second.map(|taken| taken.id()),
            Some(mailbox(1, MailboxPurpose::Knock, 0).id())
        );
        // Mailbox 3 expires exactly now, so it is not handed out.
        assert!(mailboxes.take_pooled(MailboxPurpose::Knock, now)?.is_none());
        assert_eq!(mailboxes.pooled_count(MailboxPurpose::Knock, now)?, 0);
        assert_eq!(mailboxes.pooled_count(MailboxPurpose::Reply, now)?, 1);
        Ok(())
    }

    #[test]
    fn mailbox_state_and_deletion() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let mailboxes = server.mailboxes();
        let kept = mailbox(1, MailboxPurpose::Knock, 100);
        let expiring = mailbox(2, MailboxPurpose::Knock, 50);
        let doomed = mailbox(3, MailboxPurpose::Knock, 200);
        for each in [&kept, &expiring, &doomed] {
            mailboxes.insert(each)?;
        }
        mailboxes.set_state(&kept.id(), MailboxState::InUse)?;
        mailboxes.set_state(&kept.id(), MailboxState::Pooled)?;
        assert_eq!(
            mailboxes.get(&kept.id())?.map(|stored| stored.state),
            Some(MailboxState::Pooled)
        );
        mailboxes.delete(&doomed.id())?;
        assert!(mailboxes.get(&doomed.id())?.is_none());
        assert_eq!(mailboxes.delete_expired(UnixSeconds::new(49))?, 0);
        assert_eq!(mailboxes.delete_expired(UnixSeconds::new(50))?, 1);
        assert!(mailboxes.get(&kept.id())?.is_some());
        let not_found = |result: Result<(), StoreError>| {
            matches!(result, Err(StoreError::NotFound { table: "mailbox" }))
        };
        assert!(not_found(mailboxes.delete(&doomed.id())));
        assert!(not_found(
            mailboxes.set_state(&doomed.id(), MailboxState::InUse)
        ));
        Ok(())
    }

    #[test]
    fn a_mailbox_whose_id_does_not_match_its_key_is_corrupt() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let stored = mailbox(1, MailboxPurpose::Knock, 100);
        fixture.server()?.mailboxes().insert(&stored)?;
        fixture.session.with(|connection| {
            connection.execute("UPDATE mailbox SET read_key = zeroblob(32)", [])?;
            Ok(())
        })?;
        assert!(matches!(
            fixture
                .server()?
                .mailboxes()
                .by_handle(&PushHandle::from_bytes([1; 8])),
            Err(StoreError::Corrupt {
                field: "mailbox.mailbox_id"
            })
        ));
        Ok(())
    }

    fn subscription(fill: u8, expires_at: u64) -> StoredSubscription {
        StoredSubscription {
            group: GroupId::from_bytes([fill; 32]),
            id: SubscriptionId::from_bytes([fill; 16]),
            handle: PushHandle::from_bytes([fill; 8]),
            expires_at: UnixSeconds::new(expires_at),
        }
    }

    #[test]
    fn subscriptions_round_trip_renew_and_expire() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let server = fixture.server()?;
        let subscriptions = server.subscriptions();
        let first = subscription(1, 100);
        let second = subscription(2, 300);
        subscriptions.put(&first)?;
        subscriptions.put(&second)?;
        assert_eq!(subscriptions.get(&first.group)?, Some(first));
        assert_eq!(subscriptions.by_handle(&second.handle)?, Some(second));
        let renewed = StoredSubscription {
            id: SubscriptionId::from_bytes([9; 16]),
            handle: PushHandle::from_bytes([9; 8]),
            expires_at: UnixSeconds::new(200),
            ..first
        };
        subscriptions.put(&renewed)?;
        assert_eq!(subscriptions.get(&first.group)?, Some(renewed));
        assert_eq!(subscriptions.by_handle(&first.handle)?, None);
        assert_eq!(subscriptions.delete_expired(UnixSeconds::new(199))?, 0);
        assert_eq!(subscriptions.delete_expired(UnixSeconds::new(200))?, 1);
        subscriptions.delete(&second.group)?;
        subscriptions.delete(&second.group)?;
        assert_eq!(fixture.rows("subscription")?, 0);
        Ok(())
    }
}
