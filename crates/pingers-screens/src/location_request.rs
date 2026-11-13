// SPDX-License-Identifier: AGPL-3.0-only
//! A member asks to see this device's exact location: who, where, and the answer so far.

use pingers_meetup::{AllowListChoice, AskBack, GrantScope};
use pingers_proto::LocationRequestId;
use pingers_time::UnixSeconds;

use crate::ScreenError;
use crate::common::{Name, Person};
use crate::home::meetup_names;
use crate::snapshot::ClientSnapshot;

/// The answer as the user has set it so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnswerDraft {
    /// "Share for": this Meetup, or 15 minutes.
    pub scope: GrantScope,
    /// "Ask to see Ben too".
    pub ask_back: AskBack,
    /// "Always allow Ben in Climbing crew".
    pub always: AllowListChoice,
}

impl AnswerDraft {
    /// What the screen starts with: this Meetup, ask back, just this time.
    pub const INITIAL: Self = Self {
        scope: GrantScope::ThisMeetup,
        ask_back: AskBack::AskBack,
        always: AllowListChoice::JustThisTime,
    };
}

/// The Location request screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationRequestView {
    /// The request.
    pub id: LocationRequestId,
    /// Who asks.
    pub from: Person,
    /// The Spot of the Meetup.
    pub spot_name: Name,
    /// Its Party.
    pub party_name: Name,
    /// The answer so far.
    pub answer: AnswerDraft,
    /// When it lapses unanswered.
    pub expires_at: UnixSeconds,
}

/// The Location request screen.
///
/// # Errors
///
/// [`ScreenError::UnknownLocationRequest`]; [`ScreenError::UnknownMeetup`] if its Meetup, Party
/// or Spot is gone; [`ScreenError::UnknownMember`] if the asker has left.
pub fn location_request(
    snapshot: &ClientSnapshot,
    id: LocationRequestId,
    answer: AnswerDraft,
) -> Result<LocationRequestView, ScreenError> {
    let request = snapshot
        .location_requests
        .iter()
        .find(|request| request.id == id)
        .ok_or(ScreenError::UnknownLocationRequest)?;
    let meetup = snapshot.meetup(request.meetup)?;
    let names = meetup_names(snapshot, meetup).ok_or(ScreenError::UnknownMeetup)?;
    let peer = meetup.peer(request.from)?;
    Ok(LocationRequestView {
        id,
        from: Person::new(peer.leaf, &peer.name, crate::common::Me::Other),
        spot_name: names.spot,
        party_name: names.party,
        answer,
        expires_at: request.expires_at,
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn the_request_names_who_and_where() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = location_request(
            &snapshot,
            fixtures::location_request_id(),
            AnswerDraft::INITIAL,
        )?;
        assert_eq!(view.from.name.as_str(), "Ben");
        assert_eq!(view.spot_name.as_str(), "Riverside Park");
        assert_eq!(view.party_name.as_str(), "Climbing crew");
        assert_eq!(view.answer, AnswerDraft::INITIAL);
        assert_eq!(view.expires_at, fixtures::at(19, 35));
        assert_eq!(view.id, fixtures::location_request_id());
        Ok(())
    }

    #[test]
    fn a_request_that_cannot_be_shown_is_an_error() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            location_request(
                &snapshot,
                LocationRequestId::from_bytes([0; 16]),
                AnswerDraft::INITIAL
            ),
            Err(ScreenError::UnknownLocationRequest)
        );
        for meetup in &mut snapshot.meetups {
            meetup.others.clear();
        }
        assert_eq!(
            location_request(
                &snapshot,
                fixtures::location_request_id(),
                AnswerDraft::INITIAL
            ),
            Err(ScreenError::UnknownMember)
        );
        snapshot.parties.clear();
        assert_eq!(
            location_request(
                &snapshot,
                fixtures::location_request_id(),
                AnswerDraft::INITIAL
            ),
            Err(ScreenError::UnknownMeetup)
        );
        Ok(())
    }
}
