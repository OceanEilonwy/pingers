// SPDX-License-Identifier: AGPL-3.0-only
//! Invite records and the values the invite API returns (join flow A).

use core::time::Duration;

use pingers_crypto::JoinCode;
use pingers_proto::{DisplayName, MailboxId, SequenceNumber};
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
    /// Someone was let in; the link stopped working.
    Used,
    /// The 24 hours (or the mailbox's lifetime, if shorter) are over.
    Expired,
    /// The inviter cancelled it.
    Revoked,
}

/// One invite as the invite screen lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InviteSummary {
    /// The invite.
    pub invite: InviteId,
    /// When the link was made ("from the link you shared 2 h ago").
    pub shared_at: UnixSeconds,
    /// When it stops accepting join requests.
    pub expires: UnixSeconds,
    /// Its status now.
    pub status: InviteStatus,
}

/// A join request waiting for the inviter, named by the joiner's reply mailbox. On the
/// joiner's side, the join this device started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JoinId(MailboxId);

impl JoinId {
    pub(crate) const fn new(reply_mailbox: MailboxId) -> Self {
        Self(reply_mailbox)
    }

    /// The join whose Welcome goes to `reply_mailbox`, as the joiner recorded it (a join
    /// survives a restart of the app).
    #[must_use]
    pub const fn for_reply_mailbox(reply_mailbox: MailboxId) -> Self {
        Self(reply_mailbox)
    }

    /// The joiner's reply mailbox, where the Welcome goes.
    #[must_use]
    pub const fn reply_mailbox(self) -> MailboxId {
        self.0
    }
}

/// A join request shown as "Ben asked to join", with the code Ben reads out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingJoin {
    /// Let in or turn away by this id.
    pub join: JoinId,
    /// The invite it came through.
    pub invite: InviteId,
    /// The name the joiner gave.
    pub name: DisplayName,
    /// The code the joiner's phone shows; they must match.
    pub code: JoinCode,
    /// When this device read the request.
    pub asked_at: UnixSeconds,
}

/// Why a join request was turned away without asking the inviter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// The invite expired.
    Expired,
    /// Someone was already let in through it.
    AlreadyUsed,
    /// The inviter cancelled the invite.
    Revoked,
    /// The Party is quantum-resistant and the request carries no X-Wing key package (the
    /// joiner's build cannot make one).
    NoQuantumResistantKeyPackage,
}

/// Why a post to an invite mailbox changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ignored {
    /// It was read before (its sequence number is not past the invite's read position).
    AlreadySeen,
    /// It does not open under the invite's secret, or does not decode: not from a link
    /// holder. Anyone who knows the mailbox id can post to it.
    Unreadable,
    /// A withdrawal of a request that is not waiting (turned away, let in, or never seen).
    NotPending,
}

/// The stored part of an invite's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoredStatus {
    Open,
    /// Let in the joiner whose Welcome goes to this reply mailbox.
    Used(MailboxId),
    Revoked,
}

/// An invite as this device stores it. The secret is kept until the invite is deleted with
/// its Party; the store is encrypted at rest.
pub(crate) struct InviteRecord {
    pub(crate) secret: Zeroizing<[u8; 32]>,
    pub(crate) shared_at: UnixSeconds,
    pub(crate) expires: UnixSeconds,
    pub(crate) status: StoredStatus,
    /// The last post read from the invite mailbox, so a read resumes after it.
    pub(crate) read_position: Option<SequenceNumber>,
}

const VERSION: u8 = 2;

impl InviteRecord {
    pub(crate) fn status_at(&self, now: UnixSeconds) -> InviteStatus {
        match self.status {
            StoredStatus::Used(_) => InviteStatus::Used,
            StoredStatus::Revoked => InviteStatus::Revoked,
            StoredStatus::Open if now >= self.expires => InviteStatus::Expired,
            StoredStatus::Open => InviteStatus::Open,
        }
    }

    pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut writer = Writer::new();
        writer.byte(VERSION);
        writer.fixed(self.secret.as_slice());
        writer.u64(self.shared_at.get());
        writer.u64(self.expires.get());
        match self.status {
            StoredStatus::Open => writer.byte(0),
            StoredStatus::Used(reply) => {
                writer.byte(1);
                writer.fixed(reply.as_bytes());
            }
            StoredStatus::Revoked => writer.byte(2),
        }
        match self.read_position {
            None => writer.byte(0),
            Some(position) => {
                writer.byte(1);
                writer.u64(position.get());
            }
        }
        Zeroizing::new(writer.finish())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, PartiesError> {
        let mut reader = Reader::new(bytes, "invite record");
        if reader.byte()? != VERSION {
            return Err(reader.malformed());
        }
        let secret = Zeroizing::new(reader.fixed()?);
        let shared_at = UnixSeconds::new(reader.u64()?);
        let expires = UnixSeconds::new(reader.u64()?);
        let status = match reader.byte()? {
            0 => StoredStatus::Open,
            1 => StoredStatus::Used(MailboxId::from_bytes(reader.fixed()?)),
            2 => StoredStatus::Revoked,
            _ => return Err(reader.malformed()),
        };
        let read_position = match reader.byte()? {
            0 => None,
            1 => Some(SequenceNumber::new(reader.u64()?)),
            _ => return Err(reader.malformed()),
        };
        reader.end()?;
        Ok(Self {
            secret,
            shared_at,
            expires,
            status,
            read_position,
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
            shared_at: UnixSeconds::new(40),
            expires: UnixSeconds::new(100),
            status,
            read_position: None,
        }
    }

    #[test]
    fn records_round_trip() -> Result<(), TestError> {
        for status in [
            StoredStatus::Open,
            StoredStatus::Used(MailboxId::from_bytes([3; 16])),
            StoredStatus::Revoked,
        ] {
            for read_position in [None, Some(SequenceNumber::new(7))] {
                let mut original = record(status);
                original.read_position = read_position;
                let decoded = InviteRecord::decode(&original.encode())?;
                assert_eq!(*decoded.secret, *original.secret);
                assert_eq!(decoded.shared_at, original.shared_at);
                assert_eq!(decoded.expires, original.expires);
                assert_eq!(decoded.status, status);
                assert_eq!(decoded.read_position, read_position);
            }
        }
        let bytes = record(StoredStatus::Open).encode();
        // The version, status and read-position bytes.
        for index in [0, 49, bytes.len().saturating_sub(1)] {
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
            record(StoredStatus::Used(MailboxId::from_bytes([1; 16])))
                .status_at(UnixSeconds::new(500)),
            InviteStatus::Used
        );
        assert_eq!(
            record(StoredStatus::Revoked).status_at(UnixSeconds::new(1)),
            InviteStatus::Revoked
        );
    }
}
