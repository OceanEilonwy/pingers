// SPDX-License-Identifier: AGPL-3.0-only
//! This device's roster profile in each Party: its display name and the id of its Party group
//! subscription (design B.2, "Party state operations"). A change is stored at once and
//! published to the Party as a profile operation, which every member applies
//! (`Incoming::ProfileUpdated`); while a commit waits, the profile waits with it.

use pingers_mls::{MlsContext, MlsError};
use pingers_proto::{PartyId, SubscriptionId};
use pingers_store::{MemberName, Session};
use rand_core::CryptoRng;

use crate::lifecycle::{identity, party_row, seal_operation, set_subscription, subscription_of};
use crate::ops::PartyOperation;
use crate::{Outbound, PartiesError, PartyLifecycle};

/// What changed in this device's profile.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProfileChange {
    /// The user changed their display name.
    Name(MemberName),
    /// This device replaced its Party group subscription (a new id), or dropped it (`None`,
    /// wake-up pushes turned off), so a member who removes this device later drops the right
    /// one.
    Subscription(Option<SubscriptionId>),
}

/// Whether the changed profile can go out now.
#[must_use]
pub enum ProfilePublication {
    /// Send this group message ([`Outbound::GroupMessage`]; boxed, it is large).
    Send(Box<Outbound>),
    /// A commit is waiting for the server, or a leaver's proposal for a commit; nothing can be
    /// sent in this epoch. The change is kept; [`PartyLifecycle::publish_waiting_profile`]
    /// sends it once the commit has settled.
    Waiting,
}

impl PartyLifecycle<'_> {
    /// Changes this device's profile in a Party and publishes it to the other members, who
    /// show the new name in their rosters (design B.2).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::NoAccount`]; [`PartiesError::Mls`];
    /// [`PartiesError::Store`]. Nothing changes on an error.
    pub fn update_profile(
        &self,
        party: &PartyId,
        change: ProfileChange,
        rng: &mut impl CryptoRng,
    ) -> Result<ProfilePublication, PartiesError> {
        self.session.transaction(|session| {
            let row = party_row(session, party)?;
            match change {
                ProfileChange::Name(name) => {
                    let mut own = session
                        .members()
                        .get(party, row.own_leaf)?
                        .ok_or(PartiesError::Internal)?;
                    own.name = name;
                    session.members().put(&own)?;
                }
                ProfileChange::Subscription(subscription) => {
                    let own_key = identity(session)?.signing_key.public_key();
                    set_subscription(session, party, own_key, subscription)?;
                }
            }
            session.profiles_waiting().mark(party)?;
            Ok(match publish(session, party, rng)? {
                Some(outbound) => ProfilePublication::Send(Box::new(outbound)),
                None => ProfilePublication::Waiting,
            })
        })
    }

    /// Sends this device's profile if a change is still waiting and nothing blocks it any
    /// more; `None` if nothing waits or it still cannot go. Call after a commit of the Party
    /// settles (accepted, rejected, or another member's merged).
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::NoAccount`]; [`PartiesError::Mls`];
    /// [`PartiesError::Store`].
    pub fn publish_waiting_profile(
        &self,
        party: &PartyId,
        rng: &mut impl CryptoRng,
    ) -> Result<Option<Outbound>, PartiesError> {
        self.session.transaction(|session| {
            party_row(session, party)?;
            if !session.profiles_waiting().is_waiting(party)? {
                return Ok(None);
            }
            publish(session, party, rng)
        })
    }

    /// Whether this device's changed profile is still waiting to be sent.
    ///
    /// # Errors
    ///
    /// [`PartiesError::UnknownParty`]; [`PartiesError::Store`].
    pub fn profile_waiting(&self, party: &PartyId) -> Result<bool, PartiesError> {
        party_row(self.session, party)?;
        Ok(self.session.profiles_waiting().is_waiting(party)?)
    }
}

/// Seals this device's current profile, unless a commit blocks sending; clears the waiting
/// mark once it is sealed.
fn publish(
    session: &Session,
    party: &PartyId,
    rng: &mut impl CryptoRng,
) -> Result<Option<Outbound>, PartiesError> {
    if session.pending_commits().get(party)?.is_some() {
        return Ok(None);
    }
    let row = party_row(session, party)?;
    let identity = identity(session)?;
    let own_key = identity.signing_key.public_key();
    let own = session
        .members()
        .get(party, row.own_leaf)?
        .ok_or(PartiesError::Internal)?;
    let operation = PartyOperation::Profile {
        name: own.name,
        subscription: subscription_of(session, party, &own_key)?,
    };
    let context = MlsContext::new(session, &identity.signing_key);
    let mut group = context.party_group(&row.group)?;
    match seal_operation(&mut group, &operation, rng) {
        Ok(outbound) => {
            session.profiles_waiting().clear(party)?;
            Ok(Some(outbound))
        }
        // A leaver's proposal waits for a commit (this device's or another member's).
        Err(PartiesError::Mls(MlsError::CommitPending)) => Ok(None),
        Err(error) => Err(error),
    }
}
