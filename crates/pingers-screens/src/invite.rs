// SPDX-License-Identifier: AGPL-3.0-only
//! Invite people (the link and where each invite got to), the inviter's "Ben asked to join",
//! and the joiner's three screens of join flow A: Open invite, Asked to join, You're in.
//!
//! The link carries nothing about the Party, so the joiner's first two screens show nothing
//! about it; the Party's name, members and Spots appear only once the joiner is let in.

use pingers_proto::{MailboxId, PartyId};
use pingers_time::UnixSeconds;

use crate::ScreenError;
use crate::clock::Hours;
use crate::common::{Initials, Me, Name, PartyColour, Person};
use crate::snapshot::{ClientSnapshot, InviteStatus, InviteUrl, Joining, SafetyCode};

/// One invite in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invitation {
    /// Shared, not opened yet.
    Pending {
        /// The invite.
        id: MailboxId,
        /// How long ago it was shared, rounded down.
        shared_ago: Hours,
        /// How long until it expires, rounded up.
        expires_in: Hours,
    },
    /// Someone asked to join through it: compare the code, then let them in.
    Opened {
        /// The request, for [`join_request`].
        id: MailboxId,
        /// The name they gave.
        name: Name,
        /// The code to compare.
        code: SafetyCode,
    },
    /// Joined after the codes were verified.
    Joined {
        /// The invite.
        id: MailboxId,
        /// Who joined.
        person: Person,
    },
}

/// The Invite people screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteView {
    /// The Party's name.
    pub party_name: Name,
    /// The link to share and show as a QR code, if one has been made.
    pub link: Option<InviteUrl>,
    /// How long the link keeps working, rounded up.
    pub expires_in: Option<Hours>,
    /// Who asked to join, oldest first, then the invites, newest first. Expired pending
    /// invites are left out.
    pub invitations: Vec<Invitation>,
}

/// The Invite people screen.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`].
pub fn invite(
    snapshot: &ClientSnapshot,
    party: PartyId,
    now: UnixSeconds,
) -> Result<InviteView, ScreenError> {
    let party = snapshot.party(party)?;
    let live_link = party
        .invite_link
        .as_ref()
        .filter(|link| link.expires_at > now);
    let asked = party
        .join_requests
        .iter()
        .map(|request| Invitation::Opened {
            id: request.id,
            name: request.name.clone(),
            code: request.code.clone(),
        });
    let invites = party
        .invites
        .iter()
        .filter_map(|invite| match &invite.status {
            InviteStatus::Pending {
                shared_at,
                expires_at,
            } => (*expires_at > now).then(|| Invitation::Pending {
                id: invite.id,
                shared_ago: Hours::rounding_down(now.saturating_duration_since(*shared_at)),
                expires_in: Hours::rounding_up(expires_at.saturating_duration_since(now)),
            }),
            InviteStatus::Joined { leaf } => party
                .members
                .iter()
                .find(|member| member.leaf == *leaf)
                .map(|member| Invitation::Joined {
                    id: invite.id,
                    person: Person::new(member.leaf, &member.name, member.me),
                }),
        });
    Ok(InviteView {
        party_name: party.name.clone(),
        link: live_link.map(|link| link.url.clone()),
        expires_in: live_link
            .map(|link| Hours::rounding_up(link.expires_at.saturating_duration_since(now))),
        invitations: asked.chain(invites).collect(),
    })
}

/// "Ben asked to join": the inviter compares the code Ben reads out, then lets him in or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequestView {
    /// The request, for `decide_join`.
    pub id: MailboxId,
    /// The name they gave.
    pub name: Name,
    /// The Party's name.
    pub party_name: Name,
    /// How long ago the link they used was shared, rounded down.
    pub shared_ago: Hours,
    /// The code their phone should show.
    pub code: SafetyCode,
}

