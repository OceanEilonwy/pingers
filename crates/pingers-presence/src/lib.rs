// SPDX-License-Identifier: AGPL-3.0-only
//! Board, leases, mailboxes, knocks (sans-IO).
//!
//! The co-presence protocol (architecture §3, design B.4). All inputs and outputs are values;
//! the caller does the network, storage and clock reads.
//!
//! Client half:
//! - [`ActiveSpot`]: a Spot's keys, derivable only while the device is `Inside` it
//!   (design B.4.3, "match only where you are"), and the [`MeetupKey`] (`PSK_meet` with its
//!   id) that binds a Meetup to that Spot (design B.5);
//! - [`build_lease`]: real slots plus random dummies, shuffled, always four;
//! - [`TrialMatcher`]: trial-match board pages against active Spots, and open their beacons;
//! - [`matching_epochs`]: which epochs to match with after a Commit (design B.4.7);
//! - [`knock_targets`], [`build_knock`], [`open_knock`], [`verify_knock`]: the knock and its
//!   checks;
//! - [`decide_knock`]: the liveness decision, tie-break and Meetup admission;
//! - [`prepare_mailbox`], [`mailbox_covers_lease`] and [`knock_unanswered`]: the mailbox pool
//!   and unanswered knocks;
//! - [`plan_upload`] and [`read_board`]: the cover policy.
//!
//! Server half (feature `server`): lease validation, segment expiry and hashing, mailbox
//! posting rules.
#![forbid(unsafe_code)]

mod cadence;
mod epoch;
mod error;
mod knock;
mod lease;
mod liveness;
mod mailbox;
mod matching;
mod random;
#[cfg(feature = "server")]
mod server;
mod spot;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod knock_tests;
#[cfg(test)]
mod lease_tests;
#[cfg(test)]
mod matching_tests;
#[cfg(test)]
mod scenarios;
#[cfg(test)]
mod vector_tests;

pub use cadence::{
    DataMode, EpochChange, MIN_UPLOAD_SPACING, ReadPolicy, TARGET_UPLOAD_SPACING, UploadPlan,
    UploadTrigger, plan_upload, read_board,
};
pub use epoch::{EpochState, EpochTransition, MatchingEpochs, matching_epochs};
pub use error::PresenceError;
pub use knock::{
    KnockContext, KnockInputs, OpenedKnock, OutgoingKnock, RosterLookup, VerifiedKnock,
    build_knock, open_knock, verify_knock,
};
pub use lease::{
    BuiltLease, LEASE_BUCKETS, LeaseInputs, RealSlot, SlotCandidate, SlotContent, SlotRecord,
    build_lease, choose_slots,
};
pub use liveness::{
    Admission, FreshFix, KnockDecision, KnockSituation, LivenessEvidence, Sharing, decide_knock,
};
pub use mailbox::{
    LEASE_DURATION, MAX_MAILBOX_LIFETIME, MailboxPurpose, MailboxRegistration, MailboxRequest,
    PushSealing, knock_unanswered, mailbox_covers_lease, mailbox_expires_at, prepare_mailbox,
};
pub use matching::{
    BoardMatch, KnockFilter, MAX_KNOCKS_PER_ARRIVAL, OwnRecords, TrialMatcher, knock_targets,
};
#[cfg(feature = "server")]
pub use server::{
    check_lease, check_mailbox_live, check_mailbox_post, check_mailbox_spec, closed_segment_hash,
    live_segments, segment_closed, segment_expired,
};
pub use spot::{ActiveSpot, MeetupKey, MeetupKeyId, SpotRef, SpotStatus};
