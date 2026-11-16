// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-parties' invites and joins (schema version 7): `party_invite` and `join_approval` on
//! the inviter's side, `party_join` on the joiner's, and `joined_via` once a join completed.

use pingers_crypto::{InviteSecret, JoinCode};
use pingers_proto::{
    DisplayName, InvitePayload, JoinRequest, KeyPackageBytes, LeafIndex, MailboxId, PartyId,
    SequenceNumber, SubscriptionId, WireDecode as _, WireEncode as _,
};
use pingers_time::UnixSeconds;
use rusqlite::{OptionalExtension as _, Row, params};
use zeroize::Zeroizing;

use crate::codec::{
    from_int, narrow, read_array, read_optional_array, read_secret, read_secret_blob, read_time,
    time, to_int,
};
use crate::repo::expect_one;
use crate::{Session, StoreError};

impl Session {
    /// Invites this device made (`party_invite`).
    #[must_use]
    pub fn party_invites(&self) -> PartyInvites<'_> {
        PartyInvites { session: self }
    }

    /// Join requests waiting for this device's approval (`join_approval`).
    #[must_use]
    pub fn join_approvals(&self) -> JoinApprovals<'_> {
        JoinApprovals { session: self }
    }

    /// Joins this device asked for and is waiting on (`party_join`).
    #[must_use]
    pub fn party_joins(&self) -> PartyJoins<'_> {
        PartyJoins { session: self }
    }

    /// How this device came into the Parties it joined (`joined_via`).
    #[must_use]
    pub fn joined_via(&self) -> JoinedVias<'_> {
        JoinedVias { session: self }
    }
}

/// Where an invite stands, apart from expiry (which follows from the time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteState {
    /// Accepting join requests until it expires.
    Open,
    /// Someone was let in through it.
    Used {
        /// The reply mailbox of the joiner let in.
        admitted: MailboxId,
    },
    /// The inviter cancelled it.
    Revoked,
}

/// An invite this device made. The secret is the link's, kept until the invite goes with its
/// Party; the store is encrypted at rest.
pub struct PartyInvite {
    /// The invite's mailbox, which also names the invite.
    pub mailbox: MailboxId,
    /// The Party it invites to.
    pub party: PartyId,
    /// The link's secret.
    pub secret: InviteSecret,
    /// When it was made.
    pub shared_at: UnixSeconds,
    /// When it stops accepting join requests.
    pub expires: UnixSeconds,
    /// Open, used or cancelled.
    pub state: InviteState,
    /// The last post read from the mailbox (`None` before any), so a read resumes after it.
    pub read_position: Option<SequenceNumber>,
}

/// Repository for `party_invite`.
pub struct PartyInvites<'session> {
    session: &'session Session,
}

impl PartyInvites<'_> {
    /// Stores a new invite.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist or the mailbox already names an
    /// invite; [`StoreError::OutOfRange`] for a time or position past the column's range.
    pub fn insert(&self, invite: &PartyInvite) -> Result<(), StoreError> {
        let columns = InviteColumns::of(invite)?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO party_invite (mailbox_id, party_id, secret, shared_at, expires_at, \
                 status, admitted_reply, read_position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    invite.mailbox.as_bytes(),
                    invite.party.as_bytes(),
                    invite.secret.expose_bytes(),
                    columns.shared_at,
                    columns.expires,
                    columns.status,
                    columns.admitted,
                    columns.read_position,
                ],
            )?;
            Ok(())
        })
    }

    /// Replaces a stored invite's state and read position (its Party, secret and times never
    /// change). Its waiting join requests stay.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if no invite has that mailbox; [`StoreError::OutOfRange`].
    pub fn update(&self, invite: &PartyInvite) -> Result<(), StoreError> {
        let columns = InviteColumns::of(invite)?;
        self.session.with(|connection| {
            let changed = connection.execute(
                "UPDATE party_invite SET status = ?2, admitted_reply = ?3, read_position = ?4 \
                 WHERE mailbox_id = ?1",
                params![
                    invite.mailbox.as_bytes(),
                    columns.status,
                    columns.admitted,
                    columns.read_position,
                ],
            )?;
            expect_one(changed, "party_invite")
        })
    }

    /// The invite with this mailbox, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, mailbox: &MailboxId) -> Result<Option<PartyInvite>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!("{INVITE_SELECT} WHERE mailbox_id = ?1"),
                    [mailbox.as_bytes()],
                    |row| Ok(read_invite(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// A Party's invites, oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, party: &PartyId) -> Result<Vec<PartyInvite>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "{INVITE_SELECT} WHERE party_id = ?1 ORDER BY shared_at, rowid"
            ))?;
            let rows = statement.query_map([party.as_bytes()], |row| Ok(read_invite(row)))?;
            rows.map(|invite| invite?).collect()
        })
    }
}

