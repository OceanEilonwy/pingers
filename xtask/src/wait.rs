// SPDX-License-Identifier: AGPL-3.0-only
//! The clock and the sleeps of the local-stack and emulator commands, in one place: they poll
//! processes, ports and screens on the real clock, so these are the only call sites the
//! style guide's injected-time rule (§6) is lifted for.

use std::time::{Duration, Instant};

/// A moment some time from now, for polling loops.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadline {
    at: Instant,
}

impl Deadline {
    /// `timeout` from now.
    #[expect(
        clippy::disallowed_methods,
        reason = "xtask polls real processes and devices on the real clock"
    )]
    pub(crate) fn after(timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            at: now.checked_add(timeout).unwrap_or(now),
        }
    }

    /// Whether the moment has come.
    #[expect(
        clippy::disallowed_methods,
        reason = "xtask polls real processes and devices on the real clock"
    )]
    pub(crate) fn passed(self) -> bool {
        Instant::now() >= self.at
    }
}

/// Blocks the thread for `duration`.
#[expect(
    clippy::disallowed_methods,
    reason = "xtask polls real processes and devices on the real clock"
)]
pub(crate) fn pause(duration: Duration) {
    std::thread::sleep(duration);
}
