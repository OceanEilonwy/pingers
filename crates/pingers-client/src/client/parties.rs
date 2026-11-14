// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s Party and Spot commands: pingers-parties.

use pingers_proto::{LeafIndex, MailboxId, PartyId, SpotId};

use super::Client;
use crate::ClientError;
use crate::ports::{JoinDecision, PartyDraft, SpotDraft};
use crate::snapshot::{InviteLink, InviteUrl, Name};

impl Client {
    /// Creates a Party (New party).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn create_party(&self, draft: PartyDraft) -> Result<PartyId, ClientError> {
        self.published(self.crates.parties.create_party(draft).await)
            .await
    }

    /// Makes an invite link (Invite).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn create_invite(&self, party: PartyId) -> Result<InviteLink, ClientError> {
        self.published(self.crates.parties.create_invite(party).await)
            .await
    }

    /// Opens an invite link; the snapshot's join preview then shows the code to compare.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn open_invite(&self, link: InviteUrl, my_name: Name) -> Result<(), ClientError> {
        self.published(self.crates.parties.open_invite(link, my_name).await)
            .await
    }

    /// Joins the opened invite's Party once the codes matched (Accept invite).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn accept_invite(&self) -> Result<PartyId, ClientError> {
        self.published(self.crates.parties.accept_invite().await)
            .await
    }

    /// Admits or refuses someone who opened this device's invite.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn decide_join(
        &self,
        party: PartyId,
        invite: MailboxId,
        decision: JoinDecision,
    ) -> Result<(), ClientError> {
        let result = self.crates.parties.decide_join(party, invite, decision);
        self.published(result.await).await
    }

    /// Cancels an invite this device made (Invite people's ✕ on a pending invite).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn cancel_invite(
        &self,
        party: PartyId,
        invite: MailboxId,
    ) -> Result<(), ClientError> {
        self.published(self.crates.parties.cancel_invite(party, invite).await)
            .await
    }

    /// Sets the name this device's user goes by in Parties they create or join from now on.
    ///
    /// # Errors
    ///
    /// [`ClientError::Records`] or [`ClientError::Store`] if it cannot be stored.
    pub async fn set_display_name(&self, name: Name) -> Result<(), ClientError> {
        self.published(self.crates.parties.set_display_name(name).await)
            .await
    }

    /// Removes a member from a Party.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn remove_member(
        &self,
        party: PartyId,
        member: LeafIndex,
    ) -> Result<(), ClientError> {
        self.published(self.crates.parties.remove_member(party, member).await)
            .await
    }

    /// Leaves a Party.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn leave_party(&self, party: PartyId) -> Result<(), ClientError> {
        self.published(self.crates.parties.leave_party(party).await)
            .await
    }

    /// Creates or replaces a Spot (Spot editor).
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn save_spot(&self, party: PartyId, spot: SpotDraft) -> Result<SpotId, ClientError> {
        self.published(self.crates.parties.save_spot(party, spot).await)
            .await
    }

    /// Deletes a Spot.
    ///
    /// # Errors
    ///
    /// [`ClientError::Parties`].
    pub async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), ClientError> {
        self.published(self.crates.parties.delete_spot(party, spot).await)
            .await
    }
}
