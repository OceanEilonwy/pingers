// SPDX-License-Identifier: AGPL-3.0-only
//! Primitives and key schedule.
//!
//! The only crate that calls cryptographic primitives (style guide §8). Other crates call the
//! named functions here (`derive_spot_key`, `seal_push_token`, ...); domain-separation labels
//! (`pingers/v1/...`) exist only here, as private constants.
//!
//! - Presence key schedule from the Party exporter: `K_spot`, `K_tag`, record tags, `K_seal`,
//!   the board beacon seal, `K_knock`, the meetup PSK (design B.4.1, B.4.2, B.5).
//! - Knock sealing and signatures (B.4.2), revoke commitments, segment hashes (B.4.3).
//! - Mailbox ids from read keys and sealed push tokens to the push relay (B.4.9).
//! - Group state snapshots and log-auth keys from the MLS exporter (B.2).
//! - Location keys, grants and asks inside a Meetup (B.6.1).
//! - Invite join requests (D).
//!
//! Secret types wrap `zeroize::Zeroizing` and implement neither `Debug`, `Display`, `Clone`
//! nor `Serialize`. MACs and tags are compared with `subtle`, never `==`.
//! Randomness is always passed in as `&mut impl CryptoRng`.
#![forbid(unsafe_code)]

mod board;
mod error;
mod group;
mod hpke;
mod identity;
mod invite;
mod labels;
mod location;
mod mailbox;
#[cfg(test)]
mod negative_tests;
mod presence;
mod primitives;
mod secret;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod vector_tests;

pub use board::{RevokeKey, revoke_commitment, segment_hash};
pub use error::CryptoError;
pub use group::{
    GroupRequest, LogAuthKeyPair, StateKey, open_state_snapshot, seal_state_snapshot,
    sign_group_request, verify_group_request,
};
pub use identity::{
    AccountPublicKey, AccountSigningKey, Signature, account_id, sign_registration,
    verify_registration,
};
pub use invite::{InviteSecret, open_join_request, seal_join_request};
pub use labels::ExporterLabel;
pub use location::{
    GrantContext, GrantKeyPair, GrantPublicKey, GrantSecretKey, LocationAad, LocationAsk,
    LocationGrant, LocationKey, LocationToSeal, OpenedGrant, SEALED_LOCATION_LENGTH,
    dummy_location, open_location, open_location_ask, open_location_grant, seal_location,
    seal_location_ask, seal_location_grant,
};
pub use mailbox::{
    MailboxReadKey, PushBind, PushRelayPrivateKey, PushRelayPublicKey, PushTokenToSeal,
    key_config_hash, mailbox_id, open_push_token, push_bind_for_mailbox,
    push_bind_for_subscription, seal_push_token,
};
pub use presence::{
    BeaconHeader, KnockKey, KnockPlaintext, KnockSignedFields, MeetupPsk, PresenceRoot, SealKey,
    SpotKey, TagKey, derive_knock_key, derive_meetup_psk, derive_seal_key, derive_spot_key,
    derive_tag_key, open_board_beacon, open_knock, record_tag, seal_board_beacon, seal_knock,
    sign_knock, tag_matches, verify_knock,
};
