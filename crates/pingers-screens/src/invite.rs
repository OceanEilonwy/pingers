// SPDX-License-Identifier: AGPL-3.0-only
//! Invite people (the link and where each invite got to) and Accept invite (what an opened link
//! shows before joining).

use pingers_proto::{MailboxId, PartyId};
use pingers_time::UnixSeconds;

use crate::ScreenError;
use crate::clock::Hours;
use crate::common::{Initials, Me, Name, PartyColour, Person};
use crate::snapshot::{ClientSnapshot, InviteStatus, InviteUrl, SafetyCode};

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
    /// Opened: compare the code, then verify.
    Opened {
        /// The invite.
        id: MailboxId,
        /// The name the opener gave.
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
    /// The invites, newest first. Expired pending invites are left out.
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
    let invitations = party
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
            InviteStatus::Opened { name, code } => Some(Invitation::Opened {
                id: invite.id,
                name: name.clone(),
                code: code.clone(),
            }),
            InviteStatus::Joined { leaf } => party
                .members
                .iter()
                .find(|member| member.leaf == *leaf)
                .map(|member| Invitation::Joined {
                    id: invite.id,
                    person: Person::new(member.leaf, &member.name, member.me),
                }),
        })
        .collect();
    Ok(InviteView {
        party_name: party.name.clone(),
        link: live_link.map(|link| link.url.clone()),
        expires_in: live_link
            .map(|link| Hours::rounding_up(link.expires_at.saturating_duration_since(now))),
        invitations,
    })
}

/// How many members Accept invite shows as avatars before "+2".
pub const PREVIEW_MEMBERS: usize = 3;

/// The Accept invite screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptInvite {
    /// Who invited.
    pub inviter: Name,
    /// The Party's name.
    pub party_name: Name,
    /// Its avatar letters.
    pub party_initials: Initials,
    /// Its colour.
    pub colour: PartyColour,
    /// The first few members, as avatars.
    pub preview: Vec<Person>,
    /// How many more members than the avatars show.
    pub more_members: usize,
    /// How many members.
    pub members: usize,
    /// How many Spots.
    pub spots: usize,
    /// The code to compare with the inviter's screen.
    pub safety_code: SafetyCode,
}

/// The Accept invite screen, if an invite link is open.
#[must_use]
pub fn accept_invite(snapshot: &ClientSnapshot) -> Option<AcceptInvite> {
    let preview = snapshot.join_preview.as_ref()?;
    let members = preview.members.len();
    let mut leaf = pingers_proto::LeafIndex::new(0);
    let mut avatars = Vec::new();
    for name in preview.members.iter().take(PREVIEW_MEMBERS) {
        avatars.push(Person::new(leaf, name, Me::Other));
        leaf = leaf.checked_next().unwrap_or(leaf);
    }
    Some(AcceptInvite {
        inviter: preview.inviter.clone(),
        party_initials: Initials::of(&preview.party_name),
        party_name: preview.party_name.clone(),
        colour: preview.colour,
        preview: avatars,
        more_members: members.saturating_sub(PREVIEW_MEMBERS),
        members,
        spots: preview.spot_count,
        safety_code: preview.safety_code.clone(),
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
            matches!(opened, Invitation::Opened { name, code, .. } if name.as_str() == "Ben" && code.as_str() == "4821")
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
    fn accept_invite_previews_three_members() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = accept_invite(&snapshot).ok_or(FixtureError::Missing)?;
        assert_eq!(view.inviter.as_str(), "Ana");
        assert_eq!(view.party_initials.as_str(), "CC");
        let names: Vec<_> = view
            .preview
            .iter()
            .map(|person| person.name.as_str())
            .collect();
        assert_eq!(names, ["Ana", "Chloé", "Dev"]);
        let leaves: Vec<_> = view
            .preview
            .iter()
            .map(|person| person.leaf.get())
            .collect();
        assert_eq!(leaves, [0, 1, 2]);
        assert_eq!((view.members, view.more_members, view.spots), (5, 2, 3));
        assert_eq!(view.safety_code.as_str(), "4821 0937 5512");
        assert_eq!(view.colour, PartyColour::Blue);

        let mut none = snapshot;
        none.join_preview = None;
        assert_eq!(accept_invite(&none), None);
        Ok(())
    }
}
