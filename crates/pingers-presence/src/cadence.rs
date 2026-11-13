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
    /// A Party Commit woke the device (design B.4.7): re-post now, so records carry the new
    /// epoch's tags. Every member woken by the Commit does this, whether or not it holds a
    /// real record, so the uploads that follow a Commit say nothing about who is in a Spot.
    EpochChanged,
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
            | UploadTrigger::SuppressedWithRealRecord
            | UploadTrigger::EpochChanged,
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
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_proto::ProtoError;
    use pingers_testkit::seeded_rng;

    use super::*;
    use crate::test_support::FixedRng;

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
        assert_eq!(
            plan_upload(UploadTrigger::EpochChanged, recent, now),
            UploadPlan::UploadNow
        );
    }

    #[test]
    fn first_cover_upload_goes_at_once() {
        assert_eq!(
            plan_upload(UploadTrigger::Wake, None, UnixSeconds::new(5)),
            UploadPlan::UploadNow
        );
    }

    fn policy(
        mode: DataMode,
        epoch_change: EpochChange,
        ratio: f32,
    ) -> Result<ReadPolicy, ProtoError> {
        Ok(ReadPolicy {
            mode,
            epoch_change,
            ratio: CoverReadRatio::new(ratio)?,
        })
    }

    #[test]
    fn confirmed_entry_always_reads() -> Result<(), ProtoError> {
        let mut rng = seeded_rng(1);
        let low = policy(DataMode::LowData, EpochChange::Unchanged, 0.0)?;
        let never = policy(DataMode::Normal, EpochChange::Unchanged, 0.0)?;
        for policy in [low, never] {
            assert_eq!(
                read_board(UploadTrigger::ConfirmedEntry, &policy, &mut rng),
                ReadBoard::Read
            );
        }
        Ok(())
    }

    #[test]
    fn low_data_reads_only_after_an_epoch_change() -> Result<(), ProtoError> {
        let mut rng = seeded_rng(2);
        let changed = policy(DataMode::LowData, EpochChange::Changed, 0.0)?;
        assert_eq!(
            read_board(UploadTrigger::Wake, &changed, &mut rng),
            ReadBoard::Read
        );
        let unchanged = policy(DataMode::LowData, EpochChange::Unchanged, 1.0)?;
        assert_eq!(
            read_board(UploadTrigger::Wake, &unchanged, &mut rng),
            ReadBoard::Skip
        );
        Ok(())
    }

    #[test]
    fn normal_mode_reads_at_the_cover_ratio() -> Result<(), ProtoError> {
        let mut rng = seeded_rng(3);
        let always = policy(DataMode::Normal, EpochChange::Unchanged, 1.0)?;
        let never = policy(DataMode::Normal, EpochChange::Changed, 0.0)?;
        let quarter = policy(DataMode::Normal, EpochChange::Unchanged, 0.25)?;
        let mut reads = 0_u32;
        for _ in 0..1_000 {
            assert_eq!(
                read_board(UploadTrigger::ExitWithRealRecord, &always, &mut rng),
                ReadBoard::Read
            );
            assert_eq!(
                read_board(UploadTrigger::Wake, &never, &mut rng),
                ReadBoard::Skip
            );
            if read_board(UploadTrigger::Wake, &quarter, &mut rng) == ReadBoard::Read {
                reads = reads.saturating_add(1);
            }
        }
        // Binomial(1000, 0.25): mean 250, standard deviation about 14; the seed is fixed.
        assert!((200..=300).contains(&reads), "{reads} reads");
        Ok(())
    }

    #[test]
    fn a_draw_must_be_below_the_ratio_to_read() -> Result<(), ProtoError> {
        // 0x4000_0000 / 2^32 is exactly 0.25: not below it, so skip; one less reads. `next_u32`
        // reads its four bytes little-endian, as rand_core and ChaCha do.
        let quarter = policy(DataMode::Normal, EpochChange::Unchanged, 0.25)?;
        let mut at = FixedRng::new(&[0, 0, 0, 0x40]);
        assert_eq!(
            read_board(UploadTrigger::Wake, &quarter, &mut at),
            ReadBoard::Skip
        );
        let mut below = FixedRng::new(&[0xff, 0xff, 0xff, 0x3f]);
        assert_eq!(
            read_board(UploadTrigger::Wake, &quarter, &mut below),
            ReadBoard::Read
        );
        Ok(())
    }
}
