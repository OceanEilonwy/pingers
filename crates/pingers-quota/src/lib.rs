// SPDX-License-Identifier: AGPL-3.0-only
//! Server: RLN tree, nullifiers, Privacy Pass issuer, limits.
//!
//! Who may post (design B.4.8, C.1, D):
//!
//! - [`Membership`]: the RLN membership tree, one leaf per registered account, batched into
//!   hourly root epochs. The public leaf list shows only what the current root contains; the
//!   current and previous roots are accepted.
//! - [`Nullifiers`]: RLN admission. Bucket (current, or previous within 60 s), root, request
//!   digest and proof, then the nullifier: new is accepted, the same request again is a replay,
//!   a different request with the same nullifier is rejected. Slashing is off, so that share is
//!   ignored. The per-bucket limit of 8 per scope is the circuit's message limit. In memory
//!   only.
//! - [`PrivacyPass`]: weekly issuer keys, a daily allowance of 300 tokens per account, and the
//!   spent set.
//!
//! Storage is [`MembershipStore`] and [`PrivacyPassStore`], both implemented by
//! [`MemoryQuotaStore`] and, behind the `sqlite` feature, `SqliteQuotaStore`. Share one store
//! between both services through an `Arc`.
#![forbid(unsafe_code)]

mod error;
mod membership;
mod memory;
mod nullifiers;
mod privacy_pass;
#[cfg(feature = "sqlite")]
mod sqlite;
mod store;

pub use error::QuotaError;
pub use membership::{LEAVES_PER_PAGE, Membership, ROOT_EPOCH_LENGTH, root_epoch};
pub use memory::MemoryQuotaStore;
pub use nullifiers::{Admission, BUCKET_GRACE, Nullifiers};
pub use privacy_pass::{
    DAILY_TOKEN_LIMIT, ISSUANCE_PERIOD, Issued, IssuerPublicKey, PrivacyPass, REDEMPTION_GRACE,
};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteQuotaStore;
pub use store::{
    Allowance, Issuance, IssuanceDay, IssuerKeyRecord, MembershipStore, PrivacyPassStore, Spend,
    StoredLeaf, TokenNonce, Zeroing,
};
