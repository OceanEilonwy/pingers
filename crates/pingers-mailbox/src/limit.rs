// SPDX-License-Identifier: AGPL-3.0-only
//! Hooks for limiting posts per mailbox, on top of the anonymous credential's own limit.

use core::num::NonZeroU32;
use std::collections::HashMap;

use pingers_proto::MailboxId;
use pingers_time::{Bucket, UnixSeconds};

/// The answer of a [`PostLimit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Store the post.
    Admit,
    /// Refuse it.
    Refuse,
}

/// Decides whether one more post to a mailbox is allowed. Called only for posts that pass
/// every other rule, so a refusal elsewhere does not use up the allowance.
///
/// The membership proof or Privacy Pass token already limits each poster (pingers-quota);
/// this hook bounds how fast any one mailbox fills, whoever posts.
pub trait PostLimit {
    /// Counts the post if admitted.
    fn admit(&mut self, mailbox: &MailboxId, now: UnixSeconds) -> Admission;
}

/// Admits every post.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPostLimit;

impl PostLimit for NoPostLimit {
    fn admit(&mut self, _mailbox: &MailboxId, _now: UnixSeconds) -> Admission {
        Admission::Admit
    }
}

/// At most `max` posts per mailbox per 15-minute bucket. Counts live in memory and are
/// dropped when the bucket changes, so no history of posts is kept.
#[derive(Debug)]
pub struct PostsPerBucket {
    max: NonZeroU32,
    bucket: Option<Bucket>,
    counts: HashMap<MailboxId, u32>,
}

impl PostsPerBucket {
    /// A limit of `max` posts per mailbox per bucket.
    #[must_use]
    pub fn new(max: NonZeroU32) -> Self {
        Self {
            max,
            bucket: None,
            counts: HashMap::new(),
        }
    }
}

impl PostLimit for PostsPerBucket {
    fn admit(&mut self, mailbox: &MailboxId, now: UnixSeconds) -> Admission {
        let bucket = now.bucket();
        if self.bucket != Some(bucket) {
            self.bucket = Some(bucket);
            self.counts.clear();
        }
        let count = self.counts.entry(*mailbox).or_insert(0);
        if *count >= self.max.get() {
            return Admission::Refuse;
        }
        *count = count.saturating_add(1);
        Admission::Admit
    }
}

#[cfg(test)]
mod tests {
    use pingers_time::Bucket;

    use super::*;

    #[test]
    fn limit_applies_per_mailbox_and_resets_each_bucket() {
        let mut limit = PostsPerBucket::new(NonZeroU32::MIN.saturating_add(1));
        let first = MailboxId::from_bytes([1; 16]);
        let second = MailboxId::from_bytes([2; 16]);
        let now = UnixSeconds::new(9_000);
        assert_eq!(limit.admit(&first, now), Admission::Admit);
        assert_eq!(limit.admit(&first, now), Admission::Admit);
        assert_eq!(limit.admit(&first, now), Admission::Refuse);
        assert_eq!(limit.admit(&second, now), Admission::Admit);
        let next_bucket = now.checked_add(Bucket::LENGTH).unwrap_or(now);
        assert_eq!(limit.admit(&first, next_bucket), Admission::Admit);
    }

    #[test]
    fn no_limit_admits_everything() {
        let mut limit = NoPostLimit;
        let mailbox = MailboxId::from_bytes([1; 16]);
        for _ in 0..1_000 {
            assert_eq!(limit.admit(&mailbox, UnixSeconds::new(0)), Admission::Admit);
        }
    }
}
