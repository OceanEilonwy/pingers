// SPDX-License-Identifier: AGPL-3.0-only
//! The prover self-test, run once per set of parameters in the main app (design B.4.8, Privacy
//! Pass fallback trigger 2).
//!
//! Until a self-test has passed, the main app uses Privacy Pass like an extension. The test
//! takes about 21 proof times (8 s on an emulator, tens of seconds on a slow phone), so it runs
//! in the background: [`SyncEngine::start_prover_self_test`] starts it and returns. The test
//! is of this device's prover, not of a server, so the engines of a process (one per server)
//! share one [`SelfTestSlot`]: one test runs at a time, whichever engine started it. Its result
//! is kept in the store:
//!
//! - passed: proofs are used until the parameters change (a new `PARAMETERS_ID`);
//! - too slow: tested again after [`RETEST_AFTER`], since a phone that was busy or hot may pass
//!   on another day;
//! - wrong parameters or a rejected proof: a fault of the build, not of the phone; never tested
//!   again for these parameters.

use core::time::Duration;
use std::sync::{Arc, Mutex};

use pingers_anon::{
    PARAMETERS_ID, SelfTestPlan, SelfTestReport, SelfTestVerdict, prover_self_test,
};
use pingers_store::{Database, ProverVerdict, SelfTestRecord};
use pingers_time::{Timer, UnixSeconds};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;

use crate::engine::{device_blocking, lock};
use crate::{ProcessRole, SyncEngine, SyncError};

/// Where the process's one background self-test runs: every engine of the process holds a
/// clone ([`crate::SyncParts::self_test`]), so a second engine finds the test already running
/// instead of starting another. The test is stopped when the last clone goes.
#[derive(Clone, Default)]
pub struct SelfTestSlot {
    running: Arc<RunningSelfTest>,
}

/// The task, owned by every clone of a [`SelfTestSlot`] together.
#[derive(Default)]
struct RunningSelfTest {
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for RunningSelfTest {
    /// Stops waiting for the test. (A proof already on the blocking pool finishes; its result
    /// is recorded or dropped with the store.)
    fn drop(&mut self) {
        if let Some(task) = lock(&self.task).take() {
            task.abort();
        }
    }
}

/// How long a "too slow" result stands before the prover is tested again.
pub const RETEST_AFTER: Duration = Duration::from_hours(7 * 24);

/// What the stored self-test says about using the prover now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelfTestStanding {
    /// It passed for these parameters; proofs take about `p95` at worst.
    Passed {
        /// The 95th percentile the test measured.
        p95: Duration,
    },
    /// It failed and stands: use Privacy Pass.
    Failed,
    /// No valid result: test (and use Privacy Pass meanwhile).
    Due,
}

/// What [`SyncEngine::start_prover_self_test`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
#[non_exhaustive]
pub enum SelfTestStart {
    /// A test started in the background.
    Started,
    /// A test was already running; nothing more started.
    AlreadyRunning,
    /// Nothing to do: an extension, or a result that stands.
    NotDue,
}

/// The standing of `record` at `now`.
pub(crate) fn standing(record: Option<&SelfTestRecord>, now: UnixSeconds) -> SelfTestStanding {
    let Some(record) = record.filter(|record| record.parameters == PARAMETERS_ID) else {
        return SelfTestStanding::Due;
    };
    match record.verdict {
        ProverVerdict::Passed => SelfTestStanding::Passed { p95: record.p95 },
        ProverVerdict::TooSlow
            if record
                .tested_at
                .checked_add(RETEST_AFTER)
                .is_some_and(|retest| now >= retest) =>
        {
            SelfTestStanding::Due
        }
        // A failure stands, and a verdict this build does not know is no pass.
        ProverVerdict::TooSlow
        | ProverVerdict::WrongParameters
        | ProverVerdict::ProofRejected
        | _ => SelfTestStanding::Failed,
    }
}

const fn stored_verdict(verdict: SelfTestVerdict) -> ProverVerdict {
    match verdict {
        SelfTestVerdict::Passed => ProverVerdict::Passed,
        SelfTestVerdict::TooSlow => ProverVerdict::TooSlow,
        SelfTestVerdict::WrongParameters => ProverVerdict::WrongParameters,
        // Any verdict added later is never a pass.
        SelfTestVerdict::ProofRejected | _ => ProverVerdict::ProofRejected,
    }
}

/// What a self-test needs, detached from the engine so a background task can own it.
struct SelfTestRun {
    database: Arc<Database>,
    timer: Arc<dyn Timer>,
    /// The self-test proof's randomness, zeroed when dropped.
    seed: zeroize::Zeroizing<[u8; 32]>,
    started: UnixSeconds,
}

