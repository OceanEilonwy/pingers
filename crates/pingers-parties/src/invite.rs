// SPDX-License-Identifier: AGPL-3.0-only
//! Invite records and the values the invite API returns.

use core::time::Duration;

use pingers_proto::{DisplayName, InviteUses, MailboxId};
use pingers_time::UnixSeconds;
use zeroize::Zeroizing;

use crate::PartiesError;
use crate::codec::{Reader, Writer};

/// How long an invite link is valid (design G.1 product rule: one use or 24 hours).
pub const INVITE_LIFETIME: Duration = Duration::from_hours(24);

/// An invite, named by its mailbox (each invite has its own anonymous mailbox).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InviteId(MailboxId);

impl InviteId {
    /// The invite for a mailbox, as pingers-sync reports posts to it.
    #[must_use]
    pub const fn for_mailbox(mailbox: MailboxId) -> Self {
        Self(mailbox)
    }

    /// The invite's mailbox.
    #[must_use]
    pub const fn mailbox(self) -> MailboxId {
        self.0
    }
}

/// Where an invite stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InviteStatus {
    /// Join requests are accepted.
    Open,
    /// A single-use invite admitted its one joiner.
    Used,
    /// The 24 hours (or the mailbox's lifetime, if shorter) are over.
    Expired,
    /// The inviter revoked it.
    Revoked,
}

/// One invite as the invite screen lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InviteSummary {
    /// The invite.
    pub invite: InviteId,
    /// Single or multiple use.
    pub uses: InviteUses,
    /// When it stops accepting join requests.
    pub expires: UnixSeconds,
    /// Its status now.
    pub status: InviteStatus,
}

/// A join request waiting for the inviter's approval (multi-use invites), named by the
/// joiner's reply mailbox. On the joiner's side, the join this device started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JoinId(MailboxId);

impl JoinId {
    pub(crate) const fn new(reply_mailbox: MailboxId) -> Self {
        Self(reply_mailbox)
    }

    /// The joiner's reply mailbox, where the Welcome goes.
    #[must_use]
    pub const fn reply_mailbox(self) -> MailboxId {
        self.0
    }
}

/// A join request shown as "Bob wants to join".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingJoin {
    /// Approve or decline it by this id.
    pub join: JoinId,
    /// The name the joiner gave.
    pub name: DisplayName,
}

/// Why a join request was turned away without a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The invite expired.
    Expired,
    /// The single-use invite was already used.
    AlreadyUsed,
    /// The inviter revoked the invite.
    Revoked,
}

/// The stored part of an invite's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoredStatus {
    Open,
    Used,
    Revoked,
}

/// An invite as this device stores it. The secret is kept until the invite is deleted with
/// its Party; the store is encrypted at rest.
pub(crate) struct InviteRecord {
    pub(crate) secret: Zeroizing<[u8; 32]>,
    pub(crate) uses: InviteUses,
    pub(crate) expires: UnixSeconds,
    pub(crate) status: StoredStatus,
}

const VERSION: u8 = 1;

impl InviteRecord {
    pub(crate) fn status_at(&self, now: UnixSeconds) -> InviteStatus {
        match self.status {
            StoredStatus::Used => InviteStatus::Used,
            StoredStatus::Revoked => InviteStatus::Revoked,
            StoredStatus::Open if now >= self.expires => InviteStatus::Expired,
            StoredStatus::Open => InviteStatus::Open,
        }
    }

    pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut writer = Writer::new();
        writer.byte(VERSION);
        writer.fixed(self.secret.as_slice());
        writer.byte(match self.uses {
            InviteUses::Multiple => 0,
            InviteUses::Single => 1,
        });
        writer.u64(self.expires.get());
        writer.byte(match self.status {
            StoredStatus::Open => 0,
            StoredStatus::Used => 1,
            StoredStatus::Revoked => 2,
        });
        Zeroizing::new(writer.finish())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "invite record");
        if reader.byte()? != VERSION {
            return Err(reader.malformed());
        }
        let secret = Zeroizing::new(reader.fixed()?);
        let uses = match reader.byte()? {
            0 => InviteUses::Multiple,
            1 => InviteUses::Single,
            _ => return Err(reader.malformed()),
        };
        let expires = UnixSeconds::new(reader.u64()?);
        let status = match reader.byte()? {
            0 => StoredStatus::Open,
            1 => StoredStatus::Used,
            2 => StoredStatus::Revoked,
            _ => return Err(reader.malformed()),
        };
        reader.end()?;
        Ok(Self {
            secret,
            uses,
            expires,
            status,
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::test_support::TestError;

    fn record(status: StoredStatus) -> InviteRecord {
        InviteRecord {
            secret: Zeroizing::new([9; 32]),
            uses: InviteUses::Single,
            expires: UnixSeconds::new(100),
            status,
        }
    }

    #[test]
    fn records_round_trip() -> Result<(), TestError> {
        for status in [
            StoredStatus::Open,
            StoredStatus::Used,
            StoredStatus::Revoked,
        ] {
            for uses in [InviteUses::Single, InviteUses::Multiple] {
                let mut original = record(status);
                original.uses = uses;
                let decoded = InviteRecord::decode(&original.encode())?;
                assert_eq!(*decoded.secret, *original.secret);
                assert_eq!(decoded.uses, uses);
                assert_eq!(decoded.expires, original.expires);
                assert_eq!(decoded.status, status);
            }
        }
        let bytes = record(StoredStatus::Open).encode();
        for index in [0, 33, bytes.len().saturating_sub(1)] {
            let mut changed = bytes.to_vec();
            if let Some(byte) = changed.get_mut(index) {
                *byte = 7;
            }
            assert!(InviteRecord::decode(&changed).is_err());
        }
        Ok(())
    }

    #[test]
    fn open_invites_expire_at_their_deadline_and_other_states_stay() {
        let open = record(StoredStatus::Open);
        assert_eq!(open.status_at(UnixSeconds::new(99)), InviteStatus::Open);
        assert_eq!(open.status_at(UnixSeconds::new(100)), InviteStatus::Expired);
        assert_eq!(
            record(StoredStatus::Used).status_at(UnixSeconds::new(500)),
            InviteStatus::Used
        );
        assert_eq!(
            record(StoredStatus::Revoked).status_at(UnixSeconds::new(1)),
            InviteStatus::Revoked
        );
    }
}
