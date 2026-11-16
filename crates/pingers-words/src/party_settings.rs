// SPDX-License-Identifier: AGPL-3.0-only
//! A Party's settings, in words (canvas `PartySettings`, `PartySettingsShared`), with Safety
//! codes (`SafetyCode`) and Leave party (`LeaveParty`).

use core::time::Duration;

use pingers_screens::common::{Operator, PrivacyLevel};
use pingers_screens::party::Overlap;
use pingers_screens::party_settings::{
    LeaveParty, PartySettings, SafetyCodes, ServiceRow, SharedOperator,
};
use pingers_screens::snapshot::{KeyTrust, PostQuantum, RelayStatus, SafetyCheck};
use pingers_time::UnixSeconds;

use crate::Language;
use crate::common::{members, plural, privacy_level};
use crate::party::overlap_sentence;

/// A server or relay row, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceRowText {
    /// "Server", "Privacy relay", "Push relay".
    pub title: String,
    /// "relay.quietpath.net · hides your internet address".
    pub detail: String,
    /// The operator chip: "Flat 7", or "Not published".
    pub operator: String,
    /// Whether the chip is orange: the server's operator runs this too.
    pub shared: bool,
}

/// The relay check line, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayCheckText {
    /// "Your server answered through it · 2 min ago".
    pub text: String,
    /// Whether it is good news (green) rather than a problem (orange).
    pub ok: bool,
}

/// "For this Party, Flat 7 could work out **who met whom**. …": the bold part between two.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsequenceText {
    /// "For this Party, Flat 7 could work out ".
    pub before: String,
    /// "who met whom".
    pub bold: String,
    /// ". It still can't see where anyone is, …".
    pub after: String,
    /// "Wake-ups settings", when turning wake-ups off is the fix.
    pub wakeups_link: Option<String>,
}

/// The Party settings screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartySettingsText {
    /// "Party settings".
    pub title: String,
    /// "Climbing crew · only this Party".
    pub subtitle: String,
    /// "Back to Climbing crew", for screen readers.
    pub back: String,
    /// "Invite people".
    pub invite: String,
    /// "6 members".
    pub members: String,
    /// "Who runs this Party's server".
    pub operators_heading: String,
    /// "Full privacy", "Reduced privacy".
    pub privacy: String,
    /// Which operators, as sentences.
    pub privacy_detail: String,
    /// What a shared operator could learn, when privacy is reduced.
    pub consequence: Option<ConsequenceText>,
    /// The server row.
    pub server: ServiceRowText,
    /// How this phone came to trust the server's keys: "Server keys from your invite link".
    pub keys: String,
    /// The privacy relay row.
    pub relay: ServiceRowText,
    /// The relay check, once run.
    pub relay_check: Option<RelayCheckText>,
    /// The push relay row.
    pub push_relay: ServiceRowText,
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

fn operator_chip(language: Language, row: &ServiceRow) -> String {
    let Language::English = language;
    row.operator.as_ref().map_or_else(
        || "Not published".into(),
        |operator| operator.as_str().to_owned(),
    )
}

fn host(row: &ServiceRow) -> String {
    row.host
        .as_ref()
        .map_or_else(String::new, |host| host.as_str().to_owned())
}

