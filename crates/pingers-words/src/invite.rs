// SPDX-License-Identifier: AGPL-3.0-only
//! Invite people and Accept invite, in words.

use pingers_screens::common::Initials;
use pingers_screens::invite::{AcceptInvite, Invitation, InviteView};

use crate::Language;
use crate::common::{avatar, members, spots};

/// "Invite people": the screen's title and the button's spoken label.
pub const TITLE: &str = "Invite people";

/// An invite row, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvitationText {
    /// The avatar letters, once someone opened or joined.
    pub avatar: Option<String>,
    /// "Ben", or "Invite link".
    pub title: String,
    /// "Opened the link · compare code 4821", "Not opened yet · expires in 22 h", "Joined · code verified".
    pub status: String,
    /// "Link shared 2 h ago", for a pending invite.
    pub shared: Option<String>,
    /// "Verify", for an opened invite.
    pub verify: Option<String>,
}

/// The Invite people screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteText {
    /// "Invite people".
    pub title: String,
    /// "Scan in person, or share the link.\nEach invite works once · expires in 24 h".
    pub instructions: Option<String>,
    /// "Share invite link".
    pub share: String,
    /// "Invite to Climbing crew", the share sheet's title.
    pub share_title: String,
    /// "Invitations".
    pub invitations_heading: String,
    /// The invites.
    pub invitations: Vec<InvitationText>,
    /// "Invite QR code", the code's spoken label.
    pub qr_label: String,
}

/// The Invite people screen, in words.
#[must_use]
pub fn invite(language: Language, view: &InviteView) -> InviteText {
    let Language::English = language;
    InviteText {
        title: TITLE.into(),
        instructions: view.expires_in.map(|hours| {
            format!(
                "Scan in person, or share the link.\nEach invite works once · expires in {} h",
                hours.get()
            )
        }),
        share: "Share invite link".into(),
        share_title: format!("Invite to {}", view.party_name.as_str()),
        invitations_heading: "Invitations".into(),
        invitations: view
            .invitations
            .iter()
            .map(|invitation| match invitation {
                Invitation::Opened { name, code, .. } => InvitationText {
                    avatar: Some(Initials::of(name).as_str().to_owned()),
                    title: name.as_str().to_owned(),
                    status: format!("Opened the link · compare code {}", code.as_str()),
                    shared: None,
                    verify: Some("Verify".into()),
                },
                Invitation::Pending {
                    shared_ago,
                    expires_in,
                    ..
                } => InvitationText {
                    avatar: None,
                    title: "Invite link".into(),
                    status: format!("Not opened yet · expires in {} h", expires_in.get()),
                    shared: Some(format!("Link shared {} h ago", shared_ago.get())),
                    verify: None,
                },
                Invitation::Joined { person, .. } => InvitationText {
                    avatar: Some(avatar(language, person)),
                    title: person.name.as_str().to_owned(),
                    status: "Joined · code verified".into(),
                    shared: None,
                    verify: None,
                },
            })
            .collect(),
        qr_label: "Invite QR code".into(),
    }
}

/// The Accept invite screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptInviteText {
    /// "Ana invited you to".
    pub invited_by: String,
    /// "Climbing crew".
    pub party_name: String,
    /// "CC".
    pub party_initials: String,
    /// The avatars.
    pub avatars: Vec<String>,
    /// "+2", when there are more members than avatars.
    pub more: Option<String>,
    /// "5 members · 3 spots".
    pub counts: String,
    /// "What joining means".
    pub what_joining_means: String,
    /// The three points.
    pub points: [String; 3],
    /// "Check this code matches Ana's screen".
    pub check_code: String,
    /// "4821 0937 5512".
    pub code: String,
    /// "Codes match · Join".
    pub join: String,
    /// "Decline".
    pub decline: String,
}

