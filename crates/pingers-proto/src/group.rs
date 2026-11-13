// SPDX-License-Identifier: AGPL-3.0-only
//! The MLS delivery service's envelopes: group logs, commits and live frames (design D).

use minicbor::Decoder;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::{Epoch, GroupId, ProtoError, SequenceNumber, Subscription, SubscriptionId, Urgency};

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

impl ContentType {
    /// Writes the MLS wire value: 1 application, 2 proposal, 3 commit (RFC 9420).
    fn encode_cbor(self, writer: &mut Writer) {
        writer.unsigned(match self {
            Self::Application => 1_u8,
            Self::Proposal => 2,
            Self::Commit => 3,
        });
    }

    fn decode_cbor(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        match decoder.u8()? {
            1 => Ok(Self::Application),
            2 => Ok(Self::Proposal),
            3 => Ok(Self::Commit),
            _ => Err(ProtoError::OutOfRange {
                field: "content type",
            }),
        }
    }
}

impl Delivery {
    /// Writes the wire value: 0 stored, 1 ephemeral.
    fn encode_cbor(self, writer: &mut Writer) {
        writer.unsigned(match self {
            Self::Stored => 0_u8,
            Self::Ephemeral => 1,
        });
    }

    fn decode_cbor(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        match decoder.u8()? {
            0 => Ok(Self::Stored),
            1 => Ok(Self::Ephemeral),
            _ => Err(ProtoError::OutOfRange { field: "delivery" }),
        }
    }
}

/// Reads an MLS message.
fn read_message(decoder: &mut Decoder<'_>) -> Result<MlsMessageBytes, ProtoError> {
    Ok(MlsMessageBytes::from_bytes(decoder.bytes()?.to_vec()))
}

/// Reads an optional snapshot.
fn read_snapshot(decoder: &mut Decoder<'_>) -> Result<Option<SealedSnapshot>, ProtoError> {
    cbor::option(decoder, |decoder| {
        Ok(SealedSnapshot::from_bytes(decoder.bytes()?.to_vec()))
    })
}

impl CborBody for SubscriptionId {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.bytes(self.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::fixed_bytes(decoder).map(Self::from_bytes)
    }
}

/// Layout: `[group id, auth key, subscriptions, state or null]`.
impl CborBody for CreateGroup {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.bytes(self.group_id.as_bytes())?;
        writer.bytes(self.auth_key.as_bytes())?;
        writer.list(&self.subscriptions)?;
        writer.option(self.state.as_ref(), |writer, state| {
            writer.bytes(state.as_bytes())
        })
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            group_id: GroupId::from_bytes(cbor::fixed_bytes(decoder)?),
            auth_key: GroupAuthKey::from_bytes(cbor::fixed_bytes(decoder)?),
            subscriptions: cbor::list(decoder)?,
            state: read_snapshot(decoder)?,
        })
    }
}

/// Layout: `[epoch, commit, next auth key, state or null, subscriptions added, subscription ids
/// removed, wake urgency]`.
impl CborBody for CommitRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(7)?;
        writer.unsigned(self.epoch.get());
        writer.bytes(self.commit.as_bytes())?;
        writer.bytes(self.next_auth_key.as_bytes())?;
        writer.option(self.state.as_ref(), |writer, state| {
            writer.bytes(state.as_bytes())
        })?;
        writer.list(&self.subscriptions_added)?;
        writer.list(&self.subscriptions_removed)?;
        self.wake.encode_cbor(writer);
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 7)?;
        Ok(Self {
            epoch: Epoch::new(decoder.u64()?),
            commit: read_message(decoder)?,
            next_auth_key: GroupAuthKey::from_bytes(cbor::fixed_bytes(decoder)?),
            state: read_snapshot(decoder)?,
            subscriptions_added: cbor::list(decoder)?,
            subscriptions_removed: cbor::list(decoder)?,
            wake: Urgency::decode_cbor(decoder)?,
        })
    }
}

/// Layout: `[0, sequence]` for Accepted, `[1, head epoch]` for Conflict.
impl CborBody for CommitResponse {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        match self {
            Self::Accepted { sequence } => {
                writer.unsigned(0_u8);
                writer.unsigned(sequence.get());
            }
            Self::Conflict { head_epoch } => {
                writer.unsigned(1_u8);
                writer.unsigned(head_epoch.get());
            }
        }
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        match decoder.u8()? {
            0 => Ok(Self::Accepted {
                sequence: SequenceNumber::new(decoder.u64()?),
            }),
            1 => Ok(Self::Conflict {
                head_epoch: Epoch::new(decoder.u64()?),
            }),
            _ => Err(ProtoError::OutOfRange {
                field: "commit response",
            }),
        }
    }
}

/// Layout: `[epoch, body, delivery, wake urgency or null]`.
impl CborBody for AppMessage {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.unsigned(self.epoch.get());
        writer.bytes(self.body.as_bytes())?;
        self.delivery.encode_cbor(writer);
        writer.option(self.wake.as_ref(), |writer, wake| {
            wake.encode_cbor(writer);
            Ok(())
        })
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            epoch: Epoch::new(decoder.u64()?),
            body: read_message(decoder)?,
            delivery: Delivery::decode_cbor(decoder)?,
            wake: cbor::option(decoder, Urgency::decode_cbor)?,
        })
    }
}

/// Layout: `[sequence, epoch, content type, body]`.
impl CborBody for GroupLogEntry {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.unsigned(self.sequence.get());
        writer.unsigned(self.epoch.get());
        self.content_type.encode_cbor(writer);
        writer.bytes(self.body.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            sequence: SequenceNumber::new(decoder.u64()?),
            epoch: Epoch::new(decoder.u64()?),
            content_type: ContentType::decode_cbor(decoder)?,
            body: read_message(decoder)?,
        })
    }
}

/// Layout: `[epoch, body, delivery]`.
impl CborBody for LiveFrame {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(3)?;
        writer.unsigned(self.epoch.get());
        writer.bytes(self.body.as_bytes())?;
        self.delivery.encode_cbor(writer);
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 3)?;
        Ok(Self {
            epoch: Epoch::new(decoder.u64()?),
            body: read_message(decoder)?,
            delivery: Delivery::decode_cbor(decoder)?,
        })
    }
}

versioned_cbor!(
    CreateGroup,
    CommitRequest,
    CommitResponse,
    AppMessage,
    GroupLogEntry,
    LiveFrame
);
