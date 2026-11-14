// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s Meetup commands: pingers-meetup-session.

use pingers_meetup::MeetPointDraft;
use pingers_proto::{ChatBody, ChatMessageId, GroupId};

use super::Client;
use crate::ClientError;

impl Client {
    /// Sends a chat message, or a reply in `parent`'s thread.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn send_chat(
        &self,
        meetup: GroupId,
        body: ChatBody,
        parent: Option<ChatMessageId>,
    ) -> Result<ChatMessageId, ClientError> {
        let result = self.crates.meetups.send_chat(meetup, body, parent);
        self.published(result.await).await
    }

    /// Sets or clears the meet point.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn set_meet_point(
        &self,
        meetup: GroupId,
        point: MeetPointDraft,
    ) -> Result<(), ClientError> {
        self.published(self.crates.meetups.set_meet_point(meetup, point).await)
            .await
    }

    /// Leaves a Meetup.
    ///
    /// # Errors
    ///
    /// [`ClientError::MeetupSession`].
    pub async fn leave_meetup(&self, meetup: GroupId) -> Result<(), ClientError> {
        self.published(self.crates.meetups.leave_meetup(meetup).await)
            .await
    }
}
