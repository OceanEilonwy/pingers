// SPDX-License-Identifier: AGPL-3.0-only
//! The session's compass: never computed for a member whose key is not held (design B.6.1).
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_geo::{Bearing, Coordinate, Fix, Meters};

use super::{ALICE, BEN, CHLOE, Net, TestResult, asking};
use crate::{CompassTarget, GrantScope, MeetPointDraft, MeetupAction, Observer, SHORT_GRANT};

fn observer_at(latitude: f64, longitude: f64, net: &Net) -> Result<Observer, super::TestError> {
    Ok(Observer {
        fix: Fix {
            coordinate: Coordinate::new(latitude, longitude)?,
            accuracy: Meters::from_whole(5),
            taken_at: net.now,
        },
        heading: Some(Bearing::new(90.0)?),
    })
}

/// Ben stands 0.001° north of the origin and has sent one frame to Chloé.
fn ben_shared_with_chloe(scope: GrantScope, seed: u64) -> Result<Net, super::TestError> {
    let mut net = Net::new(&asking(3), seed)?;
    net.fix(BEN, 0.001, 0.0)?;
    net.grant(CHLOE, BEN, scope)?;
    net.advance(60)?;
    Ok(net)
}

#[test]
fn the_compass_points_at_a_member_whose_key_is_held() -> TestResult {
    let net = ben_shared_with_chloe(GrantScope::ThisMeetup, 70)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    let reading = net
        .session(CHLOE)?
        .compass(CompassTarget::Member(BEN), &observer, net.now)
        .ok_or(super::TestError::Setup("no reading"))?;
    assert!((reading.distance.get() - 111.195).abs() < 0.01);
    assert!(reading.bearing.degrees().abs() < 1e-6);
    // Facing east, north is a quarter turn to the left.
    let relative = reading
        .relative_bearing
        .ok_or(super::TestError::Setup("no heading"))?;
    assert!((relative.degrees() - 270.0).abs() < 1e-6);
    // The fix was taken at the start and sent with the frame a minute later.
    assert_eq!(reading.age, Duration::from_secs(60));
    Ok(())
}

#[test]
fn no_compass_without_the_key() -> TestResult {
    let net = ben_shared_with_chloe(GrantScope::ThisMeetup, 71)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    assert_eq!(
        net.session(ALICE)?
            .compass(CompassTarget::Member(BEN), &observer, net.now),
        None
    );
    Ok(())
}

#[test]
fn no_compass_with_the_key_but_no_position_yet() -> TestResult {
    let mut net = Net::new(&asking(3), 72)?;
    net.grant(CHLOE, BEN, GrantScope::ThisMeetup)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    assert_eq!(
        net.session(CHLOE)?
            .compass(CompassTarget::Member(BEN), &observer, net.now),
        None
    );
    Ok(())
}

#[test]
fn no_compass_towards_oneself() -> TestResult {
    let net = ben_shared_with_chloe(GrantScope::ThisMeetup, 73)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    assert_eq!(
        net.session(CHLOE)?
            .compass(CompassTarget::Member(CHLOE), &observer, net.now),
        None
    );
    Ok(())
}

#[test]
fn the_compass_stops_the_moment_a_grant_lapses() -> TestResult {
    let net = ben_shared_with_chloe(GrantScope::FifteenMinutes, 74)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    let session = net.session(CHLOE)?;
    // The grant was given at the start, 60 s before `net.now`.
    let lapse = net
        .now
        .checked_add(SHORT_GRANT.saturating_sub(Duration::from_secs(60)))
        .ok_or(super::TestError::Setup("time"))?;
    let just_before = pingers_time::UnixSeconds::new(lapse.get() - 1);
    let reading = session.compass(CompassTarget::Member(BEN), &observer, just_before);
    assert_eq!(
        reading.map(|reading| reading.age),
        Some(Duration::from_secs(lapse.get() - 1 - super::START))
    );
    assert_eq!(
        session.compass(CompassTarget::Member(BEN), &observer, lapse),
        None
    );
    Ok(())
}

#[test]
fn the_compass_points_at_a_fixed_meet_point_without_any_key() -> TestResult {
    let mut net = Net::new(&asking(3), 75)?;
    net.act(
        BEN,
        MeetupAction::SetMeetPoint(MeetPointDraft::Fixed {
            position: Coordinate::new(0.0, 0.001)?,
            label: None,
        }),
    )?;
    let observer = observer_at(0.0, 0.0, &net)?;
    let reading = net
        .session(ALICE)?
        .compass(CompassTarget::MeetPoint, &observer, net.now)
        .ok_or(super::TestError::Setup("no reading"))?;
    assert!((reading.bearing.degrees() - 90.0).abs() < 1e-6);
    assert_eq!(reading.age, Duration::ZERO);
    Ok(())
}

#[test]
fn meet_at_member_points_only_for_those_holding_the_key() -> TestResult {
    let mut net = ben_shared_with_chloe(GrantScope::ThisMeetup, 76)?;
    net.act(
        BEN,
        MeetupAction::SetVisibility(crate::LocationVisibility::Nobody),
    )?;
    net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::AtMe))?;
    net.advance(60)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    // Under Nobody, meet-at-me grants nobody: neither Alice nor Chloé can point at Ben.
    for viewer in [ALICE, CHLOE] {
        assert_eq!(
            net.session(viewer)?
                .compass(CompassTarget::MeetPoint, &observer, net.now),
            None
        );
    }
    // With the default setting it grants everyone, so Alice can.
    net.act(
        BEN,
        MeetupAction::SetVisibility(crate::LocationVisibility::AskEachTime),
    )?;
    net.advance(60)?;
    assert!(
        net.session(ALICE)?
            .compass(CompassTarget::MeetPoint, &observer, net.now)
            .is_some()
    );
    Ok(())
}

#[test]
fn no_compass_towards_a_cleared_or_missing_meet_point() -> TestResult {
    let mut net = Net::new(&asking(2), 77)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    assert_eq!(
        net.session(ALICE)?
            .compass(CompassTarget::MeetPoint, &observer, net.now),
        None
    );
    net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::Clear))?;
    assert_eq!(
        net.session(ALICE)?
            .compass(CompassTarget::MeetPoint, &observer, net.now),
        None
    );
    Ok(())
}

#[test]
fn the_meet_at_me_owner_gets_no_compass_to_itself() -> TestResult {
    let mut net = Net::new(&asking(2), 78)?;
    net.fix(BEN, 1.0, 1.0)?;
    net.act(BEN, MeetupAction::SetMeetPoint(MeetPointDraft::AtMe))?;
    net.advance(60)?;
    let observer = observer_at(0.0, 0.0, &net)?;
    assert_eq!(
        net.session(BEN)?
            .compass(CompassTarget::MeetPoint, &observer, net.now),
        None
    );
    Ok(())
}