impl SelfTestRun {
    /// Runs the test off the async executor and records the result.
    async fn go(self, plan: SelfTestPlan) -> Result<SelfTestReport, SyncError> {
        let Self {
            database,
            timer,
            seed,
            started,
        } = self;
        let report = tokio::task::spawn_blocking(move || {
            prover_self_test(plan, &*timer, &mut ChaCha20Rng::from_seed(*seed))
        })
        .await
        .map_err(|_| SyncError::Internal)??;
        let record = SelfTestRecord {
            parameters: PARAMETERS_ID.to_owned(),
            verdict: stored_verdict(report.verdict),
            first: report.first,
            median: report.median,
            p95: report.p95,
            timed: report.timed,
            tested_at: started,
        };
        device_blocking(&database, move |session| {
            Ok(session.prover_self_test().save(&record)?)
        })
        .await?;
        Ok(report)
    }
}

impl SyncEngine {
    /// The stored self-test's standing now.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn prover_standing(&self) -> Result<SelfTestStanding, SyncError> {
        let now = self.now();
        self.blocking(move |session| Ok(standing(session.prover_self_test().load()?.as_ref(), now)))
            .await
    }

    /// Runs the self-test with `plan` now, off the async executor, and records the result. The
    /// timing uses the engine's timer.
    ///
    /// # Errors
    ///
    /// [`SyncError::Anon`] if proving faults (not a verdict); [`SyncError::Store`];
    /// [`SyncError::Internal`].
    pub async fn test_prover(&self, plan: SelfTestPlan) -> Result<SelfTestReport, SyncError> {
        self.self_test_run().go(plan).await
    }

    fn self_test_run(&self) -> SelfTestRun {
        let mut seed = zeroize::Zeroizing::new([0_u8; 32]);
        self.with_rng(|rng| rng.fill_bytes(seed.as_mut_slice()));
        SelfTestRun {
            database: Arc::clone(&self.database),
            timer: Arc::clone(&self.timer),
            seed,
            started: self.now(),
        }
    }

    /// Starts the configured self-test (`SyncSettings::self_test`) in the background if this is
    /// the main app, no valid result is stored, and no test is running in this process (in any
    /// engine sharing its [`SelfTestSlot`]). Returns at once; the slot owns the task. Until a test passes, requests carry
    /// Privacy Pass tokens.
    ///
    /// # Errors
    ///
    /// [`SyncError::Store`].
    pub async fn start_prover_self_test(&self) -> Result<SelfTestStart, SyncError> {
        if self.settings().role != ProcessRole::MainApp
            || self.prover_standing().await? != SelfTestStanding::Due
        {
            return Ok(SelfTestStart::NotDue);
        }
        let mut running = lock(&self.self_test.running.task);
        if running.as_ref().is_some_and(|task| !task.is_finished()) {
            return Ok(SelfTestStart::AlreadyRunning);
        }
        let run = self.self_test_run();
        let plan = self.settings().self_test;
        *running = Some(tokio::spawn(async move {
            // A fault leaves no record: the next call tries again.
            drop(run.go(plan).await);
        }));
        Ok(SelfTestStart::Started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(verdict: ProverVerdict, tested_at: u64) -> SelfTestRecord {
        SelfTestRecord {
            parameters: PARAMETERS_ID.to_owned(),
            verdict,
            first: Duration::from_millis(800),
            median: Duration::from_millis(400),
            p95: Duration::from_millis(450),
            timed: 20,
            tested_at: UnixSeconds::new(tested_at),
        }
    }

    const NOW: UnixSeconds = UnixSeconds::new(10_000_000);

    #[test]
    fn a_pass_stands_with_its_percentile() {
        assert_eq!(
            standing(Some(&record(ProverVerdict::Passed, 0)), NOW),
            SelfTestStanding::Passed {
                p95: Duration::from_millis(450)
            }
        );
    }

    #[test]
    fn no_result_or_one_for_other_parameters_is_due() {
        assert_eq!(standing(None, NOW), SelfTestStanding::Due);
        let mut old = record(ProverVerdict::Passed, 0);
        old.parameters = "rln-2/older".to_owned();
        assert_eq!(standing(Some(&old), NOW), SelfTestStanding::Due);
    }

    #[test]
    fn too_slow_is_tested_again_after_a_week() {
        let week = RETEST_AFTER.as_secs();
        let tested = NOW.get().saturating_sub(week);
        assert_eq!(
            standing(Some(&record(ProverVerdict::TooSlow, tested)), NOW),
            SelfTestStanding::Due
        );
        assert_eq!(
            standing(
                Some(&record(ProverVerdict::TooSlow, tested.saturating_add(1))),
                NOW
            ),
            SelfTestStanding::Failed
        );
    }

    #[test]
    fn a_build_fault_is_never_tested_again() {
        for verdict in [ProverVerdict::WrongParameters, ProverVerdict::ProofRejected] {
            assert_eq!(
                standing(Some(&record(verdict, 0)), NOW),
                SelfTestStanding::Failed
            );
        }
    }
}
