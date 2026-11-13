// SPDX-License-Identifier: AGPL-3.0-only
//! Property tests of the session state machine over random sequences of consent actions,
//! fixes, cadence changes and time.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use pingers_proto::LeafIndex;
use proptest::prelude::*;

use super::{Net, Shape, TestError, TestResult, asking, shapes};
use crate::{
    CadenceMode, GrantScope, LocationVisibility, MeetupAction, MeetupError, MeetupEvent,
    MeetupInput,
};

const DEVICES: u32 = 3;

#[derive(Clone, Debug)]
enum Op {
    Wait(u8),
    Fix(u32),
    Cadence(u32, CadenceMode),
    Ask(u32, u32),
    Answer(u32, Option<GrantScope>),
    Revoke(u32, u32),
    Visibility(u32, LocationVisibility),
}

impl Op {
    /// Whether the op is a consent decision (as opposed to time, position or screen state).
    const fn is_consent(&self) -> bool {
        matches!(
            self,
            Self::Ask(..) | Self::Answer(..) | Self::Revoke(..) | Self::Visibility(..)
        )
    }
}

fn leaf() -> impl Strategy<Value = u32> {
    0..DEVICES
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (1_u8..=90).prop_map(Op::Wait),
        1 => leaf().prop_map(Op::Fix),
        1 => (leaf(), prop_oneof![Just(CadenceMode::Live), Just(CadenceMode::Background)])
            .prop_map(|(leaf, mode)| Op::Cadence(leaf, mode)),
        2 => (leaf(), leaf()).prop_map(|(from, to)| Op::Ask(from, to)),
        2 => (leaf(), prop_oneof![
                Just(None),
                Just(Some(GrantScope::ThisMeetup)),
                Just(Some(GrantScope::FifteenMinutes)),
            ])
            .prop_map(|(leaf, answer)| Op::Answer(leaf, answer)),
        1 => (leaf(), leaf()).prop_map(|(from, to)| Op::Revoke(from, to)),
        1 => (leaf(), prop_oneof![
                Just(LocationVisibility::AskEachTime),
                Just(LocationVisibility::Everyone),
                Just(LocationVisibility::Nobody),
            ])
            .prop_map(|(leaf, visibility)| Op::Visibility(leaf, visibility)),
    ]
}

/// Every position a device decrypted came from a member that shares with it right now.
fn only_grantors_are_seen(net: &mut Net) -> TestResult {
    for viewer in 0..DEVICES {
        let viewer = LeafIndex::new(viewer);
        for event in net.take_events(viewer)? {
            if let MeetupEvent::PeerLocation(location) = event {
                assert!(
                    net.session(location.leaf)?.shares_with(viewer),
                    "a position was decrypted without a current grant"
                );
            }
        }
    }
    Ok(())
}

fn apply(net: &mut Net, op: &Op) -> TestResult {
    let result = match *op {
        Op::Wait(seconds) => {
            for _ in 0..seconds {
                net.advance(1)?;
                only_grantors_are_seen(net)?;
            }
            Ok(Vec::new())
        }
        Op::Fix(leaf) => {
            let latitude = f64::from(leaf) / 1_000.0;
            net.fix(LeafIndex::new(leaf), latitude, 0.5)?;
            Ok(Vec::new())
        }
        Op::Cadence(leaf, mode) => net.run(LeafIndex::new(leaf), MeetupInput::Cadence(mode)),
        Op::Ask(from, to) => net.act(
            LeafIndex::new(from),
            MeetupAction::AskLocation(LeafIndex::new(to)),
        ),
        Op::Answer(leaf, scope) => match net.last_request(LeafIndex::new(leaf)) {
            Ok(request) => net.act(
                LeafIndex::new(leaf),
                MeetupAction::AnswerRequest {
                    request,
                    answer: scope.map_or(crate::LocationAnswer::NotNow, super::allow),
                },
            ),
            Err(_none) => Ok(Vec::new()),
        },
        Op::Revoke(from, to) => net.act(
            LeafIndex::new(from),
            MeetupAction::RevokeLocation(LeafIndex::new(to)),
        ),
        Op::Visibility(leaf, visibility) => net.act(
            LeafIndex::new(leaf),
            MeetupAction::SetVisibility(visibility),
        ),
    };
    match result {
        // Self-targeted or already answered actions are rejected; that is fine here.
        Ok(_)
        | Err(TestError::Meetup(MeetupError::CannotTargetSelf | MeetupError::UnknownRequest)) => {
            only_grantors_are_seen(net)
        }
        Err(error) => Err(error),
    }
}

/// Every device's location frames, as an observer without keys sees them.
fn location_shapes(net: &mut Net) -> Result<Vec<Vec<Shape>>, TestError> {
    (0..DEVICES)
        .map(|leaf| {
            let sent = net.take_sent(LeafIndex::new(leaf))?;
            Ok(shapes(&sent)?
                .into_iter()
                .filter(|shape| shape.kind == "location")
                .collect())
        })
        .collect()
}

fn run(ops: &[Op]) -> Result<Vec<Vec<Shape>>, TestError> {
    let mut net = Net::new(&asking(3), 100)?;
    for op in ops {
        apply(&mut net, op)?;
    }
    location_shapes(&mut net)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Consent decisions never change when location frames go out or how large they are, and
    /// no device ever decrypts a member who does not currently share with it.
    #[test]
    fn consent_never_shows_in_location_traffic(ops in proptest::collection::vec(op(), 1..30)) {
        let with_consent = run(&ops).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let without: Vec<Op> = ops.iter().filter(|op| !op.is_consent()).cloned().collect();
        let baseline = run(&without).map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(with_consent, baseline);
    }
}
