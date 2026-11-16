// SPDX-License-Identifier: AGPL-3.0-only
//! The values the invite API returns (join flow A), and a stored invite's status.

use core::time::Duration;

use pingers_crypto::JoinCode;
use pingers_proto::{DisplayName, MailboxId};
use pingers_store::{InviteState, PartyInvite};
use pingers_time::UnixSeconds;

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

/// A stored invite's status at `now`: an open invite past its deadline has expired.
pub(crate) fn status_at(invite: &PartyInvite, now: UnixSeconds) -> InviteStatus {
    match invite.state {
        InviteState::Used { .. } => InviteStatus::Used,
        InviteState::Revoked => InviteStatus::Revoked,
        InviteState::Open if now >= invite.expires => InviteStatus::Expired,
        InviteState::Open => InviteStatus::Open,
    }
}

#[cfg(test)]
mod tests {
    use pingers_crypto::InviteSecret;
    use pingers_proto::PartyId;
    use zeroize::Zeroizing;

    use super::*;

    fn invite(state: InviteState) -> PartyInvite {
        PartyInvite {
            mailbox: MailboxId::from_bytes([2; 16]),
            party: PartyId::from_bytes([1; 16]),
            secret: InviteSecret::from_bytes(Zeroizing::new([9; 32])),
            shared_at: UnixSeconds::new(40),
            expires: UnixSeconds::new(100),
            state,
            read_position: None,
        }
    }

    #[test]
    fn open_invites_expire_at_their_deadline_and_other_states_stay() {
        let open = invite(InviteState::Open);
        assert_eq!(status_at(&open, UnixSeconds::new(99)), InviteStatus::Open);
        assert_eq!(
            status_at(&open, UnixSeconds::new(100)),
            InviteStatus::Expired
        );
        assert_eq!(
            status_at(
                &invite(InviteState::Used {
                    admitted: MailboxId::from_bytes([1; 16])
                }),
                UnixSeconds::new(500)
            ),
            InviteStatus::Used
        );
        assert_eq!(
            status_at(&invite(InviteState::Revoked), UnixSeconds::new(1)),
            InviteStatus::Revoked
        );
    }
}
