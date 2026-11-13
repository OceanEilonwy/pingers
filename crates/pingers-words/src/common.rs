// SPDX-License-Identifier: AGPL-3.0-only
//! Words several screens share: lists, plurals, distances, areas, times, ages, directions,
//! colours.

use core::time::Duration;

use pingers_geo::{Meters, SquareMeters};
use pingers_screens::clock::LocalTime;
use pingers_screens::common::{Direction, Me, PartyColour, Person, PrivacyLevel};

use crate::Language;

/// "Done".
#[must_use]
pub fn done(language: Language) -> String {
    let Language::English = language;
    "Done".into()
}

/// A count with its noun in the right plural form: "1 member", "6 members".
#[must_use]
pub fn plural(language: Language, count: usize, one: &str, other: &str) -> String {
    let Language::English = language;
    if count == 1 {
        format!("{count} {one}")
    } else {
        format!("{count} {other}")
    }
}

/// Names joined as a sentence lists them: "Ben", "Ben and Chloé", "Ana, Ben and Chloé".
#[must_use]
pub fn list(language: Language, items: &[String]) -> String {
    let Language::English = language;
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// A person in the middle of a sentence: their name, or "you".
#[must_use]
pub fn person_in_sentence(language: Language, person: &Person) -> String {
    let Language::English = language;
    match person.me {
        Me::Me => "you".into(),
        Me::Other => person.name.as_str().to_owned(),
    }
}

/// A person at the start of a line: their name, or "You".
#[must_use]
pub fn person_on_its_own(language: Language, person: &Person) -> String {
    let Language::English = language;
    match person.me {
        Me::Me => "You".into(),
        Me::Other => person.name.as_str().to_owned(),
    }
}

/// An avatar's letters: the person's initials, or "You".
#[must_use]
pub fn avatar(language: Language, person: &Person) -> String {
    let Language::English = language;
    match person.me {
        Me::Me => "You".into(),
        Me::Other => person.initials.as_str().to_owned(),
    }
}

/// A distance: "85 m" to the metre, "1.2 km" from a kilometre.
#[must_use]
pub fn distance(language: Language, distance: Meters) -> String {
    let Language::English = language;
    let metres = distance.get();
    if metres.round() < 1000.0 {
        format!("{metres:.0} m")
    } else {
        format!("{:.1} km", metres / 1000.0)
    }
}

/// How fresh a shared position is: "Updated just now" under 10 s, "Updated 40 s ago" under a
/// minute, then whole minutes ("Updated 3 min ago") and whole hours ("Updated 2 h ago").
/// Counts are rounded down, so "1 min" means 60 to 119 seconds.
#[must_use]
pub fn updated(language: Language, age: Duration) -> String {
    let Language::English = language;
    let seconds = age.as_secs();
    match seconds {
        0..10 => "Updated just now".into(),
        10..60 => format!("Updated {seconds} s ago"),
        60..3600 => format!("Updated {} min ago", seconds / 60),
        _ => format!("Updated {} h ago", seconds / 3600),
    }
}

/// An area: "850 m²" below a tenth of a hectare, otherwise "0.4 ha".
#[must_use]
pub fn area(language: Language, area: SquareMeters) -> String {
    let Language::English = language;
    let square_metres = area.get();
    if square_metres < 1000.0 {
        format!("{square_metres:.0} m²")
    } else {
        format!("{:.1} ha", square_metres / 10_000.0)
    }
}

/// A time of day, 24-hour as the canvas writes it: "18:40", "07:00".
#[must_use]
pub fn time(language: Language, time: LocalTime) -> String {
    let Language::English = language;
    format!("{:02}:{:02}", time.hour(), time.minute())
}

/// A range of times: "23:00–07:00".
#[must_use]
pub fn time_range(language: Language, from: LocalTime, to: LocalTime) -> String {
    format!("{}–{}", time(language, from), time(language, to))
}

/// A compass point in a sentence: "north-east".
#[must_use]
pub fn direction(language: Language, direction: Direction) -> String {
    let Language::English = language;
    match direction {
        Direction::N => "north",
        Direction::NE => "north-east",
        Direction::E => "east",
        Direction::SE => "south-east",
        Direction::S => "south",
        Direction::SW => "south-west",
        Direction::W => "west",
        Direction::NW => "north-west",
    }
    .into()
}

/// A Party colour's name, for its swatch's label.
#[must_use]
pub fn colour(language: Language, colour: PartyColour) -> String {
    let Language::English = language;
    match colour {
        PartyColour::Blue => "Blue",
        PartyColour::Orange => "Orange",
        PartyColour::Ink => "Ink",
        PartyColour::Teal => "Teal",
        PartyColour::Plum => "Plum",
    }
    .into()
}

/// "Full privacy", "Reduced privacy".
#[must_use]
pub fn privacy_level(language: Language, level: PrivacyLevel) -> String {
    let Language::English = language;
    match level {
        PrivacyLevel::Full => "Full privacy",
        PrivacyLevel::Reduced => "Reduced privacy",
    }
    .into()
}

/// "1 member", "6 members".
#[must_use]
pub fn members(language: Language, count: usize) -> String {
    plural(language, count, "member", "members")
}

/// "1 spot", "3 spots".
#[must_use]
pub fn spots(language: Language, count: usize) -> String {
    plural(language, count, "spot", "spots")
}

/// "Back to Climbing crew", for a back button's spoken label.
#[must_use]
pub fn back_to(language: Language, place: &str) -> String {
    let Language::English = language;
    format!("Back to {place}")
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_gate::MinuteOfDay;
    use pingers_proto::LeafIndex;
    use pingers_screens::common::Name;

    use super::*;

    const EN: Language = Language::English;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn lists_read_as_sentences() {
        assert_eq!(list(EN, &[]), "");
        assert_eq!(list(EN, &strings(&["Ben"])), "Ben");
        assert_eq!(list(EN, &strings(&["Ben", "Chloé"])), "Ben and Chloé");
        assert_eq!(
            list(EN, &strings(&["Ana", "Ben", "Chloé"])),
            "Ana, Ben and Chloé"
        );
    }

    #[test]
    fn plurals_follow_the_count() {
        assert_eq!(members(EN, 1), "1 member");
        assert_eq!(members(EN, 0), "0 members");
        assert_eq!(spots(EN, 3), "3 spots");
        assert_eq!(spots(EN, 1), "1 spot");
    }

    #[test]
    fn ages_roll_over_from_seconds_to_minutes_to_hours() {
        let say = |seconds| updated(EN, Duration::from_secs(seconds));
        assert_eq!(say(0), "Updated just now");
        assert_eq!(say(9), "Updated just now");
        assert_eq!(say(10), "Updated 10 s ago");
        assert_eq!(say(40), "Updated 40 s ago");
        assert_eq!(say(59), "Updated 59 s ago");
        assert_eq!(say(60), "Updated 1 min ago");
        assert_eq!(say(119), "Updated 1 min ago");
        assert_eq!(say(120), "Updated 2 min ago");
        assert_eq!(say(3599), "Updated 59 min ago");
        assert_eq!(say(3600), "Updated 1 h ago");
        assert_eq!(say(7300), "Updated 2 h ago");
    }

    #[test]
    fn distances_switch_to_kilometres_at_a_thousand_metres() {
        let say = |metres| distance(EN, Meters::from_whole(metres));
        assert_eq!(say(85), "85 m");
        assert_eq!(say(999), "999 m");
        assert_eq!(say(1000), "1.0 km");
        assert_eq!(say(1250), "1.2 km");
    }

    #[test]
    fn areas_switch_to_hectares_at_a_tenth() -> Result<(), pingers_geo::GeoError> {
        assert_eq!(area(EN, SquareMeters::new(850.0)?), "850 m²");
        assert_eq!(area(EN, SquareMeters::new(1000.0)?), "0.1 ha");
        assert_eq!(area(EN, SquareMeters::new(4000.0)?), "0.4 ha");
        Ok(())
    }

    #[test]
    fn times_are_24_hour() -> Result<(), pingers_gate::GateError> {
        let at = |minute| MinuteOfDay::new(minute).map(LocalTime::from_minute_of_day);
        assert_eq!(time(EN, at(7 * 60)?), "07:00");
        assert_eq!(time_range(EN, at(23 * 60)?, at(7 * 60 + 5)?), "23:00–07:05");
        Ok(())
    }

    #[test]
    fn every_direction_and_colour_has_a_name() {
        let directions = [
            Direction::N,
            Direction::NE,
            Direction::E,
            Direction::SE,
            Direction::S,
            Direction::SW,
            Direction::W,
            Direction::NW,
        ]
        .map(|point| direction(EN, point));
        assert_eq!(
            directions,
            [
                "north",
                "north-east",
                "east",
                "south-east",
                "south",
                "south-west",
                "west",
                "north-west"
            ]
        );
        assert_eq!(
            PartyColour::ALL.map(|swatch| colour(EN, swatch)),
            ["Blue", "Orange", "Ink", "Teal", "Plum"]
        );
        assert_eq!(privacy_level(EN, PrivacyLevel::Full), "Full privacy");
        assert_eq!(privacy_level(EN, PrivacyLevel::Reduced), "Reduced privacy");
    }

    #[test]
    fn people_are_named_or_you() -> Result<(), pingers_screens::ScreenError> {
        let ben = Person::new(LeafIndex::new(3), &Name::new("Ben")?, Me::Other);
        let sam = Person::new(LeafIndex::new(0), &Name::new("Sam")?, Me::Me);
        assert_eq!(person_in_sentence(EN, &ben), "Ben");
        assert_eq!(person_in_sentence(EN, &sam), "you");
        assert_eq!(person_on_its_own(EN, &sam), "You");
        assert_eq!(person_on_its_own(EN, &ben), "Ben");
        assert_eq!(avatar(EN, &ben), "BE");
        assert_eq!(avatar(EN, &sam), "You");
        assert_eq!(done(EN), "Done");
        assert_eq!(back_to(EN, "Climbing crew"), "Back to Climbing crew");
        Ok(())
    }
}
