// SPDX-License-Identifier: AGPL-3.0-only
//! The per-Party screens of the design canvas's section 4, as UniFFI records with their words
//! from pingers-words: Party settings, Safety codes, Leave party, and "This server needs a
//! code" for New party.

use pingers_screens as screens;
use pingers_time::UnixSeconds;
use pingers_words::Language;

use crate::CoreError;
use crate::ids::{MemberId, PartyId};
use crate::snapshot::Snapshot;
use crate::views::{PartyDraft, PrivacyLevel};

const LANGUAGE: Language = Language::English;

/// A server or relay row on Party settings.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ServiceRowView {
    /// "Server", "Privacy relay", "Push relay".
    pub title: String,
    /// "relay.quietpath.net · hides your internet address".
    pub detail: String,
    /// The operator chip: "Flat 7", or "Not published".
    pub operator: String,
    /// Whether the chip is orange: the server's operator runs this too.
    pub shared: bool,
}

impl From<pingers_words::party_settings::ServiceRowText> for ServiceRowView {
    fn from(text: pingers_words::party_settings::ServiceRowText) -> Self {
        Self {
            title: text.title,
            detail: text.detail,
            operator: text.operator,
            shared: text.shared,
        }
    }
}

/// Party settings (canvas `PartySettings`, `PartySettingsShared`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PartySettingsView {
    /// The Party.
    pub party: PartyId,
    /// Full or reduced.
    pub privacy: PrivacyLevel,
    /// "Party settings".
    pub title: String,
    /// "Climbing crew · only this Party".
    pub subtitle: String,
    /// "Back to Climbing crew".
    pub back: String,
    /// "Invite people".
    pub invite: String,
    /// "6 members".
    pub members: String,
    /// "Who runs this Party's server".
    pub operators_heading: String,
    /// "Full privacy", "Reduced privacy".
    pub privacy_title: String,
    /// Which operators.
    pub privacy_detail: String,
    /// "For this Party, Flat 7 could work out ", when reduced.
    pub consequence_before: Option<String>,
    /// "who met whom", bold.
    pub consequence_bold: Option<String>,
    /// ". It still can't see …".
    pub consequence_after: Option<String>,
    /// "Wake-ups settings", when turning wake-ups off is the fix.
    pub wakeups_link: Option<String>,
    /// The server row.
    pub server: ServiceRowView,
    /// The privacy relay row.
    pub relay: ServiceRowView,
    /// "Your server answered through it · 2 min ago", once checked.
    pub relay_check: Option<String>,
    /// Whether the check is good news.
    pub relay_check_ok: bool,
    /// The push relay row.
    pub push_relay: ServiceRowView,
    /// "Check the privacy relay again".
    pub check_again: String,
    /// That operators are what services publish.
    pub operators_note: String,
    /// "What the server sees".
    pub server_sees: String,
    /// "Safety codes".
    pub safety_codes: String,
    /// "Checked with 2 of 5 members".
    pub safety_summary: String,
    /// "Security details".
    pub security: String,
    /// "End-to-end encrypted · post-quantum off".
    pub security_summary: String,
    /// "Leave party".
    pub leave: String,
}

/// A member row on Safety codes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SafetyRowView {
    /// Who.
    pub member: MemberId,
    /// "AN".
    pub avatar: String,
    /// Their avatar tint.
    pub tint: crate::views::Tint,
    /// "Ana".
    pub name: String,
    /// "Checked", "Not checked".
    pub state: String,
    /// Whether they are checked.
    pub checked: bool,
    /// Whether their code is the one shown.
    pub selected: bool,
}

/// Safety codes (canvas `SafetyCode`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SafetyCodesView {
    /// Whose code is shown.
    pub shown: Option<MemberId>,
    /// "Safety codes".
    pub title: String,
    /// "Climbing crew".
    pub subtitle: String,
    /// "Your code with Ana".
    pub code_heading: Option<String>,
    /// The code's twelve groups of five digits.
    pub code_groups: Vec<String>,
    /// How to compare it.
    pub compare: Option<String>,
    /// "Codes match · Mark Ana as checked", while not checked.
    pub mark: Option<String>,
    /// "Ana is checked", once checked.
    pub checked_note: Option<String>,
    /// "Members".
    pub members_heading: String,
    /// The members.
    pub members: Vec<SafetyRowView>,
    /// That checking is optional.
    pub footnote: String,
}

/// The Leave party sheet (canvas `LeaveParty`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LeavePartyView {
    /// The Party.
    pub party: PartyId,
    /// "Leave Climbing crew?".
    pub title: String,
    /// What happens.
    pub points: Vec<String>,
    /// The running Meetup that leaving ends.
    pub meetup_warning: Option<String>,
    /// "Leave party".
    pub leave: String,
    /// "Cancel".
    pub cancel: String,
}

/// What the server said to the code last tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum CodeAnswer {
    /// None tried yet.
    NotTried,
    /// Refused.
    Refused,
}

/// "This server needs a code" (canvas `PartyCode`, `PartyCodeRefused`).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PartyCodeView {
    /// "Back to New party".
    pub back: String,
    /// "This server needs a code".
    pub title: String,
    /// "You chose pingers.flat7.org for Climbing crew. …"
    pub body: String,
    /// "pingers.flat7.org".
    pub server: String,
    /// "Run by Flat 7".
    pub operator: String,
    /// "Registration code".
    pub code_label: String,
    /// What the server does with it.
    pub code_help: String,
    /// Why it was refused.
    pub refused: Option<String>,
    /// "No Party was created. …"
    pub nothing_created: Option<String>,
    /// "People you invite won't need a code: their invite link carries it."
    pub invitees: String,
    /// "Create party", or "Try again".
    pub create: String,
    /// Whether the button is enabled.
    pub can_create: bool,
}

