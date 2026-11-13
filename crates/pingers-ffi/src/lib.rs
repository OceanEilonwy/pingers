// SPDX-License-Identifier: AGPL-3.0-only
//! UniFFI bindings.
//!
//! UniFFI only. It converts types, maps errors, turns async calls into Kotlin `suspend`, and
//! exposes `Platform` and `Events` as callback interfaces. No logic. A `wipe-freed` feature
//! with a zeroing allocator will be added as in fob-ffi.
//!
//! Wave 2 exports two calls that prove the bridge end to end: [`core_version`] and
//! [`current_bucket`]. The client API arrives with pingers-client's async API.

uniffi::setup_scaffolding!();

/// What went wrong, for the app to show. (The field is `reason`, not `message`, which
/// Kotlin's `Throwable` already has.)
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    /// The device clock reads before 1970.
    #[error("{reason}")]
    Clock {
        /// Why, in English, for logs.
        reason: String,
    },
}

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
