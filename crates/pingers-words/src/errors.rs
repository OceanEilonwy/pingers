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

#[cfg(test)]
mod tests {
    use pingers_geo::GeoError;

    use super::*;

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
