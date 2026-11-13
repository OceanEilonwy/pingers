// SPDX-License-Identifier: AGPL-3.0-only
//! Test helpers: an RNG that replays listed bytes, the test error type, a roster and fixtures.

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot};
use pingers_geo::PresenceState;
use pingers_proto::{
    BoardPage, Epoch, LeafIndex, Lease, MailboxId, PartyId, Record, SegmentInfo, SpotId,
};
use pingers_testkit::{TestRng, VectorError, random_array};
use pingers_time::Bucket;
use zeroize::Zeroizing;

use crate::{ActiveSpot, PresenceError, RosterLookup, SpotRef, SpotStatus};

/// Why a test failed before reaching an assertion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    /// The code under test failed.
    #[error(transparent)]
    Presence(#[from] PresenceError),
    /// A crypto helper failed.
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    /// An encoding failed.
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    /// A time value was out of range.
    #[error(transparent)]
    Time(#[from] pingers_time::TimeError),
    /// A vector file or field was unusable.
    #[error(transparent)]
    Vector(#[from] VectorError),
    /// A vector field or fixture had the wrong form.
    #[error("malformed: {0}")]
    Malformed(&'static str),
}

/// The shared vector-replaying RNG. Presence draws read bytes with `fill_bytes` and decode
/// them big-endian themselves (`random.rs`), so its little-endian words are never used here.
pub(crate) use pingers_testkit::FixedRng;

/// The server's board for tests: segments by start bucket, append and read, no matching.
#[derive(Default)]
pub(crate) struct TestBoard {
    segments: std::collections::BTreeMap<Bucket, Vec<Record>>,
}

impl TestBoard {
    /// Appends a lease's records to its segment.
    pub(crate) fn post(&mut self, lease: &Lease) {
        self.segments
            .entry(lease.start)
            .or_default()
            .extend_from_slice(&lease.records);
    }

    /// Appends raw records to a segment (forgeries and replays).
    pub(crate) fn inject(&mut self, start: Bucket, records: &[Record]) {
        self.segments
            .entry(start)
            .or_default()
            .extend_from_slice(records);
    }

    /// Removes a lease's records (a revocation or replacement).
    pub(crate) fn revoke(&mut self, lease: &Lease) {
        if let Some(records) = self.segments.get_mut(&lease.start) {
            records.retain(|record| !lease.records.contains(record));
        }
    }

    /// One page holding every segment, oldest first, whether or not it is live: the reader
    /// must do its own expiry.
    pub(crate) fn page(&self) -> Result<BoardPage, TestError> {
        let mut segments = Vec::new();
        let mut records = Vec::new();
        for (start, segment) in &self.segments {
            segments.push(SegmentInfo {
                start: *start,
                record_count: u32::try_from(segment.len())
                    .map_err(|_overflow| TestError::Malformed("segment size"))?,
                closed_hash: None,
            });
            records.extend_from_slice(segment);
        }
        Ok(BoardPage {
            segments,
            records,
            next: None,
        })
    }
}

/// The mailbox service for tests: posts by mailbox id.
#[derive(Default)]
pub(crate) struct TestMailboxes {
    posts: std::collections::HashMap<MailboxId, Vec<Vec<u8>>>,
}

impl TestMailboxes {
    pub(crate) fn post(&mut self, knock: &crate::OutgoingKnock) {
        self.posts
            .entry(knock.mailbox)
            .or_default()
            .push(knock.post.clone());
    }

    pub(crate) fn read(&self, mailbox: &MailboxId) -> &[Vec<u8>] {
        self.posts.get(mailbox).map_or(&[], Vec::as_slice)
    }
}

/// A Party roster over a range of epochs.
pub(crate) struct TestRoster {
    pub(crate) current: Epoch,
    pub(crate) members: Vec<RosterEntry>,
}

/// A member's leaf, key, and the epochs it was in the group: from `joined` up to, not
/// including, `removed`.
pub(crate) struct RosterEntry {
    pub(crate) leaf: LeafIndex,
    pub(crate) key: AccountPublicKey,
    pub(crate) joined: Epoch,
    pub(crate) removed: Option<Epoch>,
}

impl RosterLookup for TestRoster {
    fn signature_key(&self, leaf: LeafIndex, epoch: Epoch) -> Option<AccountPublicKey> {
        self.members
            .iter()
            .find(|entry| {
                entry.leaf == leaf
                    && entry.joined <= epoch
                    && entry.removed.is_none_or(|removed| epoch < removed)
            })
            .map(|entry| entry.key)
    }

    fn current_epoch(&self) -> Epoch {
        self.current
    }
}

/// A Party member for tests.
pub(crate) struct Member {
    pub(crate) leaf: LeafIndex,
    pub(crate) signing_key: AccountSigningKey,
}

impl Member {
    pub(crate) fn new(leaf: u32, rng: &mut TestRng) -> Self {
        Self {
            leaf: LeafIndex::new(leaf),
            signing_key: AccountSigningKey::generate(rng),
        }
    }

    pub(crate) fn entry(&self, joined: Epoch, removed: Option<Epoch>) -> RosterEntry {
        RosterEntry {
            leaf: self.leaf,
            key: self.signing_key.public_key(),
            joined,
            removed,
        }
    }
}

/// A Party's presence roots by epoch and one of its Spots, for tests.
pub(crate) struct TestParty {
    pub(crate) party: PartyId,
    pub(crate) spot: SpotId,
    seed: [u8; 32],
}

impl TestParty {
    pub(crate) fn new(rng: &mut TestRng) -> Self {
        Self {
            party: PartyId::from_bytes(random_array(rng)),
            spot: SpotId::from_bytes(random_array(rng)),
            seed: random_array(rng),
        }
    }

    /// Another Spot of the same Party.
    pub(crate) fn other_spot(&self, rng: &mut TestRng) -> Self {
        Self {
            party: self.party,
            spot: SpotId::from_bytes(random_array(rng)),
            seed: self.seed,
        }
    }

    pub(crate) const fn spot_ref(&self) -> SpotRef {
        SpotRef {
            party: self.party,
            spot: self.spot,
        }
    }

    /// The presence root at `epoch`: a different value per epoch, as the MLS exporter gives.
    pub(crate) fn root(&self, epoch: Epoch) -> PresenceRoot {
        let mut bytes = self.seed;
        for (byte, epoch_byte) in bytes.iter_mut().zip(epoch.get().to_be_bytes()) {
            *byte ^= epoch_byte;
        }
        PresenceRoot::from_bytes(Zeroizing::new(bytes))
    }

    /// The Spot's keys at `epoch`, as a device Inside it holds them.
    pub(crate) fn inside(&self, epoch: Epoch) -> Result<ActiveSpot, PresenceError> {
        ActiveSpot::derive(
            &self.root(epoch),
            &SpotStatus {
                spot: self.spot_ref(),
                epoch,
                state: PresenceState::Inside,
            },
        )
    }
}
