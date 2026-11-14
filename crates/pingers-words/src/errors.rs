// SPDX-License-Identifier: AGPL-3.0-only
//! What the user is told when a screen cannot be shown or a value is refused.

use pingers_screens::ScreenError;

use crate::Language;

/// A screen error, as the user is told it.
#[must_use]
pub fn screen_error(language: Language, error: &ScreenError) -> String {
    let Language::English = language;
    match error {
        ScreenError::UnknownParty => "This party is no longer on this phone.",
        ScreenError::UnknownMeetup => "This meetup has ended.",
        ScreenError::UnknownMember => "That person has left the meetup.",
        ScreenError::NoMeetPoint => "There's no meet point yet.",
        ScreenError::UnknownLocationRequest => "This request has expired.",
        ScreenError::UnknownJoinRequest => "This request is no longer waiting.",
        ScreenError::InvalidName => "Give it a name of up to 80 characters.",
        ScreenError::InvalidHost => "Enter an address like pingers.example.org.",
        ScreenError::InvalidOperator => "This server or relay doesn't say who runs it.",
        ScreenError::InvalidSafetyCode => "That isn't a safety code.",
        ScreenError::InvalidInviteLink => "That isn't an invite link.",
        ScreenError::EmptyWindow => "The start and end times must differ.",
        ScreenError::InvalidTime => "Pick a time between 00:00 and 23:59.",
        ScreenError::UnknownZone => "This zone is no longer on this phone.",
        ScreenError::InvalidUtcOffset => "Your phone's time zone looks wrong.",
        ScreenError::ShapeOpen => "Close the shape to continue",
        ScreenError::Shape(_) => "This area can't be used. Drag a point to fix it.",
        _ => "Something went wrong.",
    }
    .into()
}

/// Why a server could not be used: added by address on New party, joined through an invite, or
/// asking for a registration code (canvas "This server needs a code").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServerRefusal {
    /// The server takes only devices with a code from whoever runs it.
    CodeRequired,
    /// The server refused the code given.
    CodeRefused,
    /// What was typed is not a server address.
    InvalidAddress,
    /// The server did not answer.
    Unreachable,
    /// The server advertises no privacy relay.
    NoRelay,
    /// None of the server's privacy relays answered.
    RelayUnreachable,
    /// The server named different relays directly and through its relay.
    RelaysDiffer,
    /// The server's keys are not the ones being trusted.
    KeyMismatch,
    /// The server was not looked at before being trusted.
    NotLookedAt,
}

/// A server refusal, as the user is told it.
#[must_use]
pub fn server_refusal(language: Language, refusal: ServerRefusal) -> String {
    let Language::English = language;
    match refusal {
        ServerRefusal::CodeRequired => {
            "Only people with a code from whoever runs this server can set up Pingers on it."
        }
        ServerRefusal::CodeRefused => {
            "The server didn't accept this code. Check it for typing mistakes, or ask whoever \
             runs the server for a new one."
        }
        ServerRefusal::InvalidAddress => "Enter an address like pingers.example.org.",
        ServerRefusal::Unreachable => "This server didn't answer. Check the address and try again.",
        ServerRefusal::NoRelay => {
            "This server doesn't name a privacy relay, so Pingers can't hide your internet \
             address from it."
        }
        ServerRefusal::RelayUnreachable => {
            "This server's privacy relay didn't answer. Try again later."
        }
        ServerRefusal::RelaysDiffer => {
            "This server named a different privacy relay when asked through its relay. Pingers \
             won't use it."
        }
        ServerRefusal::KeyMismatch => {
            "This server's keys don't match the ones you were given. Nothing was sent to it."
        }
        ServerRefusal::NotLookedAt => "Look up the server again before trusting it.",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use pingers_geo::GeoError;

    use super::*;

    #[test]
    fn a_refused_code_is_told_apart_from_a_missing_one() {
        let say = |refusal| server_refusal(Language::English, refusal);
        assert!(say(ServerRefusal::CodeRequired).starts_with("Only people with a code"));
        assert!(say(ServerRefusal::CodeRefused).starts_with("The server didn't accept"));
        assert!(say(ServerRefusal::KeyMismatch).contains("keys"));
    }

    #[test]
    fn errors_are_said_plainly() {
        let say = |error| screen_error(Language::English, &error);
        assert_eq!(say(ScreenError::UnknownMeetup), "This meetup has ended.");
        assert_eq!(say(ScreenError::ShapeOpen), "Close the shape to continue");
        assert_eq!(
            say(ScreenError::EmptyWindow),
            "The start and end times must differ."
        );
        assert_eq!(
            say(ScreenError::Shape(GeoError::SelfIntersecting)),
            "This area can't be used. Drag a point to fix it."
        );
        let all = [
            ScreenError::UnknownParty,
            ScreenError::UnknownMember,
            ScreenError::NoMeetPoint,
            ScreenError::UnknownLocationRequest,
            ScreenError::UnknownJoinRequest,
            ScreenError::InvalidName,
            ScreenError::InvalidHost,
            ScreenError::InvalidOperator,
            ScreenError::InvalidSafetyCode,
            ScreenError::InvalidInviteLink,
            ScreenError::EmptyWindow,
            ScreenError::InvalidTime,
            ScreenError::UnknownZone,
            ScreenError::InvalidUtcOffset,
        ]
        .map(say);
        assert!(
            all.iter()
                .all(|message| message.ends_with('.') && message != "Something went wrong.")
        );
    }
}
