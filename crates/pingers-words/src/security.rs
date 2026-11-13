// SPDX-License-Identifier: AGPL-3.0-only
//! Security details: "How Pingers keeps you private".

use pingers_screens::security::SecurityDetails;
use pingers_screens::server::Proofs;
use pingers_screens::snapshot::WakePushes;

use crate::Language;

/// The screen's title, also the link to it from other screens.
pub const TITLE: &str = "How Pingers keeps you private";

/// A titled section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// Its heading.
    pub title: String,
    /// Its paragraph.
    pub body: String,
}

/// A limit: a bold lead and the rest of the sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limit {
    /// "Party members you trust."
    pub lead: String,
    /// The rest.
    pub rest: String,
}

/// A row of "Under the hood": what, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mechanism {
    /// "Party and meetup encryption".
    pub what: String,
    /// "MLS · RFC 9420".
    pub how: String,
}

/// The Security details screen, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityText {
    /// The title.
    pub title: String,
    /// The introduction.
    pub intro: String,
    /// The sections.
    pub sections: Vec<Section>,
    /// "What Pingers can't protect".
    pub limits_heading: String,
    /// The limits.
    pub limits: Vec<Limit>,
    /// "Under the hood".
    pub hood_heading: String,
    /// The mechanisms.
    pub hood: Vec<Mechanism>,
    /// "Read the design document".
    pub read_document: String,
}

fn section(title: &str, body: &str) -> Section {
    Section {
        title: title.into(),
        body: body.into(),
    }
}

fn limit(lead: &str, rest: &str) -> Limit {
    Limit {
        lead: lead.into(),
        rest: rest.into(),
    }
}

fn mechanism(what: &str, how: &str) -> Mechanism {
    Mechanism {
        what: what.into(),
        how: how.into(),
    }
}

fn sections(details: SecurityDetails) -> Vec<Section> {
    let waking = match details.wake_pushes {
        WakePushes::On => {
            "The alert goes through a separate push relay. The server knows an alert was sent but \
             not to which phone. The relay knows a phone was woken but not why or by whom. Turn \
             on \"No wake-up pushes\" and nobody sees it at all."
        }
        WakePushes::NoWakePushes => {
            "\"No wake-up pushes\" is on, so nobody wakes your phone: it checks for friends on its \
             own wakes, and nobody, not even Apple or Google, sees when. Meetups can take up to an \
             hour to start."
        }
    };
    let account = match details.proofs {
        Proofs::MembershipProofs => {
            "Your account is a key made on your phone. To stop spam without knowing who you are, \
             your phone proves \"I'm a real member\" mathematically, without saying which member. \
             Nothing is kept as a location history."
        }
        Proofs::PrivacyPassOnly => {
            "Your account is a key made on your phone. Membership proofs aren't available on this \
             phone yet, so it uses anonymous passes instead: the server sees when your account \
             collects a day's batch, but not when or where they're spent. Nothing is kept as a \
             location history."
        }
    };
    vec![
        section(
            "Your Party is locked to its members",
            "Party names, members and Spots are encrypted on your phone. Only members' phones \
             hold the keys. The safety code you compare when joining proves nobody slipped in \
             between you.",
        ),
        section(
            "Finding friends without telling the server",
            "Your phone posts anonymous check-ins that only your Party can recognise, and they \
             look different every time. Your phone, not the server, spots a friend in the \
             same Spot, and only for the Spot you're in.",
        ),
        section("Waking a friend's phone", waking),
        section(
            "Hiding your internet address",
            "Everything anonymous, including live locations in a meetup, travels through a \
             privacy relay run by someone else. The relay sees your address but not what you \
             send; the server sees what arrives but not from where.",
        ),
        section(
            "Meetups are just for who was there",
            "Each meetup gets brand-new keys shared only by the people in it. Party members \
             who weren't there can't read it, people who join later don't see earlier \
             messages, and when everyone leaves, every phone deletes the chat and keys.",
        ),
        section(
            "Your exact spot is yours to share",
            "In a meetup, others see you're there, not where. Your position is locked with \
             your own key, which you hand only to people you approve. Others can't even tell \
             whether you've shared with anyone.",
        ),
        section(
            "Pauses that look like nothing",
            "Focus, Do Not Disturb and your exclusion zones are checked on your phone and \
             never leave it. While paused, your phone keeps sending blank check-ins, so being \
             paused looks exactly like being somewhere else.",
        ),
        section("No phone number, no tracking ID", account),
    ]
}

