// SPDX-License-Identifier: AGPL-3.0-only
//! Fake platform, harness, vectors.
//!
//! Shared test infrastructure for every crate's tests (architecture §3, style guide §7):
//!
//! - **Time**: [`ManualClock`] (a [`pingers_time::Clock`] and [`pingers_time::Timer`] that only
//!   moves when the test moves it), [`TEST_START`], [`test_clock`] and [`advance_to`].
//! - **Randomness**: [`seeded_rng`], a seeded ChaCha20 [`rand_core::CryptoRng`].
//! - **Platform**: [`FakePlatform`], a scriptable stand-in for the Android shell: scripted fixes,
//!   Focus/DND state, captured wake scheduling, captured OS geofences, location mode and an
//!   in-memory secure store.
//! - **Events** (feature `client`): `RecordingEvents`, a `pingers_client::Events` sink that
//!   records what the core said and checks it with `Result`-returning assertions.
//! - **Fixtures**: [`PartyBuilder`] for a Party with members and Spots, and
//!   [`SpotFixture::fix_inside`] / [`SpotFixture::fix_outside`] for fixes against a Spot.
//! - **Vectors**: [`load_vectors`] for the JSON known-answer tests in `vectors/`.
//! - **Harness**: [`InProcessHarness`], the in-process server plus OHTTP relay plus push relay.
//!   The API is fixed; the wiring waits for the server crates (see the [`harness`] module).
//!
//! # Dependencies and layers
//!
//! The testkit is a `dev` crate: `cargo xtask layers` lets it depend on anything and forbids
//! any production dependency on it, so crates take it as a **dev-dependency only**.
//!
//! Cargo allows a dev-dependency cycle (crate A's tests use the testkit, which depends on A),
//! but the test build then links two copies of A: the one under test and the one the testkit
//! was built against. Their types do not unify, so `pingers_geo::Fix` from the testkit would
//! not be the `Fix` in pingers-geo's own unit tests. To keep this rare and harmless:
//!
//! 1. **Without features, the testkit depends only on L0 and L1**: pingers-time (with
//!    `test-util`), pingers-proto, pingers-geo and pingers-gate. These are pure crates whose
//!    types (fixes, circles, ids, Focus state) are the vocabulary of every fake. Nothing at L2
//!    or above, and none of the heavy L1 crates (MLS, RLN, OHTTP), so every crate can afford it.
//! 2. **Crates the testkit depends on** (time, proto, geo, gate) use it from integration tests
//!    in `tests/`, not from `#[cfg(test)]` modules: integration tests link the crate's normal
//!    library, the same copy the testkit sees. pingers-time should not use it at all.
//! 3. **Feature `client`** adds pingers-client (L3) and pingers-meetup (L1, for
//!    `ExactLocation`) and implements `pingers_client::Platform` and `pingers_client::Events`,
//!    plus [`FakeCrates`], recording stand-ins for the client's L3 ports.
//!    Only crates at or above the composition root turn it on: pingers-client's integration
//!    tests, pingers-screens, pingers-ffi, pingers-cli. CI builds with `--all-features`, so a
//!    lower crate that uses the testkit may compile pingers-client in its test build; that is
//!    slow but correct as long as rule 2 is kept for the crates pingers-client depends on.
//! 4. **Crates in L2 and L3** that define their own ports (for example a wake scheduler trait
//!    in pingers-wake) implement them for [`FakePlatform`] in their tests: the trait is local
//!    to them, so the orphan rule allows `impl MyPort for FakePlatform`.
//!
//! The fake's own vocabulary for things pingers-client defines (fix quality, location mode,
//! OS region, push registration) lives here as small mirror types ([`FixQuality`],
//! [`LocationMode`], [`Geofence`], [`PushRegistration`]); feature `client` converts them with
//! exhaustive matches, so a new variant in pingers-client fails the testkit build instead of
//! being silently dropped.
#![forbid(unsafe_code)]

#[cfg(feature = "client")]
mod client;
#[cfg(feature = "client")]
mod crates;
#[cfg(feature = "client")]
mod events;
mod fixtures;
pub mod harness;
mod platform;
mod rng;
mod time;
mod vectors;

#[cfg(feature = "client")]
pub use crates::{CrateCall, CrateName, FakeCrates, FakeCratesError};
#[cfg(feature = "client")]
pub use events::{EventAssertion, EventKind, NoticeKind, RecordedEvent, RecordingEvents};
pub use fixtures::{FixtureError, MemberFixture, PartyBuilder, PartyFixture, SpotFixture};
pub use harness::{
    CapturedPush, Endpoints, HARNESS_MAP_HOST, HARNESS_PUSH_RELAY_ID, HarnessConfig, HarnessError,
    InProcessHarness, Service,
};
#[cfg(feature = "harness")]
pub use harness::{TlsFront, send_direct};
pub use pingers_time::ManualClock;
pub use platform::{
    FakePlatform, FixQuality, FixRequest, Geofence, LocationMode, PushRegistration,
    SecureStoreError, StoreAvailability, WakeRequest,
};
pub use rng::{FixedRng, TestRng, random_array, seeded_rng};
pub use time::{ClockError, TEST_START, advance_to, test_clock};
pub use vectors::{Vector, VectorError, VectorFile, load_vectors, load_vectors_from};
