// SPDX-License-Identifier: AGPL-3.0-only
//! The MLS delivery service's envelopes: group logs, commits and live frames (design D).

use crate::stub::not_implemented;
use crate::{
    Epoch, GroupId, ProtoError, SequenceNumber, Subscription, SubscriptionId, Urgency, WireDecode,
    WireEncode,
};

/// Defines an opaque, variable-length byte string whose `Debug` prints only its length.
macro_rules! opaque_vec {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name(Vec<u8>);

        impl $name {
            /// Wraps bytes.
            #[must_use]
            pub const fn from_bytes(bytes: Vec<u8>) -> Self {
                Self(bytes)
            }

            /// The bytes.
            #[must_use]
            pub fn as_bytes(&self) -> &[u8] {
                &self.0
            }

            /// The bytes, by value.
            #[must_use]
            pub fn into_bytes(self) -> Vec<u8> {
                self.0
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(formatter, concat!(stringify!($name), "({} bytes)"), self.0.len())
            }
        }
    };
}

opaque_vec!(
    /// A TLS-serialised MLS `KeyPackage`.
    KeyPackageBytes
);
opaque_vec!(
    /// A TLS-serialised MLS message (`PrivateMessage` for everything Pingers sends).
    MlsMessageBytes
);
opaque_vec!(
    /// A TLS-serialised MLS Welcome, delivered as a mailbox post.
    WelcomeBytes
);
opaque_vec!(
    /// A group state snapshot sealed under the epoch's state key (design B.2).
    SealedSnapshot
);

/// The Ed25519 public key that authorises reads and writes of a group log for one epoch
/// (design B.2, "log-auth").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GroupAuthKey([u8; 32]);

impl GroupAuthKey {
    /// Wraps key bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The MLS `content_type` from a message's cleartext header, which the server uses to order
/// commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentType {
    /// Application data.
    Application,
    /// A proposal.
    Proposal,
    /// A commit.
    Commit,
}

/// Whether the server keeps a group message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Delivery {
    /// Stored in the group log until the group ends (chat, meet point, commits).
    Stored,
    /// Forwarded to live channels and dropped (locations, keep-alives); never pushed.
    Ephemeral,
}

/// `POST /v1/groups` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateGroup {
    /// Random 32-byte id.
    pub group_id: GroupId,
    /// The first epoch's log-auth key.
    pub auth_key: GroupAuthKey,
    /// The creator's (and first members') subscriptions.
    pub subscriptions: Vec<Subscription>,
    /// The first state snapshot, if any.
    pub state: Option<SealedSnapshot>,
}

/// `POST /v1/groups/{id}/commit` (design D): exactly one commit per epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRequest {
    /// The epoch the commit was made in.
    pub epoch: Epoch,
    /// The commit.
    pub commit: MlsMessageBytes,
    /// The next epoch's log-auth key.
    pub next_auth_key: GroupAuthKey,
    /// The snapshot for new members.
    pub state: Option<SealedSnapshot>,
    /// Subscriptions of members being added.
    pub subscriptions_added: Vec<Subscription>,
    /// Subscriptions of members being removed.
    pub subscriptions_removed: Vec<SubscriptionId>,
    /// How to wake the other members.
    pub wake: Urgency,
}

/// The server's answer to a commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitResponse {
    /// The commit is the epoch's one commit, stored at this position.
    Accepted {
        /// Its log position.
        sequence: SequenceNumber,
    },
    /// Another commit won the epoch (HTTP 409); process the log and retry.
    Conflict {
        /// The log's current epoch.
        head_epoch: Epoch,
    },
}

/// `POST /v1/groups/{id}/msgs` (design D): the group message envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppMessage {
    /// The epoch the message was sent in.
    pub epoch: Epoch,
    /// The MLS `PrivateMessage`.
    pub body: MlsMessageBytes,
    /// Stored or ephemeral.
    pub delivery: Delivery,
    /// Whether, and how urgently, to wake subscribers who are not on the live channel.
    pub wake: Option<Urgency>,
}

/// One entry of `GET /v1/groups/{id}/msgs` (design D, `GrpMsg`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupLogEntry {
    /// Log position.
    pub sequence: SequenceNumber,
    /// The epoch from the cleartext header.
    pub epoch: Epoch,
    /// The content type from the cleartext header.
    pub content_type: ContentType,
    /// The MLS message.
    pub body: MlsMessageBytes,
}

/// One frame on a Meetup's chunked OHTTP live channel, in either direction (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveFrame {
    /// The epoch the message was sent in.
    pub epoch: Epoch,
    /// The MLS `PrivateMessage`, whose plaintext is a padded meetup frame.
    pub body: MlsMessageBytes,
    /// Stored or ephemeral.
    pub delivery: Delivery,
}

/// Implements the CBOR codec contract with stub bodies for each listed type.
macro_rules! cbor_stub {
    ($($name:ty),* $(,)?) => {
        $(
            impl WireEncode for $name {
                fn encode(&self) -> Result<Vec<u8>, ProtoError> {
                    not_implemented(self)
                }
            }

            impl WireDecode for $name {
                fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
                    not_implemented(bytes)
                }
            }
        )*
    };
}

cbor_stub!(
    CreateGroup,
    CommitRequest,
    CommitResponse,
    AppMessage,
    GroupLogEntry,
    LiveFrame
);
