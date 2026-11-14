// SPDX-License-Identifier: AGPL-3.0-only
//! RLN proving and verification time and peak memory (design H.1 blocker 5).
//!
//! ```sh
//! cargo run --release -p pingers-anon --features prover,verifier --example prove_bench -- 20
//! ```
//!
//! The argument is the number of proofs (default 20). Peak memory is the process high-water
//! mark (`VmHWM` in `/proc/self/status`), so it works the same on Linux and Android. See
//! `crates/pingers-anon/BENCHMARK.md` for running it on a phone.
#![expect(clippy::print_stdout, reason = "a benchmark reports on stdout")]
#![expect(
    clippy::disallowed_methods,
    reason = "a benchmark measures wall time and reads the environment"
)]

use std::time::{Duration, Instant};

use pingers_anon::{
    AcceptedRoots, AnonError, LeafPosition, LeafUpdate, MembershipTree, MessageId, ProofRequest,
    RlnSecret, SelfTestPlan, VerificationContext, prove, prover_self_test, request_digest, verify,
};
use pingers_proto::{RlnScope, ServerId};
use pingers_time::Bucket;
use rand_core::SeedableRng as _;

/// Registered devices in the benchmark tree: the design's 10,000-device example.
const DEVICES: u32 = 10_000;

#[derive(Debug, thiserror::Error)]
enum BenchError {
    #[error(transparent)]
    Anon(#[from] AnonError),
    #[error("setup: {0}")]
    Setup(&'static str),
}

fn peak_memory_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .trim()
        .trim_end_matches(" kB")
        .trim()
        .parse()
        .ok()
}

fn report(label: &str) {
    match peak_memory_kib() {
        Some(kib) => println!(
            "{label}: peak RSS {:.1} MiB",
            f64::from(u32::try_from(kib).unwrap_or(u32::MAX)) / 1024.0
        ),
        None => println!("{label}: peak RSS unavailable"),
    }
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    let index = sorted
        .len()
        .saturating_sub(1)
        .saturating_mul(percent)
        .checked_div(100)
        .unwrap_or(0);
    sorted.get(index).copied().unwrap_or_default()
}

fn main() -> Result<(), BenchError> {
    let runs: usize = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(20);
    let mut rng = rand_chacha::ChaCha20Rng::seed_from_u64(1);
    report("start");

    let secret = RlnSecret::generate(&mut rng)?;
    let mut updates = Vec::new();
    for _ in 1..DEVICES {
        updates.push(LeafUpdate::Appended(
            RlnSecret::generate(&mut rng)?.id_commitment()?.leaf()?,
        ));
    }
    updates.push(LeafUpdate::Appended(secret.id_commitment()?.leaf()?));
    let started = Instant::now();
    let mut tree = MembershipTree::empty()?;
    tree.apply(&updates)?;
    println!("tree of {DEVICES} leaves built in {:?}", started.elapsed());
    report("after tree");

    let path = tree.path(LeafPosition::new(DEVICES.saturating_sub(1)))?;
    let server = ServerId::new("https://pingers.example".to_owned())
        .map_err(|_| BenchError::Setup("server id"))?;
    let bucket = Bucket::from_index(1_900_000).map_err(|_| BenchError::Setup("bucket"))?;
    let digest = request_digest("POST", "/v1/board/sync", &[0; 424])?;
    let request = ProofRequest {
        secret: &secret,
        path: &path,
        root: tree.root(),
        server: &server,
        scope: RlnScope::Board,
        bucket,
        message_id: MessageId::new(0)?,
        digest,
    };

    let started = Instant::now();
    let first = prove(&request, &mut rng)?;
    println!("first proof (loads the key) in {:?}", started.elapsed());
    report("after first proof");

    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        prove(&request, &mut rng)?;
        times.push(started.elapsed());
    }
    times.sort_unstable();
    println!(
        "prove x{runs}: median {:?}, p95 {:?}, max {:?}",
        percentile(&times, 50),
        percentile(&times, 95),
        times.last().copied().unwrap_or_default()
    );
    report("after proving");

    let context = VerificationContext {
        roots: AcceptedRoots::first(tree.root()),
        server: &server,
        expected: digest,
    };
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        verify(&first, &context)?;
        times.push(started.elapsed());
    }
    times.sort_unstable();
    println!(
        "verify x{runs}: median {:?}, p95 {:?}",
        percentile(&times, 50),
        percentile(&times, 95)
    );

    // The self-test the app runs before it proves (design B.4.8), with the design's plan.
    let wall = WallTimer(Instant::now());
    let started = Instant::now();
    let self_test = prover_self_test(SelfTestPlan::DESIGN, &wall, &mut rng)?;
    println!(
        "self-test: {:?} in {:?} (first {:?}, median {:?}, p95 {:?}, {} timed)",
        self_test.verdict,
        started.elapsed(),
        self_test.first,
        self_test.median,
        self_test.p95,
        self_test.timed
    );
    report("end");
    Ok(())
}

/// Monotonic time from the process's own clock, for the self-test.
struct WallTimer(Instant);

impl pingers_time::Timer for WallTimer {
    fn now(&self) -> pingers_time::MonotonicTime {
        pingers_time::MonotonicTime::from_elapsed(self.0.elapsed())
    }

    fn sleep_until(&self, _deadline: pingers_time::MonotonicTime) -> pingers_time::Sleep<'_> {
        // The self-test never sleeps.
        Box::pin(core::future::ready(()))
    }
}