const INVITE_SELECT: &str = "SELECT mailbox_id, party_id, secret, shared_at, expires_at, status, \
                             admitted_reply, read_position FROM party_invite";

/// An invite's column values that need converting.
struct InviteColumns<'invite> {
    shared_at: i64,
    expires: i64,
    status: i64,
    admitted: Option<&'invite [u8; 16]>,
    read_position: Option<i64>,
}

impl<'invite> InviteColumns<'invite> {
    fn of(invite: &'invite PartyInvite) -> Result<Self, StoreError> {
        let (status, admitted) = match &invite.state {
            InviteState::Open => (0, None),
            InviteState::Used { admitted } => (1, Some(admitted.as_bytes())),
            InviteState::Revoked => (2, None),
        };
        Ok(Self {
            shared_at: time(invite.shared_at, "party_invite.shared_at")?,
            expires: time(invite.expires, "party_invite.expires_at")?,
            status,
            admitted,
            read_position: invite
                .read_position
                .map(|position| to_int(position.get(), "party_invite.read_position"))
                .transpose()?,
        })
    }
}

fn read_invite(row: &Row<'_>) -> Result<PartyInvite, StoreError> {
    let admitted = read_optional_array(row, 6, "party_invite.admitted_reply")?;
    let state = match (row.get::<_, i64>(5)?, admitted) {
        (0, None) => InviteState::Open,
        (1, Some(reply)) => InviteState::Used {
            admitted: MailboxId::from_bytes(reply),
        },
        (2, None) => InviteState::Revoked,
        _ => {
            return Err(StoreError::Corrupt {
                field: "party_invite.status",
            });
        }
    };
    Ok(PartyInvite {
        mailbox: MailboxId::from_bytes(read_array(row, 0, "party_invite.mailbox_id")?),
        party: PartyId::from_bytes(read_array(row, 1, "party_invite.party_id")?),
        secret: InviteSecret::from_bytes(read_secret(row, 2, "party_invite.secret")?),
        shared_at: read_time(row, 3, "party_invite.shared_at")?,
        expires: read_time(row, 4, "party_invite.expires_at")?,
        state,
        read_position: row
            .get::<_, Option<i64>>(7)?
            .map(|position| {
                from_int(position, "party_invite.read_position").map(SequenceNumber::new)
            })
            .transpose()?,
    })
}

/// A join request waiting for the inviter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinApproval {
    /// The invite it came through.
    pub invite: MailboxId,
    /// When this device read it.
    pub asked_at: UnixSeconds,
    /// The request as it arrived; its reply mailbox names the approval.
    pub request: JoinRequest,
}

/// Repository for `join_approval`.
pub struct JoinApprovals<'session> {
    session: &'session Session,
}

