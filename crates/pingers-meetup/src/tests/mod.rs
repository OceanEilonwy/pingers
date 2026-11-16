// SPDX-License-Identifier: AGPL-3.0-only
//! Session-level tests: several devices' sessions wired together by an in-memory group that
//! delivers every sent frame to every other member, as the MLS group and live channel would;
//! a member marked offline gets only stored frames, from the log, when it reconnects.
#![expect(
    clippy::wildcard_enum_match_arm,
    reason = "tests pick out the one variant they check"
)]

mod catch_up;
mod compass;
mod consent;
mod lifecycle;
mod meet_point;
mod properties;
mod traffic;
mod vectors;

use std::collections::{BTreeMap, VecDeque};

use pingers_crypto::{AccountPublicKey, GrantKeyPair};
use pingers_geo::{Coordinate, Fix, Meters};
use pingers_proto::{
    Delivery, Epoch, GroupId, LeafIndex, LocationRequestId, MeetupMessage, Padded,
    SealedLocationFrame, SizeClass, Urgency, WireEncode as _,
};
use pingers_testkit::{TestRng, seeded_rng};
use pingers_time::UnixSeconds;

use crate::{
    AllowListChoice, AlwaysAllow, AskBack, EndReason, ExactLocation, GrantScope, LocationAnswer,
    LocationVisibility, MeetupAction, MeetupConfig, MeetupError, MeetupEvent, MeetupInput,
    MeetupOutput, MeetupSession, Member, OutgoingFrame,
};

