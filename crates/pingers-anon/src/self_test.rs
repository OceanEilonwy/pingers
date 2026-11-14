// SPDX-License-Identifier: AGPL-3.0-only
//! The on-device prover self-test (design B.4.8, Privacy Pass fallback trigger 2; H.1 blockers
//! 4 and 5).
//!
//! Before a device makes membership proofs it checks, once per set of parameters:
//!
//! 1. that the embedded proving key is the PSE ceremony's key for this circuit;
//! 2. that a proof it makes verifies, and that the verifier refuses the same proof bound to
//!    another request and with a changed public input (so a verifier that accepts everything
//!    cannot pass);
//! 3. that proving is fast enough: the 95th percentile of [`SelfTestPlan::proofs`] timed proofs
//!    is at most [`SelfTestPlan::limit`] (1.5 s).
//!
//! A device that fails uses Privacy Pass for everything. The test runs in the main app, off the
//! async executor; the timer is injected so the timing logic is tested without proving slowly.

use core::num::NonZeroU8;
use core::time::Duration;

use pingers_proto::{RlnScope, ServerId};
use pingers_time::{Bucket, Timer};
use rand_core::CryptoRng;

use crate::circuit::ceremony::matches_ceremony;
use crate::{
    AcceptedRoots, AnonError, LeafPosition, LeafUpdate, MESSAGE_LIMIT, MembershipTree, MessageId,
    ProofRequest, RlnSecret, VerificationContext, prove, request_digest, verify,
};

/// How many timed proofs, and the 95th percentile they must stay under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfTestPlan {
    proofs: NonZeroU8,
    limit: Duration,
}

impl SelfTestPlan {
    /// The design's test: 20 timed proofs, 95th percentile at most 1.5 s. With 20 proofs the
    /// 95th percentile (nearest rank) is the 19th fastest, so one outlier is forgiven.
    pub const DESIGN: Self = Self {
        proofs: match NonZeroU8::new(20) {
            Some(proofs) => proofs,
            None => NonZeroU8::MIN,
        },
        limit: Duration::from_millis(1_500),
    };

    /// A plan of `proofs` timed proofs against `limit`.
    #[must_use]
    pub const fn new(proofs: NonZeroU8, limit: Duration) -> Self {
        Self { proofs, limit }
    }

    /// How many proofs are timed (after one untimed warm-up proof, which also loads the key).
    #[must_use]
    pub const fn proofs(self) -> NonZeroU8 {
        self.proofs
    }

    /// The most the 95th percentile may be.
    #[must_use]
    pub const fn limit(self) -> Duration {
        self.limit
    }

    /// The nearest rank of the 95th percentile among the timed proofs, 1-based: `ceil(0.95 n)`.
    fn rank(self) -> usize {
        let proofs = usize::from(self.proofs.get());
        proofs.saturating_mul(95).div_ceil(100).max(1)
    }
}

/// The outcome of a self-test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SelfTestVerdict {
    /// The parameters are the ceremony's, proofs verify, and proving is fast enough.
    Passed,
    /// Proofs verify, but the 95th percentile is above the limit.
    TooSlow,
    /// The embedded key is not the ceremony's key for this circuit.
    WrongParameters,
    /// A proof did not verify, or the verifier accepted a proof it must refuse.
    ProofRejected,
}

/// What the self-test measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfTestReport {
    /// The verdict.
    pub verdict: SelfTestVerdict,
    /// The warm-up proof, which includes decoding the proving key (the first proof a process
    /// makes costs this much).
    pub first: Duration,
    /// The median of the timed proofs.
    pub median: Duration,
    /// The 95th percentile (nearest rank) of the timed proofs; when the test stopped early
    /// because the limit could no longer be met, the slowest proof seen.
    pub p95: Duration,
    /// How many proofs were timed (fewer than planned when the test stopped early).
    pub timed: u8,
}

