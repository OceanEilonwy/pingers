// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s location consent: the allow-list is per Party member (pingers-parties); asks,
//! answers and revokes happen inside a Meetup (pingers-meetup-session).

use pingers_meetup::LocationAnswer;
use pingers_proto::{GroupId, LeafIndex, LocationRequestId, PartyId};

use super::Client;
use crate::ClientError;
use crate::ports::AllowListChange;

impl Client {
    /// Adds a member to, or takes them off, the location allow-list.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn set_always_allow(
        &self,
        party: PartyId,
        member: LeafIndex,
        choice: AllowListChange,
    ) -> Result<(), ClientError> {
        let result = self.crates.parties.set_always_allow(party, member, choice);
        self.published(result.await).await
    }

    /// Asks a Meetup member to see their exact location.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn ask_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<LocationRequestId, ClientError> {
        self.published(self.crates.meetups.ask_location(meetup, peer).await)
            .await
    }

    /// Answers a member's location request.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn answer_location_request(
        &self,
        meetup: GroupId,
        request: LocationRequestId,
        answer: LocationAnswer,
    ) -> Result<(), ClientError> {
        let result = self
            .crates
            .meetups
            .answer_location_request(meetup, request, answer);
        self.published(result.await).await
    }

    /// Stops sharing this device's exact location with a member.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn revoke_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<(), ClientError> {
        self.published(self.crates.meetups.revoke_location(meetup, peer).await)
            .await
    }
}