fn with_host(row: &ServiceRow, detail: &str) -> String {
    match &row.host {
        Some(host) => format!("{} · {detail}", host.as_str()),
        None => capitalised(detail),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How long ago, for a check: "just now", "2 min ago", "3 h ago".
fn ago(language: Language, age: Duration) -> String {
    let Language::English = language;
    let seconds = age.as_secs();
    match seconds {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", seconds / 60),
        _ => format!("{} h ago", seconds / 3_600),
    }
}

fn consequence(
    language: Language,
    view: &PartySettings,
    operator: &str,
) -> Option<ConsequenceText> {
    let Language::English = language;
    let relay = view.overlaps.contains(&Overlap::ServerRunsRelay);
    let push = view.overlaps.contains(&Overlap::ServerRunsPushRelay);
    let still = ". It still can't see where anyone is, which Spot, or what's said.";
    let wakeups_link = || Some("Wake-ups settings".to_owned());
    match (relay, push) {
        (_, true) => Some(ConsequenceText {
            before: if relay {
                format!("For this Party, {operator} could see your internet address and work out ")
            } else {
                format!("For this Party, {operator} could work out ")
            },
            bold: "who met whom".into(),
            after: format!(
                "{still} Turning off wake-ups stops {}, for every Party on this phone.",
                if relay { "that part" } else { "this" }
            ),
            wakeups_link: wakeups_link(),
        }),
        (true, false) => Some(ConsequenceText {
            before: format!("For this Party, {operator} could see "),
            bold: "your internet address".into(),
            after: still.into(),
            wakeups_link: None,
        }),
        (false, false) if view.overlaps.contains(&Overlap::RelayRunsPushRelay) => {
            Some(ConsequenceText {
                before: "For this Party, the operator of both relays could tie ".into(),
                bold: "your internet address to when your phone is woken".into(),
                after: format!(
                    "{still} Turning off wake-ups stops this, for every Party on this phone."
                ),
                wakeups_link: wakeups_link(),
            })
        }
        (false, false) => None,
    }
}

/// The relay check line, once it ran.
fn relay_check(
    language: Language,
    view: &PartySettings,
    now: UnixSeconds,
) -> Option<RelayCheckText> {
    let Language::English = language;
    let at = view.relay_check.checked_at?;
    let age = ago(
        language,
        Duration::from_secs(now.get().saturating_sub(at.get())),
    );
    Some(match view.relay_check.status {
        RelayStatus::Reachable => RelayCheckText {
            text: format!("Your server answered through it · {age}"),
            ok: true,
        },
        RelayStatus::Unreachable => RelayCheckText {
            text: format!("Your server didn't answer through it · {age}"),
            ok: false,
        },
        RelayStatus::KeyMismatch => RelayCheckText {
            text: format!("Its key doesn't match the one your party agreed on · {age}"),
            ok: false,
        },
    })
}

/// How this phone came to trust a Party's server keys, under its server row.
#[must_use]
pub fn keys(language: Language, trust: KeyTrust) -> String {
    let Language::English = language;
    match trust {
        KeyTrust::BuiltIn => "Server keys built into the app".into(),
        KeyTrust::FromInvite => "Server keys from your invite link".into(),
        KeyTrust::Confirmed => "Server keys you checked and trusted".into(),
    }
}

/// The Party settings screen, in words. `now` dates the relay check.
#[must_use]
pub fn party_settings(
    language: Language,
    view: &PartySettings,
    now: UnixSeconds,
) -> PartySettingsText {
    let Language::English = language;
    let name = view.name.as_str();
    let server_operator = view
        .server
        .operator
        .as_ref()
        .map_or("the server's operator", Operator::as_str);
    PartySettingsText {
        title: "Party settings".into(),
        subtitle: format!("{name} · only this Party"),
        back: format!("Back to {name}"),
        invite: "Invite people".into(),
        members: members(language, view.members),
        operators_heading: "Who runs this Party's server".into(),
        privacy: privacy_level(language, view.privacy),
        privacy_detail: match view.privacy {
            PrivacyLevel::Full => {
                "Server, privacy relay and push relay are run by three different operators.".into()
            }
            PrivacyLevel::Reduced => view
                .overlaps
                .iter()
                .map(|overlap| overlap_sentence(language, *overlap))
                .collect::<Vec<_>>()
                .join(" "),
        },
        consequence: match view.privacy {
            PrivacyLevel::Full => None,
            PrivacyLevel::Reduced => consequence(language, view, server_operator),
        },
        keys: keys(language, view.keys),
        server: ServiceRowText {
            title: "Server".into(),
            detail: host(&view.server),
            operator: operator_chip(language, &view.server),
            shared: view.privacy == PrivacyLevel::Reduced
                && view.overlaps.iter().any(|overlap| {
                    matches!(
                        overlap,
                        Overlap::ServerRunsRelay | Overlap::ServerRunsPushRelay
                    )
                }),
        },
        relay: ServiceRowText {
            title: "Privacy relay".into(),
            detail: match view.relay.shared {
                SharedOperator::No => with_host(&view.relay, "hides your internet address"),
                SharedOperator::WithServer if view.relay.operator.is_some() => {
                    with_host(&view.relay, "same operator as the server")
                }
                SharedOperator::WithServer => with_host(
                    &view.relay,
                    "publishes no operator, so counted as the server's",
                ),
            },
            operator: operator_chip(language, &view.relay),
            shared: view.relay.shared == SharedOperator::WithServer,
        },
        relay_check: relay_check(language, view, now),
        push_relay: ServiceRowText {
            title: "Push relay".into(),
            detail: match view.push_relay.shared {
                SharedOperator::No => with_host(&view.push_relay, "sees when, never why"),
                SharedOperator::WithServer => {
                    with_host(&view.push_relay, "same operator as the server")
                }
            },
            operator: operator_chip(language, &view.push_relay),
            shared: view.push_relay.shared == SharedOperator::WithServer
                && view.overlaps.contains(&Overlap::ServerRunsPushRelay),
        },
        check_again: "Check the privacy relay again".into(),
        operators_note: "Each service publishes who runs it. Pingers shows what they say; it \
                         can't prove they aren't secretly the same people."
            .into(),
        server_sees: "What the server sees".into(),
        safety_codes: "Safety codes".into(),
        safety_summary: if view.safety.others == 0 {
            "No other members yet".into()
        } else {
            format!(
                "Checked with {} of {}",
                view.safety.checked,
                plural(language, view.safety.others, "member", "members")
            )
        },
        security: "Security details".into(),
        security_summary: match view.post_quantum {
            PostQuantum::On => "End-to-end encrypted · post-quantum on".into(),
            PostQuantum::Off => "End-to-end encrypted · post-quantum off".into(),
        },
        leave: "Leave party".into(),
    }
}

/// A member row on Safety codes, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyRowText {
    /// "AN".
    pub avatar: String,
    /// "Ana".
    pub name: String,
    /// "Checked", "Not checked".
    pub state: String,
}

/// The Safety codes screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafetyCodesText {
    /// "Safety codes".
    pub title: String,
    /// "Climbing crew".
    pub subtitle: String,
    /// "Your code with Ana", once the code is computed.
    pub code_heading: Option<String>,
    /// The code's twelve groups of five digits.
    pub code_groups: Vec<String>,
    /// How to compare it.
    pub compare: Option<String>,
    /// "Codes match · Mark Ana as checked", while not checked.
    pub mark: Option<String>,
    /// "Checked", once checked.
    pub checked_note: Option<String>,
    /// "Members".
    pub members_heading: String,
    /// The members.
    pub members: Vec<SafetyRowText>,
    /// What to do when nobody else is in the Party yet.
    pub empty: Option<String>,
    /// That checking is optional.
    pub footnote: String,
}