/// The server identity the self-test proves for. It names no real server, so a self-test proof
/// can never be spent anywhere.
const SELF_TEST_SERVER: &str = "https://self-test.pingers.invalid";

/// Leaves around the device's own in the self-test tree, so its path has real siblings on both
/// sides at the lowest levels.
const NEIGHBOURS: u32 = 3;

/// Runs the self-test. Takes about `plan.proofs() + 1` proof times; it stops early once the
/// 95th percentile can no longer be met, and at once if the parameters or a proof are wrong.
///
/// # Errors
///
/// [`AnonError`] from key generation or proving: these are faults to report, not a verdict.
pub fn prover_self_test(
    plan: SelfTestPlan,
    timer: &dyn Timer,
    rng: &mut impl CryptoRng,
) -> Result<SelfTestReport, AnonError> {
    let mut report = SelfTestReport {
        verdict: SelfTestVerdict::WrongParameters,
        first: Duration::ZERO,
        median: Duration::ZERO,
        p95: Duration::ZERO,
        timed: 0,
    };
    if !matches_ceremony() {
        return Ok(report);
    }
    let fixture = Fixture::new(rng)?;

    let started = timer.now();
    let warm_up = fixture.prove(0, rng)?;
    report.first = timer.now().saturating_duration_since(started);
    if !fixture.verifies(&warm_up, 0)? || !fixture.refuses_tampering(&warm_up)? {
        report.verdict = SelfTestVerdict::ProofRejected;
        return Ok(report);
    }

    // The 95th percentile exceeds the limit once more than `planned - rank` proofs do.
    let planned = usize::from(plan.proofs.get());
    let slow_allowed = planned.saturating_sub(plan.rank());
    let mut durations = Vec::with_capacity(planned);
    let mut slow = 0_usize;
    for round in 1..=plan.proofs.get() {
        let started = timer.now();
        let proof = fixture.prove(round, rng)?;
        let took = timer.now().saturating_duration_since(started);
        if !fixture.verifies(&proof, round)? {
            report.verdict = SelfTestVerdict::ProofRejected;
            return Ok(report);
        }
        durations.push(took);
        if took > plan.limit {
            slow = slow.saturating_add(1);
            if slow > slow_allowed {
                break;
            }
        }
    }
    let summary = summarise(plan, durations)?;
    report.timed = summary.timed;
    report.median = summary.median;
    report.p95 = summary.p95;
    report.verdict = summary.verdict;
    Ok(report)
}

/// The timing part of a report.
#[derive(Debug, PartialEq, Eq)]
struct Summary {
    verdict: SelfTestVerdict,
    median: Duration,
    p95: Duration,
    timed: u8,
}

