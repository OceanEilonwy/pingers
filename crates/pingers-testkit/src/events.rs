// SPDX-License-Identifier: AGPL-3.0-only
//! A recording `pingers_client::Events` sink with `Result`-returning assertions (feature
//! `client`).

use core::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_client::Notice;
use pingers_meetup::ExactLocation;
use pingers_proto::{GroupId, LeafIndex, LocationRequestId, PartyId};
use serde::Deserialize;

/// One call the core made on its `Events` sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordedEvent {
    /// `parties_changed`.
    PartiesChanged,
    /// `join_requested`.
    JoinRequested {
        /// The Party.
        party: PartyId,
    },
    /// `meetup_started`.
    MeetupStarted {
        /// The Meetup group.
        meetup: GroupId,
    },
    /// `meetup_updated`.
    MeetupUpdated {
        /// The Meetup group.
        meetup: GroupId,
    },
    /// `meetup_message`.
    MeetupMessage {
        /// The Meetup group.
        meetup: GroupId,
    },
    /// `peer_location`.
    PeerLocation {
        /// The Meetup group.
        meetup: GroupId,
        /// The peer.
        peer: LeafIndex,
    },
    /// `location_request`.
    LocationRequest {
        /// The Meetup group.
        meetup: GroupId,
        /// Who asks.
        from: LeafIndex,
        /// The request.
        request: LocationRequestId,
    },
    /// `location_access_changed`.
    LocationAccessChanged {
        /// The Meetup group.
        meetup: GroupId,
        /// The peer.
        peer: LeafIndex,
        /// The new access.
        access: ExactLocation,
    },
    /// `meetup_ended`.
    MeetupEnded {
        /// The Meetup group.
        meetup: GroupId,
    },
    /// `notice`.
    Notice(Notice),
    /// `privacy_status_changed`.
    PrivacyStatusChanged,
}

/// The kind of a [`RecordedEvent`], without its ids: what scenario files and assertions name.
/// Spelled in `snake_case` in scenario files (`"meetup_started"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// [`RecordedEvent::PartiesChanged`].
    PartiesChanged,
    /// [`RecordedEvent::JoinRequested`].
    JoinRequested,
    /// [`RecordedEvent::MeetupStarted`].
    MeetupStarted,
    /// [`RecordedEvent::MeetupUpdated`].
    MeetupUpdated,
    /// [`RecordedEvent::MeetupMessage`].
    MeetupMessage,
    /// [`RecordedEvent::PeerLocation`].
    PeerLocation,
    /// [`RecordedEvent::LocationRequest`].
    LocationRequest,
    /// [`RecordedEvent::LocationAccessChanged`].
    LocationAccessChanged,
    /// [`RecordedEvent::MeetupEnded`].
    MeetupEnded,
    /// [`RecordedEvent::Notice`], whatever the notice.
    Notice,
    /// [`RecordedEvent::PrivacyStatusChanged`].
    PrivacyStatusChanged,
}

/// The kind of a [`Notice`], without its ids. Spelled in `snake_case` in scenario files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NoticeKind {
    /// [`Notice::UnansweredKnock`].
    UnansweredKnock,
    /// [`Notice::BoardEquivocation`].
    BoardEquivocation,
    /// [`Notice::ClockSkew`].
    ClockSkew,
    /// [`Notice::DndUnavailable`].
    DndUnavailable,
    /// [`Notice::RelayKeyMismatch`].
    RelayKeyMismatch,
    /// [`Notice::BoardTooLarge`].
    BoardTooLarge,
    /// [`Notice::PushDowngraded`].
    PushDowngraded,
    /// [`Notice::FallbackActive`].
    FallbackActive,
    /// A notice added to pingers-client after this list; extend the list.
    Unrecognised,
}