/// The "asked to join" screen for one request.
///
/// # Errors
///
/// [`ScreenError::UnknownParty`]; [`ScreenError::UnknownJoinRequest`] once it was decided or
/// withdrawn.
pub fn join_request(
    snapshot: &ClientSnapshot,
    party: PartyId,
    request: MailboxId,
    now: UnixSeconds,
) -> Result<JoinRequestView, ScreenError> {
    let party = snapshot.party(party)?;
    let asked = party
        .join_requests
        .iter()
        .find(|asked| asked.id == request)
        .ok_or(ScreenError::UnknownJoinRequest)?;
    Ok(JoinRequestView {
        id: asked.id,
        name: asked.name.clone(),
        party_name: party.name.clone(),
        shared_ago: Hours::rounding_down(now.saturating_duration_since(asked.shared_at)),
        code: asked.code.clone(),
    })
}

/// Open invite: what the joiner sees before asking. Nothing about the Party; only the name
/// they will go by, prefilled with the one they used last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenInvite {
    /// The name to prefill, once set.
    pub name: Option<Name>,
}

/// The Open invite screen.
#[must_use]
pub fn open_invite(snapshot: &ClientSnapshot) -> OpenInvite {
    OpenInvite {
        name: snapshot.display_name.clone(),
    }
}

/// Asked to join: the code to read to the inviter while they decide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskedToJoin {
    /// The code.
    pub code: SafetyCode,
}

/// The Asked to join screen, while this device waits to be let in.
#[must_use]
pub fn asked_to_join(snapshot: &ClientSnapshot) -> Option<AskedToJoin> {
    match &snapshot.joining {
        Some(Joining::Asked { code, .. }) => Some(AskedToJoin { code: code.clone() }),
        Some(Joining::LetIn { .. }) | None => None,
    }
}

/// How many members You're in shows as avatars before "+3".
pub const PREVIEW_MEMBERS: usize = 3;

/// You're in: the Party appears now, with who let this device in and the code that was
/// checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LetIn {
    /// The Party, to open or leave.
    pub party: PartyId,
    /// Who let this device in.
    pub inviter: Name,
    /// The Party's name.
    pub party_name: Name,
    /// Its avatar letters.
    pub party_initials: Initials,
    /// Its colour.
    pub colour: PartyColour,
    /// The first few other members, as avatars.
    pub preview: Vec<Person>,
    /// How many more members than the avatars show.
    pub more_members: usize,
    /// How many members, this device included.
    pub members: usize,
    /// How many Spots.
    pub spots: usize,
    /// The code that was checked.
    pub code: SafetyCode,
}