fn check_state(language: Language, check: SafetyCheck) -> String {
    let Language::English = language;
    match check {
        SafetyCheck::Checked => "Checked".into(),
        SafetyCheck::NotChecked => "Not checked".into(),
    }
}

/// The Safety codes screen, in words.
#[must_use]
pub fn safety_codes(language: Language, view: &SafetyCodes) -> SafetyCodesText {
    let Language::English = language;
    let shown = view.shown.as_ref();
    let shown_name = shown.map(|shown| shown.person.name.as_str().to_owned());
    SafetyCodesText {
        title: "Safety codes".into(),
        subtitle: view.party_name.as_str().to_owned(),
        code_heading: shown_name
            .as_ref()
            .map(|name| format!("Your code with {name}")),
        code_groups: shown.map_or_else(Vec::new, |shown| {
            shown.code.as_str().split(' ').map(str::to_owned).collect()
        }),
        compare: shown_name.as_ref().map(|name| {
            format!(
                "Compare with {name} in person or on a call. If every digit matches, nobody is \
                 passing messages between you while pretending to be {name}."
            )
        }),
        mark: shown
            .filter(|shown| shown.checked == SafetyCheck::NotChecked)
            .map(|shown| {
                format!(
                    "Codes match · Mark {} as checked",
                    shown.person.name.as_str()
                )
            }),
        checked_note: shown
            .filter(|shown| shown.checked == SafetyCheck::Checked)
            .map(|shown| format!("{} is checked", shown.person.name.as_str())),
        members_heading: "Members".into(),
        empty: view.members.is_empty().then(|| {
            "Nobody else is in this Party yet. Once people join, compare codes with each of them \
             here."
                .into()
        }),
        members: view
            .members
            .iter()
            .map(|row| SafetyRowText {
                avatar: crate::common::avatar(language, &row.person),
                name: row.person.name.as_str().to_owned(),
                state: check_state(language, row.checked),
            })
            .collect(),
        footnote: "Checking is optional. A code changes only if that member sets up Pingers \
                   again from scratch."
            .into(),
    }
}

