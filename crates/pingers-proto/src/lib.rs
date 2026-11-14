// SPDX-License-Identifier: AGPL-3.0-only
//! Every type that crosses the network or the FFI boundary in serialised form.
//!
//! Board record (96 B), lease (4 slots), knock, mailbox registration with sealed push, group
//! message envelope, meetup frame, API request and response structs. Canonical encoding: a
//! fixed byte layout for the fixed-size records ([`Record`], [`BoardBeacon`], [`Lease`],
//! [`RlnAuth`]) and CBOR (`minicbor`) for everything else; padding to size classes
//! (256 B, 1 KiB, 4 KiB, and 8 KiB and 32 KiB for mailbox posts).
//!
//! No crypto and no logic beyond encode, decode and validate lengths. Sealing, opening and
//! signing are pingers-crypto's job; this crate only carries the resulting bytes.
//!
//! Identifiers ([`PartyId`], [`SpotId`], [`MailboxId`], ...) live here because they are the
//! wire's vocabulary; every other crate uses these types rather than defining its own.
#![forbid(unsafe_code)]

mod api;
mod auth;
mod board;
mod cbor;
mod codec;
mod error;
mod group;
mod ids;
mod invite;
mod key_config;
mod knock;
mod meetup;
mod padding;
mod privacy_pass;
mod push;
mod redacted;
mod relay;
mod secret;

pub use api::{
    AnonymousCredential, AuthRequest, AuthResponse, BoardSyncRequest, BoardSyncResponse,
    CoverReadRatio, CustomPushRelays, GroupMessages, GroupState, HourlyRoot, LeaseReplacement,
    MailboxMessage, MailboxMessages, MapHostUrl, MembershipIndex, MembershipLeafRequest,
    MembershipLeafResponse, MembershipLeaves, MembershipRoot, MessageAccepted, Params,
    PushRelayInfo, ReadBoard, RegisterRequest, RegisterResponse, RootEpoch, Slashing,
};
pub use auth::{RlnAuth, RlnScope, ServerId};
pub use board::{
    BoardBeacon, BoardCursor, BoardPage, Lease, MeetupHint, Record, RecordTag, RevokeCommitment,
    SealedBeacon, SegmentHash, SegmentInfo,
};
pub use codec::{WireDecode, WireEncode};
pub use error::ProtoError;
pub use group::{
    AppMessage, CommitRequest, CommitResponse, ContentType, CreateGroup, Delivery, GroupAuthKey,
    GroupLogEntry, KeyPackageBytes, LiveFrame, MlsMessageBytes, SealedSnapshot, WelcomeBytes,
};
pub use ids::{
    AccountId, ChatMessageId, Epoch, GroupId, Lamport, LeafIndex, LeaseId, LocationGeneration,
    LocationRequestId, MailboxId, PartyId, PushHandle, RecordRandomness, RelayId, SequenceNumber,
    SpotId, SubscriptionId,
};
pub use invite::{DisplayName, InvitePayload, InviteUses, InviterFingerprint, JoinRequest};
pub use key_config::SingleSuiteKeyConfig;
pub use knock::{Knock, KnockSignature};
pub use meetup::{
    AccuracyMeters, ChatBody, E7Coordinate, HeadingDegrees, LocationPlaintext, MeetPoint,
    MeetPointLabel, MeetupMessage, SealedAsk, SealedGrant, SealedLocationFrame, SeenSegment,
    SpeedCentimetresPerSecond,
};
pub use padding::{Padded, SizeClass};
pub use privacy_pass::{
    MAX_TOKENS_PER_ISSUANCE, PrivacyPassAllowance, PrivacyPassIssueRequest,
    PrivacyPassIssueResponse, PrivacyPassKey, PrivacyPassKeys, TokenAllowance,
};
pub use push::{
    KeyConfigHash, MailboxSpec, PushBind, PushClass, PushPlatform, PushToken, PushTokenPlaintext,
    RelayRef, RelayUrl, SealedPush, SealedPushBlob, Subscription, TimeToLive, Urgency,
};
pub use redacted::Redacted;
pub use relay::{PushRelayConfig, PushRelayKeyConfig, PushRelayKeyId, PushRequest};
pub use secret::{SecretBytes, SecretText};
