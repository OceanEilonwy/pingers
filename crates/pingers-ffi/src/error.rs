// SPDX-License-Identifier: AGPL-3.0-only
//! [`CoreError`]: every crate's error, flattened for the app (architecture §5).

use pingers_screens::ScreenError;
use pingers_words::Language;

/// What went wrong, for the app to show. `reason` is already in words (pingers-words) where the
/// error is the user's to fix, and plain English for logs otherwise. (The field is `reason`,
/// not `message`, which Kotlin's `Throwable` already has.)
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    /// The device clock reads before 1970.
    #[error("{reason}")]
    Clock {
        /// Why.
        reason: String,
    },
    /// Something typed, drawn or passed in is not valid: a name, a host, a shape, an id.
    #[error("{reason}")]
    Invalid {
        /// What to fix, in words.
        reason: String,
    },
    /// The Party, Meetup, member or request is gone.
    #[error("{reason}")]
    NotFound {
        /// What is gone, in words.
        reason: String,
    },
    /// The core cannot do this yet: the client is not open, or the call is not wired.
    #[error("{reason}")]
    Unavailable {
        /// Why.
        reason: String,
    },
    /// The server or a relay could not be reached, or refused.
    #[error("{reason}")]
    Network {
        /// Why.
        reason: String,
    },
    /// The encrypted database or the secure store failed.
    #[error("{reason}")]
    Storage {
        /// Why.
        reason: String,
    },
}

impl CoreError {
    /// An [`Invalid`](Self::Invalid) error with this reason.
    pub(crate) fn invalid(reason: &str) -> Self {
        Self::Invalid {
            reason: reason.to_owned(),
        }
    }
}

impl From<ScreenError> for CoreError {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "ScreenError is non-exhaustive; every other error is the user's to fix"
    )]
    fn from(error: ScreenError) -> Self {
        let reason = pingers_words::errors::screen_error(Language::English, &error);
        match error {
            ScreenError::UnknownParty
            | ScreenError::UnknownMeetup
            | ScreenError::UnknownMember
            | ScreenError::NoMeetPoint
            | ScreenError::UnknownLocationRequest => Self::NotFound { reason },
            _ => Self::Invalid { reason },
        }
    }
}

impl From<pingers_client::snapshot::SnapshotError> for CoreError {
    fn from(error: pingers_client::snapshot::SnapshotError) -> Self {
        ScreenError::from(error).into()
    }
}

impl From<pingers_geo::GeoError> for CoreError {
    fn from(error: pingers_geo::GeoError) -> Self {
        ScreenError::from(error).into()
    }
}

impl From<pingers_proto::ProtoError> for CoreError {
    fn from(error: pingers_proto::ProtoError) -> Self {
        Self::Invalid {
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CoreError;
    use pingers_screens::ScreenError;

    #[test]
    fn screen_errors_keep_their_words() {
        let error = CoreError::from(ScreenError::UnknownMeetup);
        assert!(matches!(error, CoreError::NotFound { .. }));
        assert_eq!(error.to_string(), "This meetup has ended.");
        assert!(matches!(
            CoreError::from(ScreenError::InvalidName),
            CoreError::Invalid { .. }
        ));
    }
}
