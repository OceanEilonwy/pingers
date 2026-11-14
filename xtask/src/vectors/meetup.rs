// SPDX-License-Identifier: AGPL-3.0-only
//! `vectors/meetup/*.json`: pingers-meetup's coordinate rounding, compass readings and meet
//! point resolution, computed here from the formulas rather than with pingers-meetup.
//!
//! Floats follow the formulas operation by operation, in the order written, through the
//! platform's libm; the expected values are then rounded (distances to 4 decimal places,
//! bearings to 9), and the files say how closely an implementation must match.

use anyhow::{Context as _, Result, bail};

use super::json::{Json, object};
use super::{VectorFile, case, vector_file};

/// Mean Earth radius in metres (IUGG).
const EARTH_RADIUS_METERS: f64 = 6_371_008.8;

/// The wire's coordinate scale: 1e-7 degrees.
const E7_DIGITS: usize = 7;

pub(super) fn files() -> Result<Vec<VectorFile>> {
    Ok(vec![coordinate_wire()?, compass()?, meet_point()?])
}

fn meetup_file(name: &str, description: &str, vectors: Vec<Json>) -> Result<VectorFile> {
    vector_file("meetup", &format!("meetup/{name}"), description, vectors)
}

/// Decimal degrees as written to 1e-7 degrees, rounded half away from zero on the decimal
/// value itself (so `12.34567895` rounds up even though the nearest f64 is just below it).
fn e7(degrees: &str) -> Result<i64> {
    let (negative, magnitude) = degrees
        .strip_prefix('-')
        .map_or((false, degrees), |rest| (true, rest));
    let (whole, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
    if whole.is_empty()
        || !whole
            .chars()
            .chain(fraction.chars())
            .all(|c| c.is_ascii_digit())
    {
        bail!("{degrees:?} is not a decimal number");
    }
    let kept: String = fraction
        .chars()
        .chain(std::iter::repeat('0'))
        .take(E7_DIGITS)
        .collect();
    let round_up = fraction
        .chars()
        .nth(E7_DIGITS)
        .is_some_and(|first_dropped| first_dropped >= '5');
    let value = whole
        .parse::<i64>()?
        .checked_mul(10_000_000)
        .and_then(|scaled| scaled.checked_add(kept.parse().ok()?))
        .and_then(|value| value.checked_add(i64::from(round_up)))
        .with_context(|| format!("{degrees} is out of range"))?;
    Ok(if negative {
        value.saturating_neg()
    } else {
        value
    })
}

fn coordinate_wire() -> Result<VectorFile> {
    let cases = [
        ("london-eye", "51.5033", "-0.1196"),
        ("rounds-down-below-half", "12.34567891", "-12.34567891"),
        ("rounds-up-above-half", "12.34567896", "-12.34567896"),
        ("north-pole-date-line", "90", "180"),
        ("south-pole-date-line", "-90", "-180"),
        ("null-island", "0", "0"),
    ];
    let mut vectors = Vec::new();
    for (name, latitude, longitude) in cases {
        vectors.push(case(
            name,
            object! { "latitude": latitude.parse::<f64>()?, "longitude": longitude.parse::<f64>()? },
            object! { "latitude_e7": e7(latitude)?, "longitude_e7": e7(longitude)? },
        ));
    }
    meetup_file(
        "coordinate-wire",
        "Geo coordinates to the wire's 1e-7 degrees (design B.6 lat_e7/lon_e7), rounded half away from zero (pingers-meetup coordinate_to_wire).",
        vectors,
    )
}

/// A latitude and longitude in degrees.
#[derive(Clone, Copy)]
struct Point {
    latitude: f64,
    longitude: f64,
}

/// The remainder with the divisor's sign, as the original generator's `%` computed it:
/// `fmod`, moved into the divisor's range, and a zero that takes the divisor's sign.
fn floored_remainder(value: f64, divisor: f64) -> f64 {
    let remainder = value % divisor;
    if remainder == 0.0 {
        0.0_f64.copysign(divisor)
    } else if (divisor < 0.0) == (remainder < 0.0) {
        remainder
    } else {
        remainder + divisor
    }
}

/// `value` rounded to `digits` decimal places: the correctly rounded decimal, read back.
fn round_to(value: f64, digits: usize) -> Result<f64> {
    Ok(format!("{value:.digits$}").parse()?)
}

/// Great-circle distance in metres (haversine).
fn haversine(from: Point, to: Point) -> f64 {
    let (p1, p2) = (from.latitude.to_radians(), to.latitude.to_radians());
    let latitude_delta = (to.latitude - from.latitude).to_radians();
    let longitude_delta = (to.longitude - from.longitude).to_radians();
    let h = (latitude_delta / 2.0).sin().powf(2.0)
        + p1.cos() * p2.cos() * (longitude_delta / 2.0).sin().powf(2.0);
    EARTH_RADIUS_METERS * 2.0 * h.sqrt().min(1.0).asin()
}

/// Initial bearing from `from` to `to`, degrees clockwise from north in [0, 360).
fn initial_bearing(from: Point, to: Point) -> f64 {
    let (p1, p2) = (from.latitude.to_radians(), to.latitude.to_radians());
    let longitude_delta = (to.longitude - from.longitude).to_radians();
    let east = longitude_delta.sin() * p2.cos();
    let north = p1.cos() * p2.sin() - p1.sin() * p2.cos() * longitude_delta.cos();
    floored_remainder(east.atan2(north) * (180.0 / std::f64::consts::PI), 360.0)
}

/// One compass case's inputs.
struct CompassCase {
    name: &'static str,
    observer: Point,
    heading: Option<f64>,
    target: Point,
    taken_at: i64,
    now: i64,
}

fn compass_case(reading: &CompassCase) -> Result<Json> {
    let bearing = initial_bearing(reading.observer, reading.target);
    let mut expected = vec![
        (
            "distance_meters".to_owned(),
            Json::from(round_to(haversine(reading.observer, reading.target), 4)?),
        ),
        (
            "bearing_degrees".to_owned(),
            Json::from(round_to(bearing, 9)?),
        ),
        (
            "age_seconds".to_owned(),
            Json::from(reading.now.saturating_sub(reading.taken_at).max(0)),
        ),
    ];
    let mut inputs = vec![
        (
            "observer_latitude".to_owned(),
            Json::from(reading.observer.latitude),
        ),
        (
            "observer_longitude".to_owned(),
            Json::from(reading.observer.longitude),
        ),
        (
            "target_latitude".to_owned(),
            Json::from(reading.target.latitude),
        ),
        (
            "target_longitude".to_owned(),
            Json::from(reading.target.longitude),
        ),
        ("taken_at".to_owned(), Json::from(reading.taken_at)),
        ("now".to_owned(), Json::from(reading.now)),
    ];
    if let Some(heading) = reading.heading {
        expected.push((
            "relative_bearing_degrees".to_owned(),
            Json::from(round_to(floored_remainder(bearing - heading, 360.0), 9)?),
        ));
        inputs.push(("heading_degrees".to_owned(), Json::from(heading)));
    }
    Ok(case(
        reading.name,
        Json::Object(inputs),
        Json::Object(expected),
    ))
}

fn compass() -> Result<VectorFile> {
    let point = |latitude, longitude| Point {
        latitude,
        longitude,
    };
    let cases = [
        CompassCase {
            name: "north-facing-east",
            observer: point(0.0, 0.0),
            heading: Some(90.0),
            target: point(0.001, 0.0),
            taken_at: 100,
            now: 130,
        },
        CompassCase {
            name: "east-facing-north",
            observer: point(0.0, 0.0),
            heading: Some(0.0),
            target: point(0.0, 0.001),
            taken_at: 50,
            now: 50,
        },
        CompassCase {
            name: "south-west-facing-south-east",
            observer: point(51.5, -0.12),
            heading: Some(135.0),
            target: point(51.499, -0.121),
            taken_at: 0,
            now: 2,
        },
        CompassCase {
            name: "no-heading",
            observer: point(10.0, 10.0),
            heading: None,
            target: point(10.0005, 10.0005),
            taken_at: 7,
            now: 9,
        },
        CompassCase {
            name: "same-place",
            observer: point(1.0, 1.0),
            heading: Some(10.0),
            target: point(1.0, 1.0),
            taken_at: 5,
            now: 5,
        },
        CompassCase {
            name: "target-from-the-future",
            observer: point(0.0, 0.0),
            heading: None,
            target: point(0.0, -0.001),
            taken_at: 200,
            now: 100,
        },
    ];
    let vectors = cases.iter().map(compass_case).collect::<Result<_>>()?;
    meetup_file(
        "compass",
        "Compass readings (pingers-meetup compass): great-circle distance (mean Earth radius 6,371,008.8 m) and initial bearing from observer to target, bearing relative to the heading when one is given, and the age of the target's fix (zero if it is from the future). Distances compare within 0.01 m, bearings within 1e-6 degrees. Without a heading there is no relative bearing.",
        vectors,
    )
}

fn fixed(sender: i64, lamport: i64) -> Json {
    object! { "sender": sender, "kind": "fixed", "lamport": lamport }
}

fn at_member(sender: i64, leaf: i64, lamport: i64) -> Json {
    object! { "sender": sender, "kind": "at_member", "leaf": leaf, "lamport": lamport }
}

fn cleared(sender: i64, lamport: i64) -> Json {
    object! { "sender": sender, "kind": "cleared", "lamport": lamport }
}

/// The winning write's kind, sender and lamport.
fn winner(kind: &str, sender: i64, lamport: i64) -> Json {
    object! { "kind": kind, "sender": sender, "lamport": lamport }
}

/// No meet point, with the highest lamport seen.
fn no_meet_point(lamport: i64) -> Json {
    object! { "kind": "none", "lamport": lamport }
}

fn meet_point() -> Result<VectorFile> {
    let cases = [
        (
            "later-lamport-wins",
            vec![fixed(1, 1), at_member(2, 2, 2)],
            winner("at_member", 2, 2),
        ),
        (
            "earlier-lamport-loses",
            vec![fixed(1, 3), cleared(2, 2)],
            winner("fixed", 1, 3),
        ),
        (
            "tie-goes-to-higher-leaf",
            vec![fixed(3, 4), fixed(1, 4)],
            winner("fixed", 3, 4),
        ),
        (
            "tie-goes-to-higher-leaf-in-any-order",
            vec![fixed(1, 4), fixed(3, 4)],
            winner("fixed", 3, 4),
        ),
        (
            "at-member-from-someone-else-is-ignored",
            vec![fixed(1, 1), at_member(2, 5, 9)],
            winner("fixed", 1, 1),
        ),
        (
            "cleared-reads-as-none",
            vec![at_member(4, 4, 1), cleared(0, 2)],
            no_meet_point(2),
        ),
        ("nothing-written", Vec::new(), no_meet_point(0)),
    ];
    let vectors = cases
        .into_iter()
        .map(|(name, writes, expected)| case(name, object! { "writes": writes }, expected))
        .collect();
    meetup_file(
        "meet-point",
        "Meet point resolution (design B.6, B.6.1): writes applied in order; last writer wins by (lamport, sender leaf); AtMember counts only when sent by that leaf; Cleared reads as no meet point. Expected: the winner's kind, sender and lamport, or kind none with the highest lamport seen.",
        vectors,
    )
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn e7_rounds_half_away_from_zero_on_the_decimal() -> TestResult {
        assert_eq!(e7("12.34567895")?, 123_456_790);
        assert_eq!(e7("-12.34567895")?, -123_456_790);
        assert_eq!(e7("12.34567894999")?, 123_456_789);
        assert_eq!(e7("-0.1196")?, -1_196_000);
        assert_eq!(e7("180")?, 1_800_000_000);
        assert_eq!(e7("0")?, 0);
        Ok(())
    }

    #[test]
    fn e7_refuses_what_is_not_a_decimal() {
        assert!(e7("").is_err());
        assert!(e7(".5").is_err());
        assert!(e7("1e5").is_err());
        assert!(e7("--1").is_err());
        assert!(e7("99999999999999").is_err());
    }

    #[test]
    fn floored_remainder_takes_the_divisor_sign() {
        assert!((floored_remainder(-90.0, 360.0) - 270.0).abs() < f64::EPSILON);
        assert!((floored_remainder(450.0, 360.0) - 90.0).abs() < f64::EPSILON);
        assert!((floored_remainder(90.0, -360.0) - -270.0).abs() < f64::EPSILON);
        assert!(floored_remainder(-0.0, 360.0).is_sign_positive());
        assert!(floored_remainder(720.0, -360.0).is_sign_negative());
    }

    #[test]
    fn round_to_keeps_the_nearest_decimal() -> TestResult {
        assert!((round_to(111.195_079_734, 4)? - 111.1951).abs() < f64::EPSILON);
        assert!((round_to(-2.000_049, 4)? - -2.0).abs() < f64::EPSILON);
        Ok(())
    }
}