/// Why a test failed, other than an assertion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    #[error(transparent)]
    Meetup(#[from] MeetupError),
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    #[error(transparent)]
    Vector(#[from] pingers_testkit::VectorError),
    #[error("test setup: {0}")]
    Setup(&'static str),
}

pub(crate) type TestResult = Result<(), TestError>;

/// Wall-clock start of every test meetup.
pub(crate) const START: u64 = 1_800_000_000;

pub(crate) const ALICE: LeafIndex = LeafIndex::new(0);
pub(crate) const BEN: LeafIndex = LeafIndex::new(1);
pub(crate) const CHLOE: LeafIndex = LeafIndex::new(2);
pub(crate) const DAN: LeafIndex = LeafIndex::new(3);

pub(crate) const fn at(seconds_after_start: u64) -> UnixSeconds {
    UnixSeconds::new(START.saturating_add(seconds_after_start))
}

/// A member's fixed signature key in tests.
pub(crate) fn signature(leaf: LeafIndex) -> Result<AccountPublicKey, TestError> {
    let byte = u8::try_from(leaf.get()).map_err(|_range| TestError::Setup("leaf too large"))?;
    Ok(AccountPublicKey::from_bytes([byte.wrapping_add(1); 32]))
}

/// How one device starts.
#[derive(Clone, Debug)]
pub(crate) struct Setup {
    pub(crate) visibility: LocationVisibility,
    pub(crate) always_allow: Vec<LeafIndex>,
}

impl Setup {
    pub(crate) const fn with(visibility: LocationVisibility) -> Self {
        Self {
            visibility,
            always_allow: Vec::new(),
        }
    }
}

/// `count` devices that all ask each time.
pub(crate) fn asking(count: usize) -> Vec<Setup> {
    vec![Setup::with(LocationVisibility::AskEachTime); count]
}

/// The grant keys of a test Meetup: X25519, or X-Wing as in a quantum-resistant Party's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantSuite {
    Classical,
    XWing,
}

/// A frame a device sent, and when.
#[derive(Clone, Debug)]
pub(crate) struct Sent {
    pub(crate) at: UnixSeconds,
    pub(crate) frame: OutgoingFrame,
}

/// One device in the test group.
pub(crate) struct Device {
    pub(crate) member: Member,
    pub(crate) session: MeetupSession,
    pub(crate) events: Vec<MeetupEvent>,
    pub(crate) removals: Vec<LeafIndex>,
    pub(crate) ended: Option<EndReason>,
    pub(crate) sent: Vec<Sent>,
}

/// A stored frame waiting in the group log for a device whose live channel is down.
#[derive(Clone, Debug)]
pub(crate) struct Logged {
    pub(crate) sender: LeafIndex,
    pub(crate) epoch: Epoch,
    pub(crate) message: MeetupMessage,
}

/// Every device's session, joined by a perfect in-memory group.
pub(crate) struct Net {
    pub(crate) group: GroupId,
    pub(crate) epoch: Epoch,
    pub(crate) now: UnixSeconds,
    pub(crate) rng: TestRng,
    pub(crate) devices: BTreeMap<LeafIndex, Device>,
    /// Devices whose live channel is down, with the stored frames they have yet to read
    /// from the group log. Ephemeral frames do not reach them.
    pub(crate) offline: BTreeMap<LeafIndex, Vec<Logged>>,
}

impl Net {
    /// Devices at leaves 0, 1, 2, … with these setups, started at [`START`] and ticked once
    /// (start grants and the first location frames are delivered).
    pub(crate) fn new(setups: &[Setup], seed: u64) -> Result<Self, TestError> {
        Self::with_suite(setups, seed, GrantSuite::Classical)
    }

    /// As [`Net::new`], every member's grant key of `suite`.
    pub(crate) fn with_suite(
        setups: &[Setup],
        seed: u64,
        suite: GrantSuite,
    ) -> Result<Self, TestError> {
        let mut rng = seeded_rng(seed);
        let group = GroupId::from_bytes([7; 32]);
        let mut keys = Vec::new();
        for index in 0..setups.len() {
            let leaf = LeafIndex::new(
                u32::try_from(index).map_err(|_range| TestError::Setup("too many devices"))?,
            );
            let pair = match suite {
                GrantSuite::Classical => GrantKeyPair::generate(&mut rng)?,
                GrantSuite::XWing => GrantKeyPair::generate_x_wing(&mut rng)?,
            };
            let member = Member {
                leaf,
                signature_key: signature(leaf)?,
                grant_key: pair.public,
            };
            keys.push((member, pair.secret));
        }
        let members: Vec<Member> = keys
            .iter()
            .map(|(member, _secret)| member.clone())
            .collect();
        let mut devices = BTreeMap::new();
        for ((member, secret), setup) in keys.into_iter().zip(setups) {
            let session = MeetupSession::start(
                MeetupConfig {
                    group,
                    own_leaf: member.leaf,
                    epoch: Epoch::new(1),
                    own_grant_key: secret,
                    members: members.clone(),
                    visibility: setup.visibility,
                    always_allow: always_allow(&setup.always_allow)?,
                    joined_at: UnixSeconds::new(START),
                },
                &mut rng,
            )?;
            devices.insert(member.leaf, device(member, session));
        }
        let mut net = Self {
            group,
            epoch: Epoch::new(1),
            now: UnixSeconds::new(START),
            rng,
            devices,
            offline: BTreeMap::new(),
        };
        net.tick_all()?;
        Ok(net)
    }

    pub(crate) fn device(&self, leaf: LeafIndex) -> Result<&Device, TestError> {
        self.devices
            .get(&leaf)
            .ok_or(TestError::Setup("no such device"))
    }

    pub(crate) fn device_mut(&mut self, leaf: LeafIndex) -> Result<&mut Device, TestError> {
        self.devices
            .get_mut(&leaf)
            .ok_or(TestError::Setup("no such device"))
    }

    pub(crate) fn session(&self, leaf: LeafIndex) -> Result<&MeetupSession, TestError> {
        Ok(&self.device(leaf)?.session)
    }

    /// Feeds `input` to one device and delivers everything that follows.
    pub(crate) fn run(
        &mut self,
        leaf: LeafIndex,
        input: MeetupInput,
    ) -> Result<Vec<MeetupOutput>, TestError> {
        let now = self.now;
        let outputs = self
            .devices
            .get_mut(&leaf)
            .ok_or(TestError::Setup("no such device"))?
            .session
            .handle(input, now, &mut self.rng)?;
        self.dispatch(leaf, outputs.clone())?;
        Ok(outputs)
    }

    pub(crate) fn act(
        &mut self,
        leaf: LeafIndex,
        action: MeetupAction,
    ) -> Result<Vec<MeetupOutput>, TestError> {
        self.run(leaf, MeetupInput::Action(action))
    }

    /// Delivers a frame to one device only, as if `sender` had sent it.
    pub(crate) fn deliver(
        &mut self,
        sender: LeafIndex,
        receiver: LeafIndex,
        message: MeetupMessage,
    ) -> Result<Vec<MeetupOutput>, TestError> {
        let epoch = self.epoch;
        self.run(
            receiver,
            MeetupInput::Received {
                sender,
                epoch,
                message,
            },
        )
    }

    fn dispatch(&mut self, from: LeafIndex, outputs: Vec<MeetupOutput>) -> TestResult {
        let mut queue = VecDeque::from([(from, outputs)]);
        while let Some((sender, outputs)) = queue.pop_front() {
            for output in outputs {
                let now = self.now;
                let Some(device) = self.devices.get_mut(&sender) else {
                    continue;
                };
                match output {
                    MeetupOutput::Send(frame) => {
                        device.sent.push(Sent {
                            at: now,
                            frame: frame.clone(),
                        });
                        for (leaf, other) in &mut self.devices {
                            if *leaf == sender || other.ended.is_some() {
                                continue;
                            }
                            if let Some(log) = self.offline.get_mut(leaf) {
                                if frame.delivery == Delivery::Stored {
                                    log.push(Logged {
                                        sender,
                                        epoch: self.epoch,
                                        message: frame.message.clone(),
                                    });
                                }
                                continue;
                            }
                            let received = other.session.handle(
                                MeetupInput::Received {
                                    sender,
                                    epoch: self.epoch,
                                    message: frame.message.clone(),
                                },
                                now,
                                &mut self.rng,
                            )?;
                            queue.push_back((*leaf, received));
                        }
                    }
                    MeetupOutput::Event(event) => device.events.push(event),
                    MeetupOutput::RemoveSilentMember(leaf) => device.removals.push(leaf),
                    MeetupOutput::End(reason) => device.ended = Some(reason),
                }
            }
        }
        Ok(())
    }

    /// Closes a device's live channel: from now on it only gets stored frames, from the log.
    pub(crate) fn go_offline(&mut self, leaf: LeafIndex) {
        self.offline.insert(leaf, Vec::new());
    }

    /// Re-opens a device's live channel and has it read the group log it missed, in order.
    pub(crate) fn reconnect(&mut self, leaf: LeafIndex) -> TestResult {
        for logged in self.offline.remove(&leaf).unwrap_or_default() {
            self.run(
                leaf,
                MeetupInput::Received {
                    sender: logged.sender,
                    epoch: logged.epoch,
                    message: logged.message,
                },
            )?;
        }
        Ok(())
    }

    /// Ticks every running device once at the current time.
    pub(crate) fn tick_all(&mut self) -> TestResult {
        for leaf in self.running() {
            self.run(leaf, MeetupInput::Tick)?;
        }
        Ok(())
    }

    fn running(&self) -> Vec<LeafIndex> {
        self.devices
            .iter()
            .filter(|(_leaf, device)| device.ended.is_none())
            .map(|(leaf, _device)| *leaf)
            .collect()
    }

    /// Advances one second at a time, ticking each device when its deadline arrives.
    pub(crate) fn advance(&mut self, seconds: u64) -> TestResult {
        for _ in 0..seconds {
            self.now = UnixSeconds::new(self.now.get().saturating_add(1));
            for leaf in self.running() {
                let due = self
                    .session(leaf)?
                    .next_deadline()
                    .is_some_and(|deadline| deadline <= self.now);
                if due {
                    self.run(leaf, MeetupInput::Tick)?;
                }
            }
        }
        Ok(())
    }

    /// Gives a device a position fix at the current time.
    pub(crate) fn fix(&mut self, leaf: LeafIndex, latitude: f64, longitude: f64) -> TestResult {
        let fix = Fix {
            coordinate: Coordinate::new(latitude, longitude)?,
            accuracy: Meters::from_whole(5),
            taken_at: self.now,
        };
        self.run(leaf, MeetupInput::OwnFix(fix))?;
        Ok(())
    }

    /// Takes a device's events so far.
    pub(crate) fn take_events(&mut self, leaf: LeafIndex) -> Result<Vec<MeetupEvent>, TestError> {
        Ok(core::mem::take(&mut self.device_mut(leaf)?.events))
    }

    /// Takes a device's sent frames so far.
    pub(crate) fn take_sent(&mut self, leaf: LeafIndex) -> Result<Vec<Sent>, TestError> {
        Ok(core::mem::take(&mut self.device_mut(leaf)?.sent))
    }

    /// The id of the latest request `leaf` was asked to answer.
    pub(crate) fn last_request(&self, leaf: LeafIndex) -> Result<LocationRequestId, TestError> {
        self.device(leaf)?
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                MeetupEvent::LocationRequested { request, .. } => Some(*request),
                _ => None,
            })
            .ok_or(TestError::Setup("no request"))
    }

    /// `asker` asks `target`; returns the request id `target` received.
    pub(crate) fn ask(
        &mut self,
        asker: LeafIndex,
        target: LeafIndex,
    ) -> Result<LocationRequestId, TestError> {
        self.act(asker, MeetupAction::AskLocation(target))?;
        self.last_request(target)
    }

    /// `asker` asks `target`, who allows with `scope`.
    pub(crate) fn grant(
        &mut self,
        asker: LeafIndex,
        target: LeafIndex,
        scope: GrantScope,
    ) -> TestResult {
        let request = self.ask(asker, target)?;
        self.act(
            target,
            MeetupAction::AnswerRequest {
                request,
                answer: allow(scope),
            },
        )?;
        Ok(())
    }

    /// Removes a member from the group: a new epoch for everyone else.
    pub(crate) fn remove(&mut self, leaf: LeafIndex) -> Result<Device, TestError> {
        let removed = self
            .devices
            .remove(&leaf)
            .ok_or(TestError::Setup("no such device"))?;
        self.epoch = self.epoch.checked_next().ok_or(TestError::Setup("epoch"))?;
        self.members_changed()?;
        Ok(removed)
    }

    /// Adds a new member at `leaf` with `setup`: it starts, then everyone learns of it.
    pub(crate) fn add(&mut self, leaf: LeafIndex, setup: &Setup) -> TestResult {
        self.add_with_suite(leaf, setup, GrantSuite::Classical)
    }

    /// As [`Net::add`], the newcomer's grant key of `suite`.
    pub(crate) fn add_with_suite(
        &mut self,
        leaf: LeafIndex,
        setup: &Setup,
        suite: GrantSuite,
    ) -> TestResult {
        let pair = match suite {
            GrantSuite::Classical => GrantKeyPair::generate(&mut self.rng)?,
            GrantSuite::XWing => GrantKeyPair::generate_x_wing(&mut self.rng)?,
        };
        let member = Member {
            leaf,
            signature_key: signature(leaf)?,
            grant_key: pair.public,
        };
        self.epoch = self.epoch.checked_next().ok_or(TestError::Setup("epoch"))?;
        let mut members = self.members();
        members.push(member.clone());
        let session = MeetupSession::start(
            MeetupConfig {
                group: self.group,
                own_leaf: leaf,
                epoch: self.epoch,
                own_grant_key: pair.secret,
                members,
                visibility: setup.visibility,
                always_allow: always_allow(&setup.always_allow)?,
                joined_at: self.now,
            },
            &mut self.rng,
        )?;
        self.devices.insert(leaf, device(member, session));
        self.members_changed()?;
        Ok(())
    }

    pub(crate) fn members(&self) -> Vec<Member> {
        self.devices
            .values()
            .map(|device| device.member.clone())
            .collect()
    }

    /// Tells every running device the current membership.
    pub(crate) fn members_changed(&mut self) -> TestResult {
        let members = self.members();
        for leaf in self.running() {
            self.run(
                leaf,
                MeetupInput::MembersChanged {
                    epoch: self.epoch,
                    members: members.clone(),
                },
            )?;
        }
        Ok(())
    }

    pub(crate) fn access(
        &self,
        viewer: LeafIndex,
        peer: LeafIndex,
    ) -> Result<ExactLocation, TestError> {
        Ok(self.session(viewer)?.exact_location(peer))
    }
}