/// Judges the timed proofs of a plan: passed when every planned proof ran and the 95th
/// percentile (nearest rank) is at most the limit. A short list (the test stopped early)
/// reports its slowest proof as the percentile.
fn summarise(plan: SelfTestPlan, mut times: Vec<Duration>) -> Result<Summary, AnonError> {
    times.sort_unstable();
    let complete = times.len() == usize::from(plan.proofs.get());
    let p95 = if complete {
        times.get(plan.rank().saturating_sub(1))
    } else {
        times.last()
    }
    .copied()
    .unwrap_or_default();
    Ok(Summary {
        verdict: if complete && p95 <= plan.limit {
            SelfTestVerdict::Passed
        } else {
            SelfTestVerdict::TooSlow
        },
        median: times
            .get(times.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or_default(),
        p95,
        timed: u8::try_from(times.len()).map_err(|_| AnonError::Internal)?,
    })
}

/// A throwaway identity in a small tree, proving for a server that does not exist.
struct Fixture {
    secret: RlnSecret,
    tree: MembershipTree,
    position: LeafPosition,
    server: ServerId,
    bucket: Bucket,
}

impl Fixture {
    fn new(rng: &mut impl CryptoRng) -> Result<Self, AnonError> {
        let secret = RlnSecret::generate(rng)?;
        let mut updates = Vec::new();
        for _ in 0..NEIGHBOURS {
            updates.push(LeafUpdate::Appended(
                RlnSecret::generate(rng)?.id_commitment()?.leaf()?,
            ));
        }
        updates.insert(1, LeafUpdate::Appended(secret.id_commitment()?.leaf()?));
        let mut tree = MembershipTree::empty()?;
        tree.apply(&updates)?;
        Ok(Self {
            secret,
            tree,
            position: LeafPosition::new(1),
            server: ServerId::new(SELF_TEST_SERVER.to_owned()).map_err(|_| AnonError::Internal)?,
            bucket: Bucket::from_index(0).map_err(|_| AnonError::Internal)?,
        })
    }

    /// The request the proof of `round` binds: a different body each round.
    fn digest(round: u8) -> Result<crate::RequestDigest, AnonError> {
        request_digest("POST", "/v1/board/sync", &[round; 424])
    }

    fn prove(
        &self,
        round: u8,
        rng: &mut impl CryptoRng,
    ) -> Result<pingers_proto::RlnAuth, AnonError> {
        let path = self.tree.path(self.position)?;
        prove(
            &ProofRequest {
                secret: &self.secret,
                path: &path,
                root: self.tree.root(),
                server: &self.server,
                scope: RlnScope::Board,
                bucket: self.bucket,
                message_id: MessageId::new(round % MESSAGE_LIMIT)?,
                digest: Self::digest(round)?,
            },
            rng,
        )
    }

    fn context(&self, round: u8) -> Result<VerificationContext<'_>, AnonError> {
        Ok(VerificationContext {
            roots: AcceptedRoots::first(self.tree.root()),
            server: &self.server,
            expected: Self::digest(round)?,
        })
    }

    /// Whether `proof` (made in `round`) verifies.
    fn verifies(&self, proof: &pingers_proto::RlnAuth, round: u8) -> Result<bool, AnonError> {
        match verify(proof, &self.context(round)?) {
            Ok(_) => Ok(true),
            Err(AnonError::BadProof | AnonError::WrongRequest | AnonError::StaleRoot) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Whether the verifier refuses `proof` presented for another request, and with its share
    /// `y` changed (which only the pairing check can catch).
    fn refuses_tampering(&self, proof: &pingers_proto::RlnAuth) -> Result<bool, AnonError> {
        let other_request = matches!(
            verify(proof, &self.context(1)?),
            Err(AnonError::WrongRequest)
        );
        let mut changed = *proof;
        changed.y = proof.nullifier;
        let changed_share = matches!(
            verify(&changed, &self.context(0)?),
            Err(AnonError::BadProof)
        );
        Ok(other_request && changed_share)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use std::sync::Mutex;

    use pingers_time::{MonotonicTime, Sleep};
    use rand_chacha::ChaCha20Rng;
    use rand_core::SeedableRng as _;

    use super::*;

    /// A timer whose reading moves on by the next scripted step at each `now`, so proof times
    /// are whatever the test says (each proof reads the timer twice: start and end).
    struct ScriptedTimer {
        state: Mutex<(MonotonicTime, Vec<Duration>)>,
    }

    impl ScriptedTimer {
        /// Proof `i` takes `proofs[i]`; the warm-up proof takes a millisecond.
        fn new(proofs: &[Duration]) -> Self {
            let mut steps = vec![Duration::ZERO, Duration::from_millis(1)];
            for took in proofs {
                steps.push(Duration::ZERO);
                steps.push(*took);
            }
            steps.reverse();
            Self {
                state: Mutex::new((MonotonicTime::ORIGIN, steps)),
            }
        }
    }

    impl Timer for ScriptedTimer {
        fn now(&self) -> MonotonicTime {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let step = state.1.pop().unwrap_or_default();
            state.0 = state.0.saturating_add(step);
            state.0
        }

        fn sleep_until(&self, _deadline: MonotonicTime) -> Sleep<'_> {
            Box::pin(core::future::ready(()))
        }
    }

    fn plan(proofs: u8) -> Result<SelfTestPlan, AnonError> {
        Ok(SelfTestPlan::new(
            NonZeroU8::new(proofs).ok_or(AnonError::Internal)?,
            Duration::from_millis(1_500),
        ))
    }

    fn millis(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_millis).collect()
    }

    #[test]
    fn the_design_plan_is_twenty_proofs_under_one_and_a_half_seconds() {
        assert_eq!(SelfTestPlan::DESIGN.proofs().get(), 20);
        assert_eq!(SelfTestPlan::DESIGN.limit(), Duration::from_millis(1_500));
        assert_eq!(SelfTestPlan::DESIGN.rank(), 19);
    }

    #[test]
    fn the_rank_is_the_ceiling_of_ninety_five_percent() -> Result<(), AnonError> {
        assert_eq!(plan(1)?.rank(), 1);
        assert_eq!(plan(3)?.rank(), 3);
        assert_eq!(plan(10)?.rank(), 10);
        assert_eq!(plan(40)?.rank(), 38);
        Ok(())
    }

    #[test]
    fn real_proofs_verify_and_pass_when_fast_enough() -> Result<(), AnonError> {
        let timer = ScriptedTimer::new(&millis(&[400, 1_500, 300]));
        let report = prover_self_test(plan(3)?, &timer, &mut ChaCha20Rng::seed_from_u64(2))?;
        assert_eq!(
            report.verdict,
            SelfTestVerdict::Passed,
            "the limit itself passes"
        );
        assert_eq!(report.first, Duration::from_millis(1));
        assert_eq!(report.timed, 3);
        assert_eq!(report.median, Duration::from_millis(400));
        assert_eq!(report.p95, Duration::from_millis(1_500));
        Ok(())
    }

    #[test]
    fn the_test_stops_once_the_percentile_cannot_be_met() -> Result<(), AnonError> {
        // Of twenty, one proof over the limit is forgiven; the second ends the test.
        let timer = ScriptedTimer::new(&millis(&[400, 2_000, 500, 1_600, 450]));
        let report = prover_self_test(plan(20)?, &timer, &mut ChaCha20Rng::seed_from_u64(1))?;
        assert_eq!(report.verdict, SelfTestVerdict::TooSlow);
        assert_eq!(report.timed, 4);
        assert_eq!(report.p95, Duration::from_millis(2_000));
        Ok(())
    }

    #[test]
    fn one_slow_proof_in_twenty_is_forgiven() -> Result<(), AnonError> {
        let mut times = millis(&[400; 19]);
        times.push(Duration::from_secs(5));
        let summary = summarise(plan(20)?, times)?;
        assert_eq!(summary.verdict, SelfTestVerdict::Passed);
        assert_eq!(summary.p95, Duration::from_millis(400));
        assert_eq!(summary.timed, 20);
        Ok(())
    }

    #[test]
    fn two_slow_proofs_in_twenty_fail() -> Result<(), AnonError> {
        let mut times = millis(&[400; 18]);
        times.extend(millis(&[1_501, 1_600]));
        let summary = summarise(plan(20)?, times)?;
        assert_eq!(summary.verdict, SelfTestVerdict::TooSlow);
        assert_eq!(summary.p95, Duration::from_millis(1_501));
        assert_eq!(summary.median, Duration::from_millis(400));
        Ok(())
    }

    #[test]
    fn an_unfinished_test_never_passes() -> Result<(), AnonError> {
        let summary = summarise(plan(20)?, millis(&[100, 200]))?;
        assert_eq!(summary.verdict, SelfTestVerdict::TooSlow);
        assert_eq!(summary.p95, Duration::from_millis(200));
        Ok(())
    }
}
