// SPDX-License-Identifier: AGPL-3.0-only
//! Invite people, "Ben asked to join", and the joiner's Open invite, Asked to join and You're
//! in, in words (canvas `JoinA1`, `JoinA2`, `JoinA3` and `InviteApprove`).

use pingers_screens::common::Initials;
use pingers_screens::invite::{
    AskedToJoin, Invitation, InviteView, JoinRequestView, LetIn, OpenInvite,
};

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
    /// "Opened the link · compare code 4821 0937 5512", "Not opened yet · expires in 22 h",
    /// "Joined · code verified".
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

/// "Ben asked to join", in words (canvas `InviteApprove`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequestText {
    /// "BE".
    pub avatar: String,
    /// "Ben asked to join".
    pub title: String,
    /// "Climbing crew · from the link you shared 2 h ago".
    pub subtitle: String,
    /// "Ask Ben to read their code. It should be:".
    pub ask: String,
    /// "4821 0937 5512".
    pub code: String,
    /// "A different code means someone else opened your link. Don't let them in."
    pub warning: String,
    /// "When you let Ben in".
    pub when_heading: String,
    /// "Ben sees the Party's name, its members and its Spots, and everyone gets a new key. The
    /// link stops working."
    pub when_body: String,
    /// "Codes match · Let Ben in".
    pub let_in: String,
    /// "Don't let in".
    pub turn_away: String,
}

/// "Ben asked to join", in words. The canvas says "his code" and "He sees"; the app does not
/// know the joiner's pronouns, so these read "their code" and "Ben sees".
#[must_use]
pub fn join_request(language: Language, view: &JoinRequestView) -> JoinRequestText {
    let Language::English = language;
    let name = view.name.as_str();
    JoinRequestText {
        avatar: Initials::of(&view.name).as_str().to_owned(),
        title: format!("{name} asked to join"),
        subtitle: format!(
            "{} · from the link you shared {} h ago",
            view.party_name.as_str(),
            view.shared_ago.get()
        ),
        ask: format!("Ask {name} to read their code. It should be:"),
        code: view.code.as_str().to_owned(),
        warning: "A different code means someone else opened your link. Don't let them in.".into(),
        when_heading: format!("When you let {name} in"),
        when_body: format!(
            "{name} sees the Party's name, its members and its Spots, and everyone gets a new \
             key. The link stops working."
        ),
        let_in: format!("Codes match · Let {name} in"),
        turn_away: "Don't let in".into(),
    }
}

/// The Open invite screen, in words (canvas `JoinA1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenInviteText {
    /// "You've been invited to a Party".
    pub title: String,
    /// "You'll see its name and who's in it once the person who invited you lets you in."
    pub subtitle: String,
    /// "What joining means".
    pub what_joining_means: String,
    /// The two points.
    pub points: [String; 2],
    /// "Your name in this Party".
    pub name_label: String,
    /// The name to prefill, or empty.
    pub name: String,
    /// "Ask to join".
    pub ask: String,
    /// "Not now".
    pub not_now: String,
}

/// The Open invite screen, in words.
#[must_use]
pub fn open_invite(language: Language, view: &OpenInvite) -> OpenInviteText {
    let Language::English = language;
    OpenInviteText {
        title: "You've been invited to a Party".into(),
        subtitle: "You'll see its name and who's in it once the person who invited you lets you \
                   in."
        .into(),
        what_joining_means: "What joining means".into(),
        points: [
            "Members find out you're nearby only when you're in the same Spot at the same time."
                .into(),
            "Your location is end-to-end encrypted. The server can't see where you are or which \
             Spot you're in."
                .into(),
        ],
        name_label: "Your name in this Party".into(),
        name: view
            .name
            .as_ref()
            .map_or_else(String::new, |name| name.as_str().to_owned()),
        ask: "Ask to join".into(),
        not_now: "Not now".into(),
    }
}

/// The Asked to join screen, in words (canvas `JoinA2`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskedToJoinText {
    /// "Asked to join".
    pub title: String,
    /// "The person who invited you sees your request with the same code. When they let you in,
    /// the Party appears on your home screen."
    pub subtitle: String,
    /// "Read this code to them, in person or on a call".
    pub read_code: String,
    /// "4821 0937 5512".
    pub code: String,
    /// "If their code is different, someone else may have your link. Tell them not to let you
    /// in."
    pub warning: String,
    /// "You can leave this screen. We'll notify you when you're in."
    pub leave_note: String,
    /// "Done".
    pub done: String,
    /// "Withdraw request".
    pub withdraw: String,
}

/// The Asked to join screen, in words.
#[must_use]
pub fn asked_to_join(language: Language, view: &AskedToJoin) -> AskedToJoinText {
    let Language::English = language;
    AskedToJoinText {
        title: "Asked to join".into(),
        subtitle: "The person who invited you sees your request with the same code. When they \
                   let you in, the Party appears on your home screen."
            .into(),
        read_code: "Read this code to them, in person or on a call".into(),
        code: view.code.as_str().to_owned(),
        warning: "If their code is different, someone else may have your link. Tell them not to \
                  let you in."
            .into(),
        leave_note: "You can leave this screen. We'll notify you when you're in.".into(),
        done: "Done".into(),
        withdraw: "Withdraw request".into(),
    }
}