fn device(member: Member, session: MeetupSession) -> Device {
    Device {
        member,
        session,
        events: Vec::new(),
        removals: Vec::new(),
        ended: None,
        sent: Vec::new(),
    }
}

fn always_allow(leaves: &[LeafIndex]) -> Result<AlwaysAllow, TestError> {
    Ok(AlwaysAllow::new(
        leaves
            .iter()
            .map(|leaf| signature(*leaf))
            .collect::<Result<_, _>>()?,
    ))
}

/// "Allow" with `scope`, no ask-back, not on the allow-list.
pub(crate) const fn allow(scope: GrantScope) -> LocationAnswer {
    LocationAnswer::Allow {
        scope,
        ask_back: AskBack::DoNotAskBack,
        always: AllowListChoice::JustThisTime,
    }
}

/// What an observer without keys sees of a frame: when, its kind, padded size, delivery and
/// wake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) at: UnixSeconds,
    pub(crate) kind: &'static str,
    pub(crate) size: usize,
    pub(crate) delivery: Delivery,
    pub(crate) wake: Option<Urgency>,
}

pub(crate) fn shape(sent: &Sent) -> Result<Shape, TestError> {
    let encoded = sent.frame.message.encode()?;
    let padded = Padded::pad_smallest(&encoded, &SizeClass::MEETUP_FRAME)?;
    Ok(Shape {
        at: sent.at,
        kind: kind(&sent.frame.message),
        size: padded.as_bytes().len(),
        delivery: sent.frame.delivery,
        wake: sent.frame.wake,
    })
}