impl JoinApprovals<'_> {
    /// Files a request for approval. A request with the same reply mailbox replaces the one
    /// waiting and keeps its place in the order.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party or the invite does not exist;
    /// [`StoreError::OutOfRange`] if the request does not encode.
    pub fn put(&self, party: &PartyId, approval: &JoinApproval) -> Result<(), StoreError> {
        let request =
            Zeroizing::new(
                approval
                    .request
                    .encode()
                    .map_err(|_| StoreError::OutOfRange {
                        field: "join_approval.request",
                    })?,
            );
        let asked_at = time(approval.asked_at, "join_approval.asked_at")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT INTO join_approval (party_id, reply_mailbox, invite_mailbox, asked_at, \
                 request) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (party_id, reply_mailbox) DO UPDATE SET \
                 invite_mailbox = excluded.invite_mailbox, asked_at = excluded.asked_at, \
                 request = excluded.request",
                params![
                    party.as_bytes(),
                    approval.request.reply_mailbox.as_bytes(),
                    approval.invite.as_bytes(),
                    asked_at,
                    request.as_slice(),
                ],
            )?;
            Ok(())
        })
    }

    /// The waiting request whose Welcome would go to `reply`, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(
        &self,
        party: &PartyId,
        reply: &MailboxId,
    ) -> Result<Option<JoinApproval>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    &format!("{APPROVAL_SELECT} WHERE party_id = ?1 AND reply_mailbox = ?2"),
                    params![party.as_bytes(), reply.as_bytes()],
                    |row| Ok(read_approval(row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// The Party's waiting requests, in the order they arrived.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn list(&self, party: &PartyId) -> Result<Vec<JoinApproval>, StoreError> {
        self.session.with(|connection| {
            let mut statement = connection.prepare(&format!(
                "{APPROVAL_SELECT} WHERE party_id = ?1 ORDER BY rowid"
            ))?;
            let rows = statement.query_map([party.as_bytes()], |row| Ok(read_approval(row)))?;
            rows.map(|approval| approval?).collect()
        })
    }

    /// Drops a waiting request; dropping none is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete(&self, party: &PartyId, reply: &MailboxId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM join_approval WHERE party_id = ?1 AND reply_mailbox = ?2",
                params![party.as_bytes(), reply.as_bytes()],
            )?;
            Ok(())
        })
    }

    /// Drops every waiting request that came through `invite`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete_for_invite(&self, invite: &MailboxId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM join_approval WHERE invite_mailbox = ?1",
                [invite.as_bytes()],
            )?;
            Ok(())
        })
    }
}

const APPROVAL_SELECT: &str = "SELECT invite_mailbox, asked_at, request FROM join_approval";

fn read_approval(row: &Row<'_>) -> Result<JoinApproval, StoreError> {
    let request = read_secret_blob(row, 2, "join_approval.request")?;
    Ok(JoinApproval {
        invite: MailboxId::from_bytes(read_array(row, 0, "join_approval.invite_mailbox")?),
        asked_at: read_time(row, 1, "join_approval.asked_at")?,
        request: JoinRequest::decode(&request).map_err(|_| StoreError::Corrupt {
            field: "join_approval.request",
        })?,
    })
}

/// A join this device asked for and is waiting on.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyJoin {
    /// The reply mailbox where the Welcome goes, which names the join.
    pub reply: MailboxId,
    /// The link it came from (with the invite secret).
    pub payload: InvitePayload,
    /// The name it gave.
    pub name: DisplayName,
    /// The Party subscription it gave, if any.
    pub subscription: Option<SubscriptionId>,
    /// The classical key package the request carried.
    pub key_package: KeyPackageBytes,
    /// The X-Wing key package the request carried, if this build made one.
    pub quantum_resistant: Option<KeyPackageBytes>,
    /// When it asked.
    pub asked_at: UnixSeconds,
}

/// Repository for `party_join`.
pub struct PartyJoins<'session> {
    session: &'session Session,
}

impl PartyJoins<'_> {
    /// Records a join, replacing one with the same reply mailbox.
    ///
    /// # Errors
    ///
    /// [`StoreError::OutOfRange`] if the link does not encode or a time is past the column's
    /// range; [`StoreError::Database`] if the write fails.
    pub fn put(&self, join: &PartyJoin) -> Result<(), StoreError> {
        let payload =
            Zeroizing::new(join.payload.encode().map_err(|_| StoreError::OutOfRange {
                field: "party_join.payload",
            })?);
        let asked_at = time(join.asked_at, "party_join.asked_at")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO party_join (reply_mailbox, payload, display_name, sub_id, \
                 key_package, x_wing_key_package, asked_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    join.reply.as_bytes(),
                    payload.as_slice(),
                    join.name.as_str(),
                    join.subscription.as_ref().map(SubscriptionId::as_bytes),
                    join.key_package.as_bytes(),
                    join.quantum_resistant
                        .as_ref()
                        .map(KeyPackageBytes::as_bytes),
                    asked_at,
                ],
            )?;
            Ok(())
        })
    }

    /// The join whose Welcome goes to `reply`, if any.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, reply: &MailboxId) -> Result<Option<PartyJoin>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT payload, display_name, sub_id, key_package, x_wing_key_package, \
                     asked_at FROM party_join WHERE reply_mailbox = ?1",
                    [reply.as_bytes()],
                    |row| Ok(read_join(*reply, row)),
                )
                .optional()?
                .transpose()
        })
    }

    /// Forgets a join; forgetting none is not an error.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the delete fails.
    pub fn delete(&self, reply: &MailboxId) -> Result<(), StoreError> {
        self.session.with(|connection| {
            connection.execute(
                "DELETE FROM party_join WHERE reply_mailbox = ?1",
                [reply.as_bytes()],
            )?;
            Ok(())
        })
    }
}

