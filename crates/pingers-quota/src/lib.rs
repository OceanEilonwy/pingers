// SPDX-License-Identifier: AGPL-3.0-only
//! Server: RLN tree, nullifiers, Privacy Pass issuer, limits.
//!
//! Who may post (design B.1, B.4.8, C.1, D):
//!
//! - [`Accounts`]: registration (proof of key possession, an operator's registration code
//!   for new accounts, at most so many new accounts an hour) and the hour-long bearers of the
//!   Acct credential, both persistent.
//! - [`Membership`]: the RLN membership tree, one live leaf per registered account (at most
//!   [`DAILY_LEAF_LIMIT`] registered per account a day), batched into
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
//! Storage is [`AccountStore`], [`MembershipStore`] and [`PrivacyPassStore`], all implemented by
//! [`MemoryQuotaStore`] and, behind the `sqlite` feature, `SqliteQuotaStore`. Share one store
//! between the services through an `Arc`.
#![forbid(unsafe_code)]

mod accounts;
mod attempts;
mod error;
mod membership;
mod memory;
mod nullifiers;
mod privacy_pass;
#[cfg(feature = "sqlite")]
mod sqlite;
mod store;

pub use accounts::{
    AccountPolicy, Accounts, BEARER_LIFETIME, BEARERS_PER_ACCOUNT, CHALLENGE_GRACE, SIGNUP_WINDOW,
    SignupsPerHour,
};
pub use attempts::{
    CODE_ATTEMPT_WINDOW, CODE_ATTEMPTS_PER_ADDRESS, CODE_ATTEMPTS_PER_KEY, ClientAddress,
    MAX_TRACKED,
};
pub use error::QuotaError;
pub use membership::{
    DAILY_LEAF_LIMIT, LEAVES_PER_PAGE, Membership, ROOT_EPOCH_LENGTH, root_epoch,
};
pub use memory::MemoryQuotaStore;
pub use nullifiers::{Admission, BUCKET_GRACE, Nullifiers};
pub use privacy_pass::{
    DAILY_TOKEN_LIMIT, ISSUANCE_PERIOD, Issued, IssuerPublicKey, PrivacyPass, REDEMPTION_GRACE,
};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteQuotaStore;
pub use store::{
    AccountInsert, AccountStore, Allowance, Issuance, IssuanceDay, IssuerKeyRecord,
    MembershipStore, NewAccount, PrivacyPassStore, SignupWindow, Spend, StoredBearer, StoredLeaf,
    TokenNonce, Zeroing,
};