/// The You're in screen, once this device was let in and until the user opens the Party.
#[must_use]
pub fn let_in(snapshot: &ClientSnapshot) -> Option<LetIn> {
    let Some(Joining::LetIn {
        party,
        inviter,
        code,
    }) = &snapshot.joining
    else {
        return None;
    };
    let joined = snapshot.party(*party).ok()?;
    let others: Vec<Person> = joined
        .members
        .iter()
        .filter(|member| member.me == Me::Other)
        .map(|member| Person::new(member.leaf, &member.name, member.me))
        .collect();
    Some(LetIn {
        party: *party,
        inviter: inviter.clone(),
        party_initials: Initials::of(&joined.name),
        party_name: joined.name.clone(),
        colour: joined.colour,
        more_members: others.len().saturating_sub(PREVIEW_MEMBERS),
        preview: others.into_iter().take(PREVIEW_MEMBERS).collect(),
        members: joined.members.len(),
        spots: joined.spots.len(),
        code: code.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use core::time::Duration;

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn invite_lists_each_state() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = invite(&snapshot, fixtures::climbing_crew(), fixtures::at(19, 30))?;
        assert_eq!(view.party_name.as_str(), "Climbing crew");
        assert_eq!(
            view.link.as_ref().map(InviteUrl::as_str),
            Some("https://pingers.flat7.org/join#k7Qx")
        );
        assert_eq!(view.expires_in.map(Hours::get), Some(24));
        let [opened, pending, joined] = view.invitations.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert!(
            matches!(opened, Invitation::Opened { id, name, code } if *id == fixtures::ben_request() && name.as_str() == "Ben" && code.as_str() == "4821 0937 5512")
        );
        assert!(matches!(
            pending,
            Invitation::Pending { shared_ago, expires_in, .. } if shared_ago.get() == 2 && expires_in.get() == 22
        ));
        assert!(
            matches!(joined, Invitation::Joined { person, .. } if person.name.as_str() == "Chloé")
        );
        Ok(())
    }

    #[test]
    fn expired_links_and_invites_are_left_out() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        // The pending invite stops at 17:30 tomorrow, exactly.
        let at_expiry = fixtures::at(17, 30)
            .checked_add(Duration::from_hours(24))
            .ok_or(FixtureError::Missing)?;
        let view = invite(&snapshot, fixtures::climbing_crew(), at_expiry)?;
        assert!(
            !view
                .invitations
                .iter()
                .any(|invitation| matches!(invitation, Invitation::Pending { .. }))
        );
        let later = fixtures::at(19, 30)
            .checked_add(Duration::from_hours(24))
            .ok_or(FixtureError::Missing)?;
        let view = invite(&snapshot, fixtures::climbing_crew(), later)?;
        assert_eq!(view.link, None);
        assert_eq!(view.expires_in, None);
        assert!(
            !view
                .invitations
                .iter()
                .any(|invitation| matches!(invitation, Invitation::Pending { .. }))
        );
        assert_eq!(view.invitations.len(), 2);
        Ok(())
    }

    #[test]
    fn a_joined_invite_whose_member_left_is_left_out() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party
                .members
                .retain(|member| member.leaf != pingers_proto::LeafIndex::new(1));
        }
        let view = invite(&snapshot, fixtures::climbing_crew(), fixtures::at(19, 30))?;
        assert_eq!(view.invitations.len(), 2);
        Ok(())
    }

    #[test]
    fn a_join_request_shows_its_code_and_when_the_link_was_shared() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = join_request(
            &snapshot,
            fixtures::climbing_crew(),
            fixtures::ben_request(),
            fixtures::at(19, 30),
        )?;
        assert_eq!(view.id, fixtures::ben_request());
        assert_eq!(view.name.as_str(), "Ben");
        assert_eq!(view.party_name.as_str(), "Climbing crew");
        assert_eq!(view.shared_ago.get(), 2);
        assert_eq!(view.code.as_str(), "4821 0937 5512");
        assert!(matches!(
            join_request(
                &snapshot,
                fixtures::climbing_crew(),
                MailboxId::from_bytes([99; 16]),
                fixtures::at(19, 30),
            ),
            Err(ScreenError::UnknownJoinRequest)
        ));
        Ok(())
    }

    #[test]
    fn open_invite_prefills_the_name_and_nothing_else() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            open_invite(&snapshot).name.as_ref().map(Name::as_str),
            Some("Sam")
        );
        snapshot.display_name = None;
        assert_eq!(open_invite(&snapshot), OpenInvite { name: None });
        Ok(())
    }

    #[test]
    fn asked_to_join_shows_the_code_until_let_in() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let asked = asked_to_join(&snapshot).ok_or(FixtureError::Missing)?;
        assert_eq!(asked.code.as_str(), "4821 0937 5512");
        assert_eq!(let_in(&snapshot), None);
        snapshot.joining = None;
        assert_eq!(asked_to_join(&snapshot), None);
        Ok(())
    }

    #[test]
    fn let_in_shows_the_party_now_with_three_avatars() -> Result<(), FixtureError> {
        let snapshot = fixtures::let_in_snapshot()?;
        assert_eq!(asked_to_join(&snapshot), None);
        let view = let_in(&snapshot).ok_or(FixtureError::Missing)?;
        assert_eq!(view.party, fixtures::climbing_crew());
        assert_eq!(view.inviter.as_str(), "Ana");
        assert_eq!(
            (view.party_name.as_str(), view.party_initials.as_str()),
            ("Climbing crew", "CC")
        );
        let names: Vec<_> = view
            .preview
            .iter()
            .map(|person| person.name.as_str())
            .collect();
        // Others only: Sam is this device.
        assert_eq!(names, ["Ana", "Chloé", "Ben"]);
        assert_eq!((view.members, view.more_members, view.spots), (6, 2, 3));
        assert_eq!(view.code.as_str(), "4821 0937 5512");
        assert_eq!(view.colour, PartyColour::Blue);

        // A Party left since shows nothing.
        let mut gone = snapshot;
        gone.parties
            .retain(|party| party.id != fixtures::climbing_crew());
        assert_eq!(let_in(&gone), None);
        Ok(())
    }
}
