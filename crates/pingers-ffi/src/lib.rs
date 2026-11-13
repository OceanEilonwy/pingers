// SPDX-License-Identifier: AGPL-3.0-only
//! UniFFI bindings.
//!
//! UniFFI only (architecture §3, L5). It converts types, maps errors, turns async calls into
//! Kotlin `suspend`, and exposes `Platform` and `Events` as callback interfaces. No logic: each
//! exported call is one call into pingers-client, pingers-screens or pingers-words and a
//! conversion.
//!
//! - [`Core`]: the client. Opened with a [`Platform`] and an [`Events`] sink; actions are
//!   `suspend` functions; [`Core::snapshot`] returns the state the screens are computed from.
//! - [`Snapshot`]: each screen's view model, as records (views.rs).
//! - [`AreaEditor`]: the Draw area editing model.
//! - [`CoreError`]: every error, flattened.
//!
//! Feature `wipe-freed` (default) installs a zeroing global allocator (wiping.rs), the
//! workspace's one `unsafe` exception.

uniffi::setup_scaffolding!();

mod callbacks;
mod core;
mod error;
mod geo;
mod ids;
mod snapshot;
mod views;
#[cfg(feature = "wipe-freed")]
mod wiping;

pub use callbacks::{
    Events, FixQuality, FocusState, LocationMode, Notice, OsRegion, Platform, PlatformError,
    PushPlatform, PushTokens,
};
pub use core::{Core, GeofenceEvent, MeetPointChoice, NewPartyDraft, SettingChange, SpotToSave};
pub use error::CoreError;
pub use geo::{Coordinate, Fence, Fix};
pub use ids::{
    InviteId, LocationRequestId, MeetupId, MemberId, MessageId, PartyId, SpotId, ZoneId,
};
pub use snapshot::{AreaEditor, LocalClock, Snapshot};
pub use views::*;

/// Freed memory is zeroed, so secrets and positions copied on their way to and from the app do
/// not outlive their use (see wiping.rs).
#[cfg(feature = "wipe-freed")]
#[global_allocator]
static ALLOCATOR: wiping::Wiping = wiping::Wiping;

/// The version of the Rust core, for the app's security details.
#[uniffi::export]
#[must_use]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

/// A 15-minute bucket (pingers-time) as plain numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct BucketWindow {
    /// The bucket index `b = floor(unix_seconds / 900)`.
    pub index: u64,
    /// The first second of the bucket, in Unix seconds.
    pub start: u64,
    /// The bucket's length in seconds.
    pub length_seconds: u64,
}

impl From<pingers_time::Bucket> for BucketWindow {
    fn from(bucket: pingers_time::Bucket) -> Self {
        Self {
            index: bucket.index(),
            start: bucket.start().get(),
            length_seconds: pingers_time::Bucket::LENGTH.as_secs(),
        }
    }
}

/// The bucket the device clock is in now.
///
/// # Errors
///
/// [`CoreError::Clock`] if the system clock reads before the Unix epoch.
#[uniffi::export]
pub fn current_bucket() -> Result<BucketWindow, CoreError> {
    let now = pingers_time::SystemClock
        .try_now()
        .map_err(|error| CoreError::Clock {
            reason: error.to_string(),
        })?;
    Ok(now.bucket().into())
}

#[cfg(test)]
mod tests {
    use super::{BucketWindow, core_version};

    #[test]
    fn version_is_the_crate_version() {
        assert_eq!(core_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn bucket_window_converts_fields() {
        let bucket = pingers_time::UnixSeconds::new(1_800).bucket();
        assert_eq!(
            BucketWindow::from(bucket),
            BucketWindow {
                index: 2,
                start: 1_800,
                length_seconds: 900
            }
        );
    }
}