fn limits(details: SecurityDetails) -> Vec<Limit> {
    let apple_and_google = match details.wake_pushes {
        WakePushes::On => {
            "They see that a notification arrived, not what it says. \"No wake-up pushes\" stops \
             this."
        }
        WakePushes::NoWakePushes => {
            "With \"No wake-up pushes\" on, they don't see Pingers notifications arrive at all."
        }
    };
    vec![
        limit(
            "Party members you trust.",
            "Any member could claim to be at a Spot, or use a modified app to count how many \
             people are at one. Only invite people you'd meet anyway.",
        ),
        limit(
            "People you share with.",
            "Someone you approve can screenshot where you were. Stopping sharing locks them \
             out from then on.",
        ),
        limit(
            "Operators working together.",
            "If the server and the push relay pooled their logs, they could work out who met \
             whom, but still not where.",
        ),
        limit("Apple and Google.", apple_and_google),
        limit(
            "An unlocked, stolen phone.",
            "It reveals your Parties, Spots and exclusion zones. Ask a member to remove the \
             lost phone.",
        ),
    ]
}

/// The Security details screen, in words.
#[must_use]
pub fn security(language: Language, details: SecurityDetails) -> SecurityText {
    let Language::English = language;
    SecurityText {
        title: TITLE.into(),
        intro: "Pingers is built so that the people running it can't see where you are, who you \
                meet, or what you say. Here's how, and where the limits are."
            .into(),
        sections: sections(details),
        limits_heading: "What Pingers can't protect".into(),
        limits: limits(details),
        hood_heading: "Under the hood".into(),
        hood: vec![
            mechanism("Party and meetup encryption", "MLS · RFC 9420"),
            mechanism("Privacy relay", "Oblivious HTTP · RFC 9458, chunked"),
            mechanism("Sealed push tokens, location keys", "HPKE · RFC 9180"),
            mechanism("Anonymous membership proofs", "RLN zero-knowledge proofs"),
            mechanism("Fallback rate limiting", "Privacy Pass · RFC 9578"),
            mechanism("Post-quantum option", "X-Wing hybrid"),
        ],
        read_document: "Read the design document".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: Language = Language::English;

    fn details(wake_pushes: WakePushes, proofs: Proofs) -> SecurityDetails {
        SecurityDetails {
            wake_pushes,
            proofs,
        }
    }

    fn body<'a>(text: &'a SecurityText, title: &str) -> &'a str {
        text.sections
            .iter()
            .find(|section| section.title == title)
            .map_or("", |section| section.body.as_str())
    }

    #[test]
    fn the_default_reads_like_the_canvas() {
        let text = security(EN, details(WakePushes::On, Proofs::MembershipProofs));
        assert_eq!(text.title, "How Pingers keeps you private");
        assert_eq!(text.sections.len(), 8);
        assert!(
            body(&text, "Waking a friend's phone")
                .ends_with("Turn on \"No wake-up pushes\" and nobody sees it at all.")
        );
        assert!(
            body(&text, "No phone number, no tracking ID").contains("without saying which member")
        );
        assert_eq!(text.limits.len(), 5);
        assert_eq!(
            text.limits.get(3).map(|limit| limit.rest.as_str()),
            Some(
                "They see that a notification arrived, not what it says. \"No wake-up pushes\" stops this."
            )
        );
        assert_eq!(
            text.hood.first().map(|row| row.how.as_str()),
            Some("MLS · RFC 9420")
        );
        assert_eq!(text.hood.len(), 6);
        assert_eq!(text.limits_heading, "What Pingers can't protect");
        assert_eq!(text.hood_heading, "Under the hood");
        assert_eq!(text.read_document, "Read the design document");
        assert!(text.intro.starts_with("Pingers is built so"));
    }

    #[test]
    fn no_wake_up_pushes_is_described_as_on() {
        let text = security(
            EN,
            details(WakePushes::NoWakePushes, Proofs::MembershipProofs),
        );
        assert!(body(&text, "Waking a friend's phone").starts_with("\"No wake-up pushes\" is on"));
        assert!(
            text.limits
                .get(3)
                .is_some_and(|limit| limit.rest.starts_with("With \"No wake-up pushes\" on"))
        );
    }

    #[test]
    fn the_privacy_pass_fallback_is_described() {
        let text = security(EN, details(WakePushes::On, Proofs::PrivacyPassOnly));
        assert!(
            body(&text, "No phone number, no tracking ID").contains("anonymous passes instead")
        );
    }
}
