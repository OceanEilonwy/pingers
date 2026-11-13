// SPDX-License-Identifier: AGPL-3.0-only
//! The cover policy: when to upload a lease and whether to read the board (design B.4.3,
//! B.4.5).

use core::time::Duration;

use pingers_proto::{CoverReadRatio, ReadBoard};
use pingers_time::UnixSeconds;
use rand_core::CryptoRng;

/// Why the core is considering an upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UploadTrigger {
    /// A Spot became `Inside`: upload now, ignoring spacing.
    ConfirmedEntry,
    /// A Spot with a real record became `Outside`: upload now (dummies replace it).
    ExitWithRealRecord,
    /// Sharing became suppressed while holding a real record: upload now.
    SuppressedWithRealRecord,
    /// Any other wake of the main app: a cover upload, subject to spacing.
    Wake,
}

/// The upload decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum UploadPlan {
    /// Upload in this wake.
    UploadNow,
    /// Too soon after the last upload; do not upload before this time.
    NotBefore(UnixSeconds),
}

/// The user's "Low data" setting (design B.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DataMode {
    /// Read on uploads as `cover_read_ratio` says.
    Normal,
    /// Read only on confirmed entry and after a Party epoch change. The server can then tell
    /// when the device entered some Spot; the UI must say so.
    LowData,
}

/// Whether a Party epoch changed since the last board read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EpochChange {
    /// At least one Party moved to a new epoch.
    Changed,
    /// No Party epoch changed.
    Unchanged,
}

/// What decides whether a non-entry upload reads the board.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReadPolicy {
    /// The user's data setting.
    pub mode: DataMode,
    /// Whether a Party epoch changed since the last read.
    pub epoch_change: EpochChange,
    /// The server's `cover_read_ratio`.
    pub ratio: CoverReadRatio,
}

/// Cover uploads are at least this far apart.
pub const MIN_UPLOAD_SPACING: Duration = Duration::from_mins(20);

/// Cover uploads aim to be this far apart; schedule the next wake accordingly.
pub const TARGET_UPLOAD_SPACING: Duration = Duration::from_mins(45);

/// Decides whether to upload now (design B.4.5).
pub fn plan_upload(
    trigger: UploadTrigger,
    last_upload: Option<UnixSeconds>,
    now: UnixSeconds,
) -> UploadPlan {
    match (trigger, last_upload) {
        (UploadTrigger::Wake, Some(last)) => match last.checked_add(MIN_UPLOAD_SPACING) {
            Some(earliest) if now < earliest => UploadPlan::NotBefore(earliest),
            Some(_) | None => UploadPlan::UploadNow,
        },
        (
            UploadTrigger::ConfirmedEntry
            | UploadTrigger::ExitWithRealRecord
            | UploadTrigger::SuppressedWithRealRecord,
            _,
        )
        | (UploadTrigger::Wake, None) => UploadPlan::UploadNow,
    }
}

/// Decides whether this upload also reads the board. A confirmed entry always reads;
/// otherwise the "Low data" setting and the server's `cover_read_ratio` decide.
pub fn read_board(
    trigger: UploadTrigger,
    policy: &ReadPolicy,
    rng: &mut impl CryptoRng,
) -> ReadBoard {
    if trigger == UploadTrigger::ConfirmedEntry {
        return ReadBoard::Read;
    }
    let ReadPolicy {
        mode,
        epoch_change,
        ratio,
    } = *policy;
    match (mode, epoch_change) {
        (DataMode::LowData, EpochChange::Changed) => ReadBoard::Read,
        (DataMode::LowData, EpochChange::Unchanged) => ReadBoard::Skip,
        (DataMode::Normal, _) => {
            let draw = f64::from(rng.next_u32()) / 4_294_967_296.0;
            if draw < f64::from(ratio.get()) {
                ReadBoard::Read
            } else {
                ReadBoard::Skip
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_upload_waits_for_minimum_spacing() {
        let plan = plan_upload(
            UploadTrigger::Wake,
            Some(UnixSeconds::new(1_000)),
            UnixSeconds::new(1_600),
        );
        assert_eq!(plan, UploadPlan::NotBefore(UnixSeconds::new(2_200)));
        let later = plan_upload(
            UploadTrigger::Wake,
            Some(UnixSeconds::new(1_000)),
            UnixSeconds::new(2_200),
        );
        assert_eq!(later, UploadPlan::UploadNow);
    }

    #[test]
    fn state_changes_ignore_spacing() {
        let recent = Some(UnixSeconds::new(1_000));
        let now = UnixSeconds::new(1_001);
        assert_eq!(
            plan_upload(UploadTrigger::ConfirmedEntry, recent, now),
            UploadPlan::UploadNow
        );
        assert_eq!(
            plan_upload(UploadTrigger::ExitWithRealRecord, recent, now),
            UploadPlan::UploadNow
        );
        assert_eq!(
            plan_upload(UploadTrigger::SuppressedWithRealRecord, recent, now),
            UploadPlan::UploadNow
        );
    }
}