/// The You're in screen, in words (canvas `JoinA3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LetInText {
    /// "CC".
    pub party_initials: String,
    /// "Ana let you in".
    pub let_in_by: String,
    /// "Climbing crew".
    pub party_name: String,
    /// The avatars.
    pub avatars: Vec<String>,
    /// "+2", when there are more members than avatars.
    pub more: Option<String>,
    /// "6 members · 3 spots".
    pub counts: String,
    /// "Code 4821 0937 5512 was checked before Ana let you in. You can see it again in the
    /// Party's security details."
    pub checked: String,
    /// "Open Climbing crew".
    pub open: String,
    /// "Leave Party".
    pub leave: String,
    /// The notification posted when the Welcome arrives: "Ana let you in".
    pub notification_title: String,
    /// "Climbing crew".
    pub notification_body: String,
}

/// The You're in screen, in words.
#[must_use]
pub fn let_in(language: Language, view: &LetIn) -> LetInText {
    let Language::English = language;
    let inviter = view.inviter.as_str();
    let party_name = view.party_name.as_str();
    LetInText {
        party_initials: view.party_initials.as_str().to_owned(),
        let_in_by: format!("{inviter} let you in"),
        party_name: party_name.to_owned(),
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
        checked: format!(
            "Code {} was checked before {inviter} let you in. You can see it again in the \
             Party's security details.",
            view.code.as_str()
        ),
        open: format!("Open {party_name}"),
        leave: "Leave Party".into(),
        notification_title: format!("{inviter} let you in"),
        notification_body: party_name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::invite::{
        asked_to_join as asked_screen, invite as invite_screen, join_request as request_screen,
        let_in as let_in_screen, open_invite as open_screen,
    };

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
                ("Ben", "Opened the link · compare code 4821 0937 5512"),
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
    fn join_request_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = join_request(
            EN,
            &request_screen(
                &snapshot,
                fixtures::climbing_crew(),
                fixtures::ben_request(),
                fixtures::at(19, 30),
            )?,
        );
        assert_eq!(text.avatar, "BE");
        assert_eq!(text.title, "Ben asked to join");
        assert_eq!(
            text.subtitle,
            "Climbing crew · from the link you shared 2 h ago"
        );
        assert_eq!(text.ask, "Ask Ben to read their code. It should be:");
        assert_eq!(text.code, "4821 0937 5512");
        assert_eq!(
            text.warning,
            "A different code means someone else opened your link. Don't let them in."
        );
        assert_eq!(text.when_heading, "When you let Ben in");
        assert_eq!(
            text.when_body,
            "Ben sees the Party's name, its members and its Spots, and everyone gets a new key. \
             The link stops working."
        );
        assert_eq!(
            (text.let_in.as_str(), text.turn_away.as_str()),
            ("Codes match · Let Ben in", "Don't let in")
        );
        Ok(())
    }

    #[test]
    fn open_invite_reads_like_the_canvas() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        let text = open_invite(EN, &open_screen(&snapshot));
        assert_eq!(text.title, "You've been invited to a Party");
        assert_eq!(
            text.subtitle,
            "You'll see its name and who's in it once the person who invited you lets you in."
        );
        assert_eq!(text.what_joining_means, "What joining means");
        assert_eq!(
            text.points,
            [
                "Members find out you're nearby only when you're in the same Spot at the same \
                 time.",
                "Your location is end-to-end encrypted. The server can't see where you are or \
                 which Spot you're in.",
            ]
        );
        assert_eq!(
            (text.name_label.as_str(), text.name.as_str()),
            ("Your name in this Party", "Sam")
        );
        assert_eq!(
            (text.ask.as_str(), text.not_now.as_str()),
            ("Ask to join", "Not now")
        );
        snapshot.display_name = None;
        assert_eq!(open_invite(EN, &open_screen(&snapshot)).name, "");
        Ok(())
    }

    #[test]
    fn asked_to_join_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = asked_to_join(EN, &asked_screen(&snapshot).ok_or(FixtureError::Missing)?);
        assert_eq!(text.title, "Asked to join");
        assert_eq!(
            text.subtitle,
            "The person who invited you sees your request with the same code. When they let you \
             in, the Party appears on your home screen."
        );
        assert_eq!(
            text.read_code,
            "Read this code to them, in person or on a call"
        );
        assert_eq!(text.code, "4821 0937 5512");
        assert_eq!(
            text.warning,
            "If their code is different, someone else may have your link. Tell them not to let \
             you in."
        );
        assert_eq!(
            text.leave_note,
            "You can leave this screen. We'll notify you when you're in."
        );
        assert_eq!(
            (text.done.as_str(), text.withdraw.as_str()),
            ("Done", "Withdraw request")
        );
        Ok(())
    }

    #[test]
    fn let_in_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::let_in_snapshot()?;
        let text = let_in(EN, &let_in_screen(&snapshot).ok_or(FixtureError::Missing)?);
        assert_eq!(text.party_initials, "CC");
        assert_eq!(text.let_in_by, "Ana let you in");
        assert_eq!(text.party_name, "Climbing crew");
        assert_eq!(text.avatars, ["AN", "CH", "BE"]);
        assert_eq!(text.more.as_deref(), Some("+2"));
        assert_eq!(text.counts, "6 members · 3 spots");
        assert_eq!(
            text.checked,
            "Code 4821 0937 5512 was checked before Ana let you in. You can see it again in the \
             Party's security details."
        );
        assert_eq!(
            (text.open.as_str(), text.leave.as_str()),
            ("Open Climbing crew", "Leave Party")
        );
        assert_eq!(
            (
                text.notification_title.as_str(),
                text.notification_body.as_str()
            ),
            ("Ana let you in", "Climbing crew")
        );

        // With no more members than avatars there is no "+".
        let mut small = snapshot;
        for party in &mut small.parties {
            party.members.truncate(4);
        }
        let text = let_in(EN, &let_in_screen(&small).ok_or(FixtureError::Missing)?);
        assert_eq!(text.more, None);
        Ok(())
    }
}