impl NoticeKind {
    /// The kind of `notice`.
    #[must_use]
    pub fn of(notice: &Notice) -> Self {
        match notice {
            Notice::UnansweredKnock { .. } => Self::UnansweredKnock,
            Notice::BoardEquivocation => Self::BoardEquivocation,
            Notice::ClockSkew => Self::ClockSkew,
            Notice::DndUnavailable => Self::DndUnavailable,
            Notice::RelayKeyMismatch => Self::RelayKeyMismatch,
            Notice::BoardTooLarge => Self::BoardTooLarge,
            Notice::PushDowngraded => Self::PushDowngraded,
            Notice::FallbackActive(_) => Self::FallbackActive,
            // Notice is non_exhaustive: a notice added later maps here until listed above.
            _ => Self::Unrecognised,
        }
    }
}

impl RecordedEvent {
    /// The event's kind.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::PartiesChanged => EventKind::PartiesChanged,
            Self::JoinRequested { .. } => EventKind::JoinRequested,
            Self::MeetupStarted { .. } => EventKind::MeetupStarted,
            Self::MeetupUpdated { .. } => EventKind::MeetupUpdated,
            Self::MeetupMessage { .. } => EventKind::MeetupMessage,
            Self::PeerLocation { .. } => EventKind::PeerLocation,
            Self::LocationRequest { .. } => EventKind::LocationRequest,
            Self::LocationAccessChanged { .. } => EventKind::LocationAccessChanged,
            Self::MeetupEnded { .. } => EventKind::MeetupEnded,
            Self::Notice(_) => EventKind::Notice,
            Self::PrivacyStatusChanged => EventKind::PrivacyStatusChanged,
        }
    }

    /// The notice's kind, if this is a notice.
    #[must_use]
    pub fn notice_kind(&self) -> Option<NoticeKind> {
        match self {
            Self::Notice(notice) => Some(NoticeKind::of(notice)),
            Self::PartiesChanged
            | Self::JoinRequested { .. }
            | Self::MeetupStarted { .. }
            | Self::MeetupUpdated { .. }
            | Self::MeetupMessage { .. }
            | Self::PeerLocation { .. }
            | Self::LocationRequest { .. }
            | Self::LocationAccessChanged { .. }
            | Self::MeetupEnded { .. }
            | Self::PrivacyStatusChanged => None,
        }
    }
}

/// A failed assertion on [`RecordingEvents`]: what was expected and the kinds recorded, in
/// order (ids are left out, so the message is the same on every run).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("expected {expected}; recorded {}", RecordedKinds(.recorded))]
pub struct EventAssertion {
    /// What the assertion wanted, in words.
    pub expected: String,
    /// What was recorded.
    pub recorded: Vec<EventKind>,
}

/// Formats a list of kinds as `[a, b]`.
struct RecordedKinds<'kinds>(&'kinds [EventKind]);

impl fmt::Display for RecordedKinds<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.0).finish()
    }
}

/// An `Events` sink that records every call. Clones share the record, so a test keeps a handle
/// and gives another to the core.
///
/// Assertions return `Result<(), EventAssertion>` so tests use `?` (style guide §7).
#[derive(Clone, Debug, Default)]
pub struct RecordingEvents {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
}

impl RecordingEvents {
    /// An empty record.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `event`. The `Events` implementation calls this; fake drivers may too.
    pub fn record(&self, event: RecordedEvent) {
        self.lock().push(event);
    }

    /// Everything recorded, oldest first.
    #[must_use]
    pub fn events(&self) -> Vec<RecordedEvent> {
        self.lock().clone()
    }

    /// Everything recorded, removed from the record: for asserting phase by phase.
    #[must_use]
    pub fn take(&self) -> Vec<RecordedEvent> {
        self.lock().drain(..).collect()
    }

    /// How many events of `kind` were recorded.
    #[must_use]
    pub fn count(&self, kind: EventKind) -> usize {
        self.lock()
            .iter()
            .filter(|event| event.kind() == kind)
            .count()
    }

    /// At least one event equal to `expected` was recorded.
    ///
    /// # Errors
    ///
    /// [`EventAssertion`] if none was.
    pub fn expect_event(&self, expected: &RecordedEvent) -> Result<(), EventAssertion> {
        self.check(
            |events| events.contains(expected),
            || format!("{expected:?}"),
        )
    }