#[uniffi::export]
impl Snapshot {
    /// Party settings. `relay_checked_at` is when the app last ran the relay check (Unix
    /// seconds); `now` dates it.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn party_settings(
        &self,
        party: &PartyId,
        relay_checked_at: Option<u64>,
        now: u64,
    ) -> Result<PartySettingsView, CoreError> {
        let view = screens::party_settings::party_settings(
            self.client(),
            party.try_into()?,
            relay_checked_at.map(UnixSeconds::new),
        )?;
        let text =
            pingers_words::party_settings::party_settings(LANGUAGE, &view, UnixSeconds::new(now));
        let consequence = text.consequence;
        Ok(PartySettingsView {
            party: view.party.into(),
            privacy: view.privacy.into(),
            title: text.title,
            subtitle: text.subtitle,
            back: text.back,
            invite: text.invite,
            members: text.members,
            operators_heading: text.operators_heading,
            privacy_title: text.privacy,
            privacy_detail: text.privacy_detail,
            consequence_before: consequence.as_ref().map(|it| it.before.clone()),
            consequence_bold: consequence.as_ref().map(|it| it.bold.clone()),
            consequence_after: consequence.as_ref().map(|it| it.after.clone()),
            wakeups_link: consequence.and_then(|it| it.wakeups_link),
            server: text.server.into(),
            relay: text.relay.into(),
            relay_check_ok: text.relay_check.as_ref().is_some_and(|check| check.ok),
            relay_check: text.relay_check.map(|check| check.text),
            push_relay: text.push_relay.into(),
            check_again: text.check_again,
            operators_note: text.operators_note,
            server_sees: text.server_sees,
            safety_codes: text.safety_codes,
            safety_summary: text.safety_summary,
            security: text.security,
            security_summary: text.security_summary,
            leave: text.leave,
        })
    }

    /// Whose code Safety codes opens on: the first member not checked, else the first.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn first_to_check(&self, party: &PartyId) -> Result<Option<MemberId>, CoreError> {
        Ok(
            screens::party_settings::first_to_check(self.client(), party.try_into()?)?
                .map(Into::into),
        )
    }

    /// Safety codes, showing `code` (from `Core::safety_code`) for `member`.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] for a Party or member that is gone; [`CoreError::Invalid`] for
    /// a code that is not groups of digits.
    pub fn safety_codes(
        &self,
        party: &PartyId,
        member: Option<MemberId>,
        code: Option<String>,
    ) -> Result<SafetyCodesView, CoreError> {
        let selected = match (member, code) {
            (Some(member), Some(code)) => {
                Some((member.into(), screens::snapshot::SafetyCode::new(&code)?))
            }
            _ => None,
        };
        let view =
            screens::party_settings::safety_codes(self.client(), party.try_into()?, selected)?;
        let text = pingers_words::party_settings::safety_codes(LANGUAGE, &view);
        Ok(SafetyCodesView {
            shown: view.shown.as_ref().map(|shown| shown.person.leaf.into()),
            title: text.title,
            subtitle: text.subtitle,
            code_heading: text.code_heading,
            code_groups: text.code_groups,
            compare: text.compare,
            mark: text.mark,
            checked_note: text.checked_note,
            members_heading: text.members_heading,
            members: view
                .members
                .iter()
                .zip(text.members)
                .map(|(row, words)| SafetyRowView {
                    member: row.person.leaf.into(),
                    avatar: words.avatar,
                    tint: row.person.tint.into(),
                    name: words.name,
                    state: words.state,
                    checked: row.checked == screens::snapshot::SafetyCheck::Checked,
                    selected: row.selected,
                })
                .collect(),
            footnote: text.footnote,
        })
    }

    /// The Leave party sheet.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn leave_party(&self, party: &PartyId) -> Result<LeavePartyView, CoreError> {
        let view = screens::party_settings::leave_party(self.client(), party.try_into()?)?;
        let text = pingers_words::party_settings::leave_party(LANGUAGE, &view);
        Ok(LeavePartyView {
            party: view.party.into(),
            title: text.title,
            points: text.points,
            meetup_warning: text.meetup_warning,
            leave: text.leave,
            cancel: text.cancel,
        })
    }

    /// "This server needs a code" for a draft, with `code` typed so far.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for a draft without a name, or a server that is not a host.
    pub fn party_code(
        &self,
        draft: &PartyDraft,
        code: String,
        answer: CodeAnswer,
    ) -> Result<PartyCodeView, CoreError> {
        let view = screens::party::party_code(
            &draft.try_into()?,
            &screens::common::DraftText::new(code),
            match answer {
                CodeAnswer::NotTried => screens::party::CodeAnswer::NotTried,
                CodeAnswer::Refused => screens::party::CodeAnswer::Refused,
            },
        )?;
        let text = pingers_words::party::party_code(LANGUAGE, &view);
        Ok(PartyCodeView {
            back: text.back,
            title: text.title,
            body: text.body,
            server: text.server,
            operator: text.operator,
            code_label: text.code_label,
            code_help: text.code_help,
            refused: text.refused,
            nothing_created: text.nothing_created,
            invitees: text.invitees,
            create: text.create,
            can_create: view.can_create,
        })
    }
}

#[cfg(test)]
mod tests;
