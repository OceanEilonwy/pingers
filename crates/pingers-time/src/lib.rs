// SPDX-License-Identifier: AGPL-3.0-only
//! Clock and Timer traits, `UnixSeconds`, `Bucket` (see style guide §6).
//!
//! Nothing else in the workspace reads the system clock. Code that needs wall time takes a
//! [`Clock`]; code that needs timeouts or cadences takes a [`Timer`]. Tests use
//! [`ManualClock`] (feature `test-util`) and advance it explicitly.
//!
//! - [`UnixSeconds`]: a wall-clock instant in whole seconds since the Unix epoch.
//! - [`Bucket`]: the 15-minute window `floor(unix_seconds / 900)` that leases, rate limits and
//!   tags are keyed on (design §0, F.1).
//! - [`MonotonicTime`]: a reading of a monotonic clock, for deadlines that must not jump when
//!   the wall clock is corrected.
#![forbid(unsafe_code)]

mod bucket;
mod clock;
mod error;
#[cfg(feature = "test-util")]
mod manual;
mod timer;
mod unix;

pub use bucket::{Bucket, BucketCount};
pub use clock::{Clock, SystemClock};
pub use error::TimeError;
#[cfg(feature = "test-util")]
pub use manual::ManualClock;
pub use timer::{MonotonicTime, Sleep, Timer};
pub use unix::UnixSeconds;