    /// At least one event of `kind` was recorded.
    ///
    /// # Errors
    ///
    /// [`EventAssertion`] if none was.
    pub fn expect_kind(&self, kind: EventKind) -> Result<(), EventAssertion> {
        self.check(
            |events| events.iter().any(|event| event.kind() == kind),
            || format!("a {kind:?} event"),
        )
    }

    /// No event of `kind` was recorded.
    ///
    /// # Errors
    ///
    /// [`EventAssertion`] if one was.
    pub fn expect_no_kind(&self, kind: EventKind) -> Result<(), EventAssertion> {
        self.check(
            |events| events.iter().all(|event| event.kind() != kind),
            || format!("no {kind:?} event"),
        )
    }

    /// At least one notice of `kind` was recorded.
    ///
    /// # Errors
    ///
    /// [`EventAssertion`] if none was.
    pub fn expect_notice(&self, kind: NoticeKind) -> Result<(), EventAssertion> {
        self.check(
            |events| events.iter().any(|event| event.notice_kind() == Some(kind)),
            || format!("a {kind:?} notice"),
        )
    }

    /// Events of these kinds were recorded in this order, possibly with others between them.
    ///
    /// # Errors
    ///
    /// [`EventAssertion`] if they were not.
    pub fn expect_in_order(&self, kinds: &[EventKind]) -> Result<(), EventAssertion> {
        self.check(
            |events| {
                let mut recorded = events.iter().map(RecordedEvent::kind);
                kinds
                    .iter()
                    .all(|wanted| recorded.any(|kind| kind == *wanted))
            },
            || format!("{kinds:?} in this order"),
        )
    }

    /// Nothing was recorded.
    ///
    /// # Errors
    ///
    /// [`EventAssertion`] if anything was.
    pub fn expect_quiet(&self) -> Result<(), EventAssertion> {
        self.check(<[RecordedEvent]>::is_empty, || String::from("no events"))
    }

