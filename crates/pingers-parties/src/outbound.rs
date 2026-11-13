// SPDX-License-Identifier: AGPL-3.0-only
//! What this crate asks pingers-sync to send. Nothing here touches the network.

use pingers_crypto::LogAuthKeyPair;
use pingers_proto::{
    AppMessage, CommitRequest, CreateGroup, GroupId, MailboxId, Padded, PartyId, SubscriptionId,
};

/// A request for pingers-sync to send. Group requests (other than [`Outbound::Leave`]) are
/// signed with the group's current log-auth key, which pingers-sync gets from
/// `PartyLifecycle::log_auth_key` (design D, Grp auth).
#[non_exhaustive]
pub enum Outbound {
    /// `POST /v1/groups`: register a new Party's group with its first subscription and
    /// snapshot. Anonymous (scope Group); there is no log-auth signature yet.
    CreateGroup(CreateGroup),
    /// `POST /v1/groups/{id}/commit`, then, once accepted, the Welcome.
    Commit(OutgoingCommit),
    /// `POST /v1/groups/{id}/msgs`: a Party state operation (stored, quiet wake, design G.2).
    GroupMessage {
        /// The Party's group.
        group: GroupId,
        /// The message.
        message: AppMessage,
    },
    /// `POST /v1/mailboxes/{id}/msgs` (anonymous, scope Mailbox).
    MailboxPost {
        /// The mailbox to post to.
        mailbox: MailboxId,
        /// The body, exactly one mailbox size class long.
        body: Padded,
    },
    /// `DELETE /v1/mailboxes/{id}` with the mailbox's read key, which pingers-sync reads from
    /// the store's `mailbox` table; then delete the row.
    DeleteMailbox {
        /// The mailbox.
        mailbox: MailboxId,
    },
    /// Leaving a Party: post the self-remove proposal, then delete this device's
    /// subscription. The group is already gone from this device, so the log-auth key travels
    /// with the request.
    Leave(LeaveRequest),
}

/// A commit this device made, waiting for the server.
///
/// Post `request` to the group's commit endpoint. On `200`, call
/// `PartyLifecycle::commit_accepted` and then post `welcome` if there is one; on `409`, call
/// `PartyLifecycle::commit_rejected`, catch up on the log, and retry the operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingCommit {
    /// The Party.
    pub party: PartyId,
    /// Its group.
    pub group: GroupId,
    /// The commit request, with the next epoch's log-auth key and snapshot.
    pub request: CommitRequest,
    /// The Welcome for an added member, to post only after the commit is accepted.
    pub welcome: Option<WelcomeDelivery>,
}

/// A Welcome and the reply mailbox it goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WelcomeDelivery {
    /// The joiner's one-time reply mailbox.
    pub mailbox: MailboxId,
    /// The Welcome, padded to a mailbox size class.
    pub body: Padded,
}

/// What pingers-sync sends when this device leaves a Party.
pub struct LeaveRequest {
    /// The group.
    pub group: GroupId,
    /// The self-remove proposal, for `POST /v1/groups/{id}/msgs`.
    pub proposal: AppMessage,
    /// This device's subscription, to delete with `DELETE /v1/groups/{id}/subs/{sub_id}`.
    pub subscription: Option<SubscriptionId>,
    /// The log-auth key of the epoch the proposal was made in, to sign both requests.
    pub auth: LogAuthKeyPair,
}