fn read_join(reply: MailboxId, row: &Row<'_>) -> Result<PartyJoin, StoreError> {
    let payload = read_secret_blob(row, 0, "party_join.payload")?;
    let corrupt = |field| move |_| StoreError::Corrupt { field };
    Ok(PartyJoin {
        reply,
        payload: InvitePayload::decode(&payload).map_err(corrupt("party_join.payload"))?,
        name: DisplayName::new(row.get(1)?).map_err(corrupt("party_join.display_name"))?,
        subscription: read_optional_array(row, 2, "party_join.sub_id")?
            .map(SubscriptionId::from_bytes),
        key_package: KeyPackageBytes::from_bytes(row.get(3)?),
        quantum_resistant: row
            .get::<_, Option<Vec<u8>>>(4)?
            .map(KeyPackageBytes::from_bytes),
        asked_at: read_time(row, 5, "party_join.asked_at")?,
    })
}

/// How this device came into a Party it joined: who let it in, and the code that was checked
/// ("Ana let you in"; the Party's security details).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinedVia {
    /// The inviter's leaf.
    pub inviter: LeafIndex,
    /// The code both phones showed.
    pub code: JoinCode,
}

/// Repository for `joined_via`.
pub struct JoinedVias<'session> {
    session: &'session Session,
}