    fn check(
        &self,
        holds: impl FnOnce(&[RecordedEvent]) -> bool,
        expected: impl FnOnce() -> String,
    ) -> Result<(), EventAssertion> {
        let events = self.lock();
        if holds(&events) {
            Ok(())
        } else {
            Err(EventAssertion {
                expected: expected(),
                recorded: events.iter().map(RecordedEvent::kind).collect(),
            })
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<RecordedEvent>> {
        // A test that panicked while holding the lock has already failed; keep the record usable.
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_client::Fallback;
    use pingers_proto::SpotId;

    use super::*;

    fn meetup() -> GroupId {
        GroupId::from_bytes([7; 32])
    }

    fn knock_notice() -> RecordedEvent {
        RecordedEvent::Notice(Notice::UnansweredKnock {
            party: PartyId::from_bytes([1; 16]),
            spot: SpotId::from_bytes([2; 16]),
        })
    }

    #[test]
    fn records_in_order_and_take_empties() {
        let events = RecordingEvents::new();
        let core_handle = events.clone();
        core_handle.record(RecordedEvent::PartiesChanged);
        core_handle.record(RecordedEvent::MeetupStarted { meetup: meetup() });
        assert_eq!(
            events.take(),
            [
                RecordedEvent::PartiesChanged,
                RecordedEvent::MeetupStarted { meetup: meetup() }
            ]
        );
        assert!(events.events().is_empty());
    }

    #[test]
    fn count_counts_only_that_kind() {
        let events = RecordingEvents::new();
        events.record(RecordedEvent::PartiesChanged);
        events.record(RecordedEvent::PrivacyStatusChanged);
        events.record(RecordedEvent::PartiesChanged);
        assert_eq!(events.count(EventKind::PartiesChanged), 2);
        assert_eq!(events.count(EventKind::PrivacyStatusChanged), 1);
        assert_eq!(events.count(EventKind::MeetupEnded), 0);
    }

    #[test]
    fn expect_event_compares_ids() -> Result<(), EventAssertion> {
        let events = RecordingEvents::new();
        events.record(RecordedEvent::MeetupEnded { meetup: meetup() });
        events.expect_event(&RecordedEvent::MeetupEnded { meetup: meetup() })?;
        let other = RecordedEvent::MeetupEnded {
            meetup: GroupId::from_bytes([8; 32]),
        };
        assert_eq!(
            events.expect_event(&other),
            Err(EventAssertion {
                expected: format!("{other:?}"),
                recorded: vec![EventKind::MeetupEnded],
            })
        );
        Ok(())
    }

    #[test]
    fn expect_kind_and_no_kind_are_opposites() -> Result<(), EventAssertion> {
        let events = RecordingEvents::new();
        events.record(RecordedEvent::PartiesChanged);
        events.expect_kind(EventKind::PartiesChanged)?;
        events.expect_no_kind(EventKind::MeetupStarted)?;
        assert!(events.expect_kind(EventKind::MeetupStarted).is_err());
        assert!(events.expect_no_kind(EventKind::PartiesChanged).is_err());
        Ok(())
    }

    #[test]
    fn expect_notice_matches_the_notice_kind_only() -> Result<(), EventAssertion> {
        let events = RecordingEvents::new();
        events.record(knock_notice());
        events.expect_notice(NoticeKind::UnansweredKnock)?;
        assert!(events.expect_notice(NoticeKind::ClockSkew).is_err());
        Ok(())
    }

    #[test]
    fn expect_in_order_allows_gaps_but_not_reordering() -> Result<(), EventAssertion> {
        let events = RecordingEvents::new();
        events.record(RecordedEvent::MeetupStarted { meetup: meetup() });
        events.record(RecordedEvent::MeetupMessage { meetup: meetup() });
        events.record(RecordedEvent::MeetupEnded { meetup: meetup() });
        events.expect_in_order(&[EventKind::MeetupStarted, EventKind::MeetupEnded])?;
        events.expect_in_order(&[])?;
        assert!(
            events
                .expect_in_order(&[EventKind::MeetupEnded, EventKind::MeetupStarted])
                .is_err()
        );
        assert!(
            events
                .expect_in_order(&[EventKind::MeetupStarted, EventKind::MeetupStarted])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn expect_quiet_fails_with_the_recorded_kinds() -> Result<(), EventAssertion> {
        let events = RecordingEvents::new();
        events.expect_quiet()?;
        events.record(knock_notice());
        let failure = events.expect_quiet().err();
        assert_eq!(
            failure.map(|failure| failure.to_string()),
            Some(String::from("expected no events; recorded [Notice]"))
        );
        Ok(())
    }

    #[test]
    fn every_notice_has_its_own_kind() {
        let party = PartyId::from_bytes([1; 16]);
        let spot = SpotId::from_bytes([2; 16]);
        let pairs = [
            (
                Notice::UnansweredKnock { party, spot },
                NoticeKind::UnansweredKnock,
            ),
            (Notice::BoardEquivocation, NoticeKind::BoardEquivocation),
            (Notice::ClockSkew, NoticeKind::ClockSkew),
            (Notice::DndUnavailable, NoticeKind::DndUnavailable),
            (Notice::RelayKeyMismatch, NoticeKind::RelayKeyMismatch),
            (Notice::BoardTooLarge, NoticeKind::BoardTooLarge),
            (Notice::PushDowngraded, NoticeKind::PushDowngraded),
            (
                Notice::FallbackActive(Fallback::SameOperator),
                NoticeKind::FallbackActive,
            ),
        ];
        for (notice, kind) in pairs {
            assert_eq!(NoticeKind::of(&notice), kind);
            assert_eq!(RecordedEvent::Notice(notice).notice_kind(), Some(kind));
        }
        assert_eq!(RecordedEvent::PartiesChanged.notice_kind(), None);
    }
}