/// The Accept invite screen, in words.
#[must_use]
pub fn accept_invite(language: Language, view: &AcceptInvite) -> AcceptInviteText {
    let Language::English = language;
    let inviter = view.inviter.as_str();
    AcceptInviteText {
        invited_by: format!("{inviter} invited you to"),
        party_name: view.party_name.as_str().to_owned(),
        party_initials: view.party_initials.as_str().to_owned(),
        avatars: view
            .preview
            .iter()
            .map(|person| avatar(language, person))
            .collect(),
        more: (view.more_members > 0).then(|| format!("+{}", view.more_members)),
        counts: format!(
            "{} · {}",
            members(language, view.members),
            spots(language, view.spots)
        ),
        what_joining_means: "What joining means".into(),
        points: [
            "Members find out you're nearby only when you're in the same Spot at the same time."
                .into(),
            "Your location is end-to-end encrypted. The server can't see where you are or which \
             Spot you're in."
                .into(),
            "Nothing is sent while you're in Focus or inside your exclusion zones.".into(),
        ],
        check_code: format!("Check this code matches {inviter}'s screen"),
        code: view.safety_code.as_str().to_owned(),
        join: "Codes match · Join".into(),
        decline: "Decline".into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::invite::{accept_invite as accept_screen, invite as invite_screen};

    use super::*;

    const EN: Language = Language::English;

    #[test]
    fn invite_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = invite(
            EN,
            &invite_screen(&snapshot, fixtures::climbing_crew(), fixtures::at(19, 30))?,
        );
        assert_eq!(text.title, "Invite people");
        assert_eq!(
            text.instructions.as_deref(),
            Some("Scan in person, or share the link.\nEach invite works once · expires in 24 h")
        );
        assert_eq!(text.share_title, "Invite to Climbing crew");
        let rows: Vec<_> = text
            .invitations
            .iter()
            .map(|row| (row.title.as_str(), row.status.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("Ben", "Opened the link · compare code 4821"),
                ("Invite link", "Not opened yet · expires in 22 h"),
                ("Chloé", "Joined · code verified"),
            ]
        );
        let [opened, pending, joined] = text.invitations.as_slice() else {
            return Err(FixtureError::Missing);
        };
        assert_eq!(
            (opened.avatar.as_deref(), opened.verify.as_deref()),
            (Some("BE"), Some("Verify"))
        );
        assert_eq!(
            (pending.avatar.as_deref(), pending.shared.as_deref()),
            (None, Some("Link shared 2 h ago"))
        );
        assert_eq!(
            (joined.avatar.as_deref(), joined.verify.as_deref()),
            (Some("CH"), None)
        );
        assert_eq!(
            (text.share.as_str(), text.invitations_heading.as_str()),
            ("Share invite link", "Invitations")
        );
        assert_eq!(text.qr_label, "Invite QR code");
        Ok(())
    }

    #[test]
    fn without_a_live_link_there_are_no_instructions() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.invite_link = None;
        }
        let text = invite(
            EN,
            &invite_screen(&snapshot, fixtures::climbing_crew(), fixtures::at(19, 30))?,
        );
        assert_eq!(text.instructions, None);
        Ok(())
    }

    #[test]
    fn accept_invite_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = accept_invite(EN, &accept_screen(&snapshot).ok_or(FixtureError::Missing)?);
        assert_eq!(text.invited_by, "Ana invited you to");
        assert_eq!(
            (text.party_name.as_str(), text.party_initials.as_str()),
            ("Climbing crew", "CC")
        );
        assert_eq!(text.avatars, ["AN", "CH", "DE"]);
        assert_eq!(text.more.as_deref(), Some("+2"));
        assert_eq!(text.counts, "5 members · 3 spots");
        assert_eq!(text.check_code, "Check this code matches Ana's screen");
        assert_eq!(text.code, "4821 0937 5512");
        assert_eq!(
            (text.join.as_str(), text.decline.as_str()),
            ("Codes match · Join", "Decline")
        );
        assert_eq!(text.what_joining_means, "What joining means");
        assert!(text.points.iter().all(|point| !point.is_empty()));

        let mut small = snapshot;
        if let Some(preview) = &mut small.join_preview {
            preview.members.truncate(3);
        }
        let text = accept_invite(EN, &accept_screen(&small).ok_or(FixtureError::Missing)?);
        assert_eq!(text.more, None);
        Ok(())
    }
}
