// SPDX-License-Identifier: AGPL-3.0-only
//! The wake rule: a post forwards the mailbox's sealed push to its relay, unopened; `Quiet`
//! pushes wait a random 0 to 30 seconds first; a 410 deletes the blob (design B.4.9, D).

use core::time::Duration;

use pingers_proto::{MailboxId, Urgency};
use pingers_time::Timer;
use rand_core::CryptoRng;

use crate::relay::{PushRelay, PushRequest, RelayAnswer, RelayTarget, RelayUnreachable};

/// The longest a `Quiet` push is held back.
pub const QUIET_DELAY_MAX: Duration = Duration::from_secs(30);

/// Delays are drawn in whole milliseconds from `0..=QUIET_DELAY_MAX`.
const QUIET_DELAY_STEPS: u64 = 30_001;

/// How long to hold a push back before sending it: nothing for `Alert`, and for `Quiet` a
/// uniformly random delay of 0 to 30 seconds, drawn per push so that a group's fan-out does
/// not reach the relay in one burst (design B.4.9).
#[must_use]
pub fn push_delay(urgency: Urgency, rng: &mut impl CryptoRng) -> Duration {
    match urgency {
        Urgency::Alert => Duration::ZERO,
        Urgency::Quiet => quiet_delay(rng),
    }
}

/// A uniform draw from `0..=QUIET_DELAY_MAX` in milliseconds, by rejection sampling so no
/// delay is more likely than another.
fn quiet_delay(rng: &mut impl CryptoRng) -> Duration {
    // The largest multiple of the step count that fits in a u64; draws at or above it are
    // redrawn (probability about 2⁻⁴⁹).
    let zone = u64::MAX
        .checked_sub(u64::MAX.checked_rem(QUIET_DELAY_STEPS).unwrap_or(0))
        .unwrap_or(u64::MAX);
    loop {
        let draw = rng.next_u64();
        // `<` and `<=` differ only for `draw == zone`, a 2⁻⁶⁴ event: cargo mutants reports
        // that mutant as surviving, and no test can tell them apart.
        if draw < zone {
            return Duration::from_millis(draw.checked_rem(QUIET_DELAY_STEPS).unwrap_or(0));
        }
    }
}

/// One push to send after a post: where, what, and how long to wait first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushJob {
    /// The mailbox whose blob this is, so a 410 can delete it.
    pub mailbox: MailboxId,
    /// The relay to call.
    pub target: RelayTarget,
    /// The request body.
    pub request: PushRequest,
    /// How long to wait before calling: zero for `Alert`.
    pub delay: Duration,
}

/// Deletes a mailbox's sealed push when its relay says the token is gone.
pub trait ForgetPush {
    /// Deletes the blob of `mailbox`, keeping the mailbox and its messages.
    ///
    /// # Errors
    ///
    /// [`crate::MailboxError::Storage`] if the storage fails.
    fn forget_push(&self, mailbox: &MailboxId) -> Result<(), crate::MailboxError>;
}

/// What became of a push.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Forwarded {
    /// The relay accepted it.
    Sent,
    /// The relay answered 410 and the blob was deleted.
    BlobDeleted,
    /// The relay refused it (422, 429 or another status) or could not be reached. Nothing is
    /// queued: the message waits in the mailbox for the owner's next wake (design B.4.9).
    NotSent,
}

/// Waits out the job's delay on `timer`, sends it, and deletes the blob on a 410.
///
/// The server spawns one task per job under an owner that cancels them on shutdown. Dropping
/// the future before it completes sends nothing, so it is cancellation-safe.
///
/// # Errors
///
/// [`crate::MailboxError::Storage`] if deleting the blob after a 410 fails.
pub async fn forward(
    job: &PushJob,
    relay: &impl PushRelay,
    timer: &impl Timer,
    store: &impl ForgetPush,
) -> Result<Forwarded, crate::MailboxError> {
    if !job.delay.is_zero() {
        timer.sleep(job.delay).await;
    }
    match relay.push(&job.target, &job.request).await {
        Ok(RelayAnswer::Accepted) => Ok(Forwarded::Sent),
        Ok(RelayAnswer::Gone) => {
            store.forget_push(&job.mailbox)?;
            Ok(Forwarded::BlobDeleted)
        }
        Ok(RelayAnswer::Rejected | RelayAnswer::RateLimited | RelayAnswer::Failed)
        | Err(RelayUnreachable) => Ok(Forwarded::NotSent),
    }
}

#[cfg(test)]
mod tests {
    use pingers_testkit::seeded_rng;

    use super::*;

    #[test]
    fn alerts_are_not_delayed() {
        let mut rng = seeded_rng(1);
        for _ in 0..100 {
            assert_eq!(push_delay(Urgency::Alert, &mut rng), Duration::ZERO);
        }
    }

    #[test]
    fn quiet_delays_are_uniform_over_zero_to_thirty_seconds() {
        const SAMPLES: u32 = 30_000;
        const BINS: usize = 10;
        let mut rng = seeded_rng(7);
        let mut bins = [0_u32; BINS];
        let mut total = Duration::ZERO;
        let mut shortest = QUIET_DELAY_MAX;
        let mut longest = Duration::ZERO;
        for _ in 0..SAMPLES {
            let delay = push_delay(Urgency::Quiet, &mut rng);
            assert!(delay <= QUIET_DELAY_MAX);
            shortest = shortest.min(delay);
            longest = longest.max(delay);
            total = total.saturating_add(delay);
            let bin = usize::try_from(delay.as_millis() / 3_001).unwrap_or(BINS);
            if let Some(count) = bins.get_mut(bin) {
                *count = count.saturating_add(1);
            }
        }
        // Each tenth of the range holds 3,000 ± 10 % of the draws (about 5.5 standard
        // deviations either side, for a fixed seed).
        for count in bins {
            assert!((2_700..=3_300).contains(&count), "bin held {count}");
        }
        let mean = total / SAMPLES;
        assert!(mean > Duration::from_millis(14_700) && mean < Duration::from_millis(15_300));
        assert!(shortest < Duration::from_millis(50));
        assert!(longest > Duration::from_millis(29_950));
    }
}
