// SPDX-License-Identifier: AGPL-3.0-only
//! Errors from constructing time values.

/// Why a time value could not be constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TimeError {
    /// The bucket index is so large that the bucket's start would not fit in [`crate::UnixSeconds`].
    #[error("bucket index is beyond the representable range")]
    BucketOutOfRange,
    /// The system clock reads earlier than the Unix epoch.
    #[error("system clock is before the Unix epoch")]
    BeforeEpoch,
}