pub(crate) fn shapes(sent: &[Sent]) -> Result<Vec<Shape>, TestError> {
    sent.iter().map(shape).collect()
}

pub(crate) const fn kind(message: &MeetupMessage) -> &'static str {
    match message {
        MeetupMessage::SealedLocation(_) => "location",
        MeetupMessage::LocationGrant(_) => "grant",
        MeetupMessage::LocationAsk(_) => "ask",
        MeetupMessage::Chat { .. } => "chat",
        MeetupMessage::MeetPoint(_) => "meet point",
        MeetupMessage::Snapshot { .. } => "snapshot",
        MeetupMessage::BoardSeen { .. } => "board seen",
        MeetupMessage::KeepAlive => "keep-alive",
        _ => "other",
    }
}

/// The location frames among sent frames.
pub(crate) fn location_frames(sent: &[Sent]) -> Vec<&SealedLocationFrame> {
    sent.iter()
        .filter_map(|sent| match &sent.frame.message {
            MeetupMessage::SealedLocation(frame) => Some(frame),
            _ => None,
        })
        .collect()
}

/// How many frames of `kind` were sent.
pub(crate) fn count(sent: &[Sent], wanted: &str) -> usize {
    sent.iter()
        .filter(|sent| kind(&sent.frame.message) == wanted)
        .count()
}

/// How many positions of `peer` the events carry.
pub(crate) fn positions_from(events: &[MeetupEvent], peer: LeafIndex) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, MeetupEvent::PeerLocation(location) if location.leaf == peer),
        )
        .count()
}