impl JoinedVias<'_> {
    /// Records how this device joined the Party, replacing any record.
    ///
    /// # Errors
    ///
    /// [`StoreError::Database`] if the Party does not exist or the write fails.
    pub fn put(&self, party: &PartyId, via: JoinedVia) -> Result<(), StoreError> {
        let code = to_int(via.code.value(), "joined_via.join_code")?;
        self.session.with(|connection| {
            connection.execute(
                "INSERT OR REPLACE INTO joined_via (party_id, inviter_leaf, join_code) \
                 VALUES (?1, ?2, ?3)",
                params![party.as_bytes(), via.inviter.get(), code],
            )?;
            Ok(())
        })
    }

    /// How this device joined the Party; `None` for one it created.
    ///
    /// # Errors
    ///
    /// [`StoreError::Corrupt`] if a stored value does not decode.
    pub fn get(&self, party: &PartyId) -> Result<Option<JoinedVia>, StoreError> {
        self.session.with(|connection| {
            connection
                .query_row(
                    "SELECT inviter_leaf, join_code FROM joined_via WHERE party_id = ?1",
                    [party.as_bytes()],
                    |row| {
                        Ok((|| {
                            let code = from_int(row.get(1)?, "joined_via.join_code")?;
                            Ok::<_, StoreError>(JoinedVia {
                                inviter: LeafIndex::new(narrow(
                                    row.get(0)?,
                                    "joined_via.inviter_leaf",
                                )?),
                                code: JoinCode::from_value(code).ok_or(StoreError::Corrupt {
                                    field: "joined_via.join_code",
                                })?,
                            })
                        })())
                    },
                )
                .optional()?
                .transpose()
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::{InviterFingerprint, SecretBytes, ServerId};

    use super::*;
    use crate::test_support::{Fixture, TestError, party};

    fn mailbox(fill: u8) -> MailboxId {
        MailboxId::from_bytes([fill; 16])
    }

    fn invite(owner: &PartyId, fill: u8, shared_at: u64) -> PartyInvite {
        PartyInvite {
            mailbox: mailbox(fill),
            party: *owner,
            secret: InviteSecret::from_bytes(Zeroizing::new([fill; 32])),
            shared_at: UnixSeconds::new(shared_at),
            expires: UnixSeconds::new(shared_at.saturating_add(100)),
            state: InviteState::Open,
            read_position: None,
        }
    }

    fn request(reply: u8, name: &str) -> Result<JoinRequest, TestError> {
        Ok(JoinRequest {
            key_package: KeyPackageBytes::from_bytes(vec![reply; 4]),
            quantum_resistant_key_package: Some(KeyPackageBytes::from_bytes(vec![reply; 9])),
            display_name: DisplayName::new(name.into())?,
            reply_mailbox: mailbox(reply),
            party_subscription: None,
        })
    }

    fn summary(invite: &PartyInvite) -> (MailboxId, u64, u64, InviteState, Option<u64>, [u8; 32]) {
        (
            invite.mailbox,
            invite.shared_at.get(),
            invite.expires.get(),
            invite.state,
            invite.read_position.map(SequenceNumber::get),
            *invite.secret.expose_bytes(),
        )
    }

    #[test]
    fn invites_round_trip_and_list_oldest_first() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let (owner, other) = (party(1)?, party(2)?);
        session.parties().insert(&owner)?;
        session.parties().insert(&other)?;
        let invites = session.party_invites();
        assert!(invites.get(&mailbox(3))?.is_none());
        invites.insert(&invite(&owner.id, 3, 50))?;
        invites.insert(&invite(&owner.id, 4, 20))?;
        invites.insert(&invite(&other.id, 5, 10))?;
        let listed: Vec<MailboxId> = invites
            .list(&owner.id)?
            .iter()
            .map(|invite| invite.mailbox)
            .collect();
        assert_eq!(listed, vec![mailbox(4), mailbox(3)]);
        // A mailbox names one invite.
        assert!(invites.insert(&invite(&other.id, 3, 1)).is_err());
        for (state, position) in [
            (
                InviteState::Used {
                    admitted: mailbox(9),
                },
                Some(7),
            ),
            (InviteState::Revoked, Some(8)),
            (InviteState::Open, None),
        ] {
            let mut changed = invite(&owner.id, 3, 50);
            changed.state = state;
            changed.read_position = position.map(SequenceNumber::new);
            invites.update(&changed)?;
            let stored = invites.get(&mailbox(3))?.ok_or(StoreError::NotFound {
                table: "party_invite",
            })?;
            assert_eq!(summary(&stored), summary(&changed));
            assert_eq!(stored.party, owner.id);
        }
        assert!(matches!(
            invites.update(&invite(&owner.id, 6, 1)),
            Err(StoreError::NotFound { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_used_invite_without_its_joiner_is_corrupt() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        session.party_invites().insert(&invite(&owner.id, 3, 1))?;
        // The CHECK refuses the mismatch, so write past it the way a damaged file would.
        session.with(|connection| {
            connection.execute_batch(
                "PRAGMA ignore_check_constraints = ON; \
                 UPDATE party_invite SET status = 1; \
                 PRAGMA ignore_check_constraints = OFF;",
            )?;
            Ok(())
        })?;
        assert!(matches!(
            session.party_invites().get(&mailbox(3)),
            Err(StoreError::Corrupt {
                field: "party_invite.status"
            })
        ));
        Ok(())
    }

    #[test]
    fn approvals_keep_their_order_and_go_with_their_invite() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        session.party_invites().insert(&invite(&owner.id, 3, 1))?;
        session.party_invites().insert(&invite(&owner.id, 4, 2))?;
        let approvals = session.join_approvals();
        let approval = |invite: u8, reply: u8, name: &str| -> Result<JoinApproval, TestError> {
            Ok(JoinApproval {
                invite: mailbox(invite),
                asked_at: UnixSeconds::new(u64::from(reply)),
                request: request(reply, name)?,
            })
        };
        approvals.put(&owner.id, &approval(3, 10, "Ben")?)?;
        approvals.put(&owner.id, &approval(4, 11, "Cy")?)?;
        approvals.put(&owner.id, &approval(3, 12, "Di")?)?;
        // The same joiner again: replaced, in its old place.
        let again = approval(3, 10, "Benjamin")?;
        approvals.put(&owner.id, &again)?;
        let names: Vec<String> = approvals
            .list(&owner.id)?
            .into_iter()
            .map(|approval| approval.request.display_name.as_str().to_owned())
            .collect();
        assert_eq!(names, ["Benjamin", "Cy", "Di"]);
        assert_eq!(approvals.get(&owner.id, &mailbox(10))?, Some(again));
        // A request needs its invite.
        assert!(approvals.put(&owner.id, &approval(5, 13, "Ed")?).is_err());
        approvals.delete(&owner.id, &mailbox(11))?;
        approvals.delete(&owner.id, &mailbox(11))?;
        assert_eq!(approvals.get(&owner.id, &mailbox(11))?, None);
        approvals.delete_for_invite(&mailbox(3))?;
        assert!(approvals.list(&owner.id)?.is_empty());
        approvals.put(&owner.id, &approval(4, 14, "Flo")?)?;
        session.with(|connection| {
            connection.execute(
                "DELETE FROM party_invite WHERE mailbox_id = ?1",
                [mailbox(4).as_bytes()],
            )?;
            Ok(())
        })?;
        assert_eq!(fixture.rows("join_approval")?, 0);
        Ok(())
    }

    #[test]
    fn joins_round_trip_and_are_forgotten() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let joins = fixture.session.party_joins();
        let join = PartyJoin {
            reply: mailbox(2),
            payload: InvitePayload {
                server: ServerId::new("https://pingers.example".into())?,
                invite_mailbox: mailbox(3),
                invite_secret: SecretBytes::new([4; 32]),
                inviter_fingerprint: InviterFingerprint::from_bytes([5; 8]),
                expires: UnixSeconds::new(900),
                registration_code: None,
            },
            name: DisplayName::new("Ben".into())?,
            subscription: Some(SubscriptionId::from_bytes([6; 16])),
            key_package: KeyPackageBytes::from_bytes(vec![7; 5]),
            quantum_resistant: Some(KeyPackageBytes::from_bytes(vec![8; 6])),
            asked_at: UnixSeconds::new(40),
        };
        assert_eq!(joins.get(&mailbox(2))?, None);
        joins.put(&join)?;
        assert_eq!(joins.get(&mailbox(2))?, Some(join.clone()));
        let classical_only = PartyJoin {
            subscription: None,
            quantum_resistant: None,
            ..join
        };
        joins.put(&classical_only)?;
        assert_eq!(joins.get(&mailbox(2))?, Some(classical_only));
        joins.delete(&mailbox(2))?;
        joins.delete(&mailbox(2))?;
        assert_eq!(joins.get(&mailbox(2))?, None);
        Ok(())
    }

    #[test]
    fn joined_via_round_trips_and_refuses_a_long_code() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let owner = party(1)?;
        session.parties().insert(&owner)?;
        assert_eq!(session.joined_via().get(&owner.id)?, None);
        let code = JoinCode::from_value(999_999_999_999).ok_or(StoreError::Internal)?;
        let via = JoinedVia {
            inviter: LeafIndex::new(3),
            code,
        };
        session.joined_via().put(&owner.id, via)?;
        assert_eq!(session.joined_via().get(&owner.id)?, Some(via));
        session.with(|connection| {
            connection.execute_batch(
                "PRAGMA ignore_check_constraints = ON; \
                 UPDATE joined_via SET join_code = 1000000000000; \
                 PRAGMA ignore_check_constraints = OFF;",
            )?;
            Ok(())
        })?;
        assert!(matches!(
            session.joined_via().get(&owner.id),
            Err(StoreError::Corrupt {
                field: "joined_via.join_code"
            })
        ));
        // A join to no Party cannot be recorded.
        assert!(session.joined_via().put(&party(9)?.id, via).is_err());
        Ok(())
    }

    #[test]
    fn invites_approvals_and_origin_go_with_their_party() -> Result<(), TestError> {
        let fixture = Fixture::new()?;
        let session = &fixture.session;
        let (gone, kept) = (party(1)?, party(2)?);
        for (owner, fill) in [(&gone, 3), (&kept, 4)] {
            session.parties().insert(owner)?;
            session
                .party_invites()
                .insert(&invite(&owner.id, fill, 1))?;
            session.join_approvals().put(
                &owner.id,
                &JoinApproval {
                    invite: mailbox(fill),
                    asked_at: UnixSeconds::new(2),
                    request: request(fill.saturating_add(10), "Ben")?,
                },
            )?;
            session.joined_via().put(
                &owner.id,
                JoinedVia {
                    inviter: LeafIndex::new(0),
                    code: JoinCode::from_value(1).ok_or(StoreError::Internal)?,
                },
            )?;
        }
        session.parties().delete(&gone.id)?;
        for table in ["party_invite", "join_approval", "joined_via"] {
            assert_eq!(fixture.rows(table)?, 1, "{table}");
        }
        Ok(())
    }
}
