// SPDX-License-Identifier: AGPL-3.0-only
//! Primitives and key schedule.
//!
//! The only crate that calls cryptographic primitives (style guide §8). Other crates call the
//! named functions here (`derive_spot_key`, `seal_push_token`, ...); domain-separation labels
//! (`pingers/v1/...`) exist only here, as private constants.
//!
//! - Presence key schedule from the Party exporter: `K_spot`, `K_tag`, record tags, `K_seal`,
//!   the board beacon seal, `K_knock`, the meetup PSK (design B.4.1, B.4.2, B.5).
//! - Knock sealing and signatures (B.4.2), revoke commitments, segment hashes and board
//!   cursors (B.4.3).
//! - Mailbox ids from read keys and sealed push tokens to the push relay (B.4.9).
//! - Group state snapshots and log-auth keys from the MLS exporter (B.2).
//! - Location keys, grants and asks inside a Meetup (B.6.1).
//! - Invite join requests (D).
//! - Keyed hashes for the push relay's rate limits and stored credentials (C.1a).
//! - The SHA-256 preimage hashes of RLN's external nullifier and request digest (B.4.8).
//!
//! Secret types wrap `zeroize::Zeroizing` and implement neither `Debug`, `Display`, `Clone`
//! nor `Serialize`. MACs and tags are compared with `subtle`, never `==`.
//! Randomness is always passed in as `&mut impl CryptoRng`.
#![forbid(unsafe_code)]

mod board;
mod cursor;
mod error;
mod group;
mod hpke;
mod identity;
mod invite;
mod keyed_hash;
mod labels;
mod location;
mod mailbox;
#[cfg(test)]
mod negative_tests;
mod padded;
mod presence;
mod primitives;
mod rln;
mod safety;
mod secret;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod vector_tests;

pub use board::{RevokeKey, revoke_commitment, revoke_key_matches, segment_hash};
pub use cursor::{BoardCursorKey, open_board_cursor, seal_board_cursor};
pub use error::CryptoError;
pub use group::{
    GroupRequest, LogAuthKeyPair, StateKey, open_state_snapshot, seal_state_snapshot,
    sign_group_request, verify_group_request,
};
pub use identity::{
    AccountPublicKey, AccountSigningKey, BearerDigest, BearerToken, Signature, account_id,
    bearer_digest, sign_auth_challenge, sign_registration, verify_auth_challenge,
    verify_registration,
};
pub use invite::{
    InviteSecret, JoinCode, JoinKeyPackages, join_code, open_join_request, seal_join_request,
    seal_join_request_in_class,
};
pub use keyed_hash::{KeyedHash, KeyedHashKey, KeyedHashPurpose, keyed_hash};
pub use labels::ExporterLabel;
pub use location::{
    GrantContext, GrantKeyPair, GrantPublicKey, GrantSecretKey, LocationAad, LocationAsk,
    LocationGrant, LocationKey, LocationToSeal, OpenedGrant, SEALED_LOCATION_LENGTH,
    dummy_location, open_location, open_location_ask, open_location_grant, seal_location,
    seal_location_ask, seal_location_grant,
};
pub use mailbox::{
    MailboxReadKey, PushBind, PushRelayPrivateKey, PushRelayPublicKey, PushTokenToOpen,
    PushTokenToSeal, key_config_hash, mailbox_id, open_push_token, push_bind_for_mailbox,
    push_bind_for_subscription, seal_push_token,
};
pub use presence::{
    BeaconHeader, KnockKey, KnockPlaintext, KnockSignedFields, MeetupPsk, PresenceRoot, SealKey,
    SpotKey, TagKey, derive_knock_key, derive_meetup_psk, derive_seal_key, derive_spot_key,
    derive_tag_key, open_board_beacon, open_knock, record_tag, seal_board_beacon, seal_knock,
    sign_knock, tag_matches, verify_knock,
};
pub use rln::{RlnPreimageHash, rln_external_nullifier_hash, rln_request_hash};
pub use safety::{SAFETY_NUMBER_DIGITS, SafetyNumber, inviter_fingerprint, safety_number};
