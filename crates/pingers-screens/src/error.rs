// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type. pingers-words says each one to the user.

use pingers_client::snapshot::SnapshotError;
use pingers_geo::GeoError;

/// Why a view model could not be built, or a value for one was rejected. Never carries names,
/// coordinates or identifiers (style guide §9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScreenError {
    /// The snapshot has no Party with this id.
    #[error("no such party")]
    UnknownParty,
    /// The snapshot has no Meetup with this id.
    #[error("no such meetup")]
    UnknownMeetup,
    /// The Meetup has no member with this leaf.
    #[error("no such meetup member")]
    UnknownMember,
    /// The Meetup has no meet point.
    #[error("no meet point")]
    NoMeetPoint,
    /// The snapshot has no location request with this id.
    #[error("no such location request")]
    UnknownLocationRequest,
    /// A name was empty, only spaces, or too long.
    #[error("name is empty or too long")]
    InvalidName,
    /// A server or relay host was empty or contained spaces.
    #[error("host is not valid")]
    InvalidHost,
    /// An operator name was empty.
    #[error("operator name is empty")]
    InvalidOperator,
    /// A safety code was not groups of digits.
    #[error("safety code is not valid")]
    InvalidSafetyCode,
    /// An invite link was empty or contained spaces.
    #[error("invite link is not valid")]
    InvalidInviteLink,
    /// A daily window that starts where it ends.
    #[error("window is empty")]
    EmptyWindow,
    /// A UTC offset outside ±18 hours.
    #[error("UTC offset out of range")]
    InvalidUtcOffset,
    /// The drawn area is still open.
    #[error("drawn area is not closed")]
    ShapeOpen,
    /// The drawn area is not a valid polygon.
    #[error("drawn area is not valid")]
    Shape(#[source] GeoError),
}

impl From<GeoError> for ScreenError {
    fn from(error: GeoError) -> Self {
        Self::Shape(error)
    }
}

impl From<SnapshotError> for ScreenError {
    fn from(error: SnapshotError) -> Self {
        match error {
            SnapshotError::UnknownParty => Self::UnknownParty,
            SnapshotError::UnknownMeetup => Self::UnknownMeetup,
            SnapshotError::UnknownMember => Self::UnknownMember,
            SnapshotError::InvalidName => Self::InvalidName,
            SnapshotError::InvalidHost => Self::InvalidHost,
            SnapshotError::InvalidOperator => Self::InvalidOperator,
            SnapshotError::InvalidSafetyCode => Self::InvalidSafetyCode,
            SnapshotError::InvalidInviteLink => Self::InvalidInviteLink,
            SnapshotError::EmptyWindow => Self::EmptyWindow,
        }
    }
}
