// SPDX-License-Identifier: AGPL-3.0-only
//! Server: mailboxes and the wake rule.
//!
//! Anonymous mailboxes (design B.4.9, D): registered without an account, posted to by anyone
//! who knows the id, read and deleted with the read key.
//!
//! - [`Mailboxes`] holds the rules: lifetime at most 30 days, posts padded to 1, 4, 8 or
//!   32 KiB, at most [`MailboxRules::max_messages`] per mailbox, a [`PostLimit`] hook, the read
//!   key as capability, expiry.
//! - [`MailboxStorage`] is where they live: [`MemoryStorage`], or `SqliteStorage` with the
//!   `sqlite` feature.
//! - The wake rule: [`Mailboxes::post`] returns a [`PushJob`] carrying the sealed push as
//!   registered; [`forward`] waits out its delay (a random 0 to 30 s for `Quiet`, none for
//!   `Alert`), calls the relay through the [`PushRelay`] trait, and deletes the blob when the
//!   relay answers 410. Nothing is queued or retried.
//!
//! # Decisions
//!
//! - **Messages live as long as their mailbox.** C.1 gives `mailbox_msg` its own
//!   `expires_at`; nothing sets it to anything but the mailbox's, so it is not stored.
//! - **Every `Quiet` push is delayed**, mailbox posts included, so that a quiet push looks the
//!   same to the relay whatever caused it. Knock mailboxes are registered `Alert` (or class
//!   Location), so knocks are not slowed.
//! - **The relay's key id comes from the sealed push**: `SealedPush::key_id` is the key the
//!   device sealed to (bound into the seal), and the server forwards it unchanged in the
//!   CBOR [`PushRequest`] (`Content-Type:` [`PUSH_CONTENT_TYPE`]). A blob whose key has been
//!   retired answers 410 and is deleted.
//! - **Custom relays do not resolve** in [`ListedRelays`]: the server holds no credential for
//!   them. An operator who allows them supplies its own [`RelayDirectory`].
#![forbid(unsafe_code)]

mod error;
mod limit;
mod relay;
mod service;
#[cfg(feature = "sqlite")]
mod sqlite;
mod storage;
mod wake;

pub use error::{MailboxError, StorageError};
pub use limit::{Admission, NoPostLimit, PostLimit, PostsPerBucket};
/// The body of `POST /v1/push`, re-exported from pingers-proto.
pub use pingers_proto::PushRequest;
pub use relay::{
    ListedRelays, PUSH_CONTENT_TYPE, PUSH_PATH, PushRelay, PushRelayUrl, RelayAnswer, RelayCredential,
    RelayDirectory, RelayTarget, RelayUnreachable,
};
pub use service::{MailboxParts, MailboxRules, Mailboxes, Posted};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStorage;
pub use storage::{MailboxStorage, MemoryStorage, Removal, StoredMailbox};
pub use wake::{ForgetPush, Forwarded, PushJob, QUIET_DELAY_MAX, forward, push_delay};