/// The Leave party sheet, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeavePartyText {
    /// "Leave Climbing crew?".
    pub title: String,
    /// What happens, one point each.
    pub points: Vec<String>,
    /// The running Meetup that leaving ends.
    pub meetup_warning: Option<String>,
    /// "Leave party".
    pub leave: String,
    /// "Cancel".
    pub cancel: String,
}

/// The Leave party sheet, in words.
#[must_use]
pub fn leave_party(language: Language, view: &LeaveParty) -> LeavePartyText {
    let Language::English = language;
    let spots = match view.spots {
        0 => "Members stop noticing you at its Spots, and you stop noticing them.".to_owned(),
        count => format!(
            "Members stop noticing you at its {}, and you stop noticing them.",
            plural(language, count, "Spot", "Spots")
        ),
    };
    LeavePartyText {
        title: format!("Leave {}?", view.name.as_str()),
        points: vec![
            spots,
            "Members see that you left. They get a new key, so you can't read anything sent \
             after you go."
                .into(),
            "To come back, someone in the Party has to invite you again.".into(),
        ],
        meetup_warning: view.meetup_at.as_ref().map(|spot| {
            format!(
                "You're in a Meetup at {} with this Party. Leaving ends it for you too.",
                spot.as_str()
            )
        }),
        leave: "Leave party".into(),
        cancel: "Cancel".into(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_screens::fixtures::{self, FixtureError};
    use pingers_screens::party_settings::{
        first_to_check, leave_party as leave_screen, party_settings as screen,
        safety_codes as codes_screen,
    };
    use pingers_screens::snapshot::SafetyCode;

    use super::*;

    const EN: Language = Language::English;
    const NOW: UnixSeconds = UnixSeconds::new(10_000);

    #[test]
    fn each_way_of_trusting_server_keys_has_its_own_words() {
        let mut seen = vec![
            keys(EN, KeyTrust::BuiltIn),
            keys(EN, KeyTrust::FromInvite),
            keys(EN, KeyTrust::Confirmed),
        ];
        assert_eq!(
            seen.first().map(String::as_str),
            Some("Server keys built into the app")
        );
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn three_operators_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = screen(
            &snapshot,
            fixtures::climbing_crew(),
            Some(UnixSeconds::new(9_880)),
        )?;
        let text = party_settings(EN, &view, NOW);
        assert_eq!(text.title, "Party settings");
        assert_eq!(text.subtitle, "Climbing crew · only this Party");
        assert_eq!(text.members, "6 members");
        assert_eq!(text.privacy, "Full privacy");
        assert_eq!(
            text.privacy_detail,
            "Server, privacy relay and push relay are run by three different operators."
        );
        assert_eq!(text.consequence, None);
        assert_eq!(
            (text.server.detail.as_str(), text.server.operator.as_str()),
            ("pingers.flat7.org", "Flat 7")
        );
        assert_eq!(
            text.relay.detail,
            "relay.quietpath.net · hides your internet address"
        );
        assert_eq!(text.relay.operator, "Quietpath");
        assert_eq!(
            text.relay_check,
            Some(RelayCheckText {
                text: "Your server answered through it · 2 min ago".into(),
                ok: true
            })
        );
        assert_eq!(
            text.push_relay.detail,
            "push.pingers.org · sees when, never why"
        );
        assert_eq!(text.push_relay.operator, "Pingers");
        assert!(!text.push_relay.shared && !text.server.shared);
        assert_eq!(text.safety_summary, "Checked with 2 of 5 members");
        assert_eq!(
            text.security_summary,
            "End-to-end encrypted · post-quantum off"
        );
        assert_eq!(text.leave, "Leave party");
        Ok(())
    }

    #[test]
    fn the_same_operator_running_the_push_relay_reads_like_the_canvas() -> Result<(), FixtureError>
    {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.config.push_relay.operator = Some(Operator::new("Flat 7")?);
            party.config.push_relay.host =
                Some(pingers_screens::common::Host::new("push.flat7.org")?);
        }
        let view = screen(&snapshot, fixtures::climbing_crew(), None)?;
        let text = party_settings(EN, &view, NOW);
        assert_eq!(text.privacy, "Reduced privacy");
        assert_eq!(
            text.privacy_detail,
            "The server's operator also runs the push relay, so it could see which phone each \
             alert wakes."
        );
        let consequence = text.consequence.ok_or(FixtureError::Missing)?;
        assert_eq!(consequence.before, "For this Party, Flat 7 could work out ");
        assert_eq!(consequence.bold, "who met whom");
        assert_eq!(
            consequence.after,
            ". It still can't see where anyone is, which Spot, or what's said. Turning off \
             wake-ups stops this, for every Party on this phone."
        );
        assert_eq!(
            consequence.wakeups_link.as_deref(),
            Some("Wake-ups settings")
        );
        assert_eq!(
            text.push_relay.detail,
            "push.flat7.org · same operator as the server"
        );
        assert!(text.push_relay.shared && text.server.shared && !text.relay.shared);
        assert_eq!(text.relay_check, None);
        Ok(())
    }

    #[test]
    fn a_relay_without_an_operator_is_counted_as_the_servers() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party.config.relay.operator = None;
            party.config.push_relay.operator = Some(Operator::new("Flat 7")?);
        }
        let view = screen(&snapshot, fixtures::climbing_crew(), None)?;
        let text = party_settings(EN, &view, NOW);
        assert_eq!(text.relay.operator, "Not published");
        assert_eq!(
            text.relay.detail,
            "relay.quietpath.net · publishes no operator, so counted as the server's"
        );
        let consequence = text.consequence.ok_or(FixtureError::Missing)?;
        assert_eq!(
            consequence.before,
            "For this Party, Flat 7 could see your internet address and work out "
        );
        assert!(
            consequence
                .after
                .ends_with("stops that part, for every Party on this phone.")
        );
        Ok(())
    }

    #[test]
    fn safety_codes_read_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let party = fixtures::climbing_crew();
        let first = first_to_check(&snapshot, party)?.ok_or(FixtureError::Missing)?;
        let code = SafetyCode::new(
            "48210 93755 12064 38817 70459 21386 66102 94573 15028 83941 57260 30694",
        )?;
        let text = safety_codes(EN, &codes_screen(&snapshot, party, Some((first, code)))?);
        assert_eq!(text.title, "Safety codes");
        assert_eq!(text.subtitle, "Climbing crew");
        assert_eq!(text.code_heading.as_deref(), Some("Your code with Ana"));
        assert_eq!(text.code_groups.len(), 12);
        assert_eq!(text.empty, None);
        assert_eq!(text.code_groups.first().map(String::as_str), Some("48210"));
        assert_eq!(
            text.mark.as_deref(),
            Some("Codes match · Mark Ana as checked")
        );
        assert_eq!(text.checked_note, None);
        let states: Vec<_> = text
            .members
            .iter()
            .map(|row| (row.name.as_str(), row.state.as_str()))
            .collect();
        assert_eq!(
            states,
            [
                ("Ana", "Not checked"),
                ("Chloé", "Checked"),
                ("Ben", "Checked"),
                ("Dev", "Not checked"),
                ("Eli", "Not checked")
            ]
        );
        Ok(())
    }

    #[test]
    fn safety_codes_with_nobody_else_say_what_to_do() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        for party in &mut snapshot.parties {
            party
                .members
                .retain(|member| member.me == pingers_screens::common::Me::Me);
        }
        let text = safety_codes(
            EN,
            &codes_screen(&snapshot, fixtures::climbing_crew(), None)?,
        );
        assert!(text.members.is_empty());
        assert_eq!(
            text.empty.as_deref(),
            Some(
                "Nobody else is in this Party yet. Once people join, compare codes with each of \
                 them here."
            )
        );
        Ok(())
    }

    #[test]
    fn leaving_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let text = leave_party(EN, &leave_screen(&snapshot, fixtures::climbing_crew())?);
        assert_eq!(text.title, "Leave Climbing crew?");
        assert_eq!(
            text.points.first().map(String::as_str),
            Some("Members stop noticing you at its 3 Spots, and you stop noticing them.")
        );
        assert_eq!(
            text.meetup_warning.as_deref(),
            Some(
                "You're in a Meetup at Riverside Park with this Party. Leaving ends it for you too."
            )
        );
        assert_eq!(
            (text.leave.as_str(), text.cancel.as_str()),
            ("Leave party", "Cancel")
        );
        Ok(())
    }
}
