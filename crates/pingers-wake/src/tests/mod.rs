// SPDX-License-Identifier: AGPL-3.0-only
//! Wakes run against fakes: pingers-testkit's `FakePlatform` and manual clock, and the fakes in
//! [`fakes`] for the server, Parties and Meetups. Two phones share one server world, so the
//! design G sequences run end to end at the wake level.

mod fakes;
mod scenarios;
mod servers;
mod suppression;
mod timing;

use core::time::Duration;
use std::sync::{Arc, Mutex};

use pingers_crypto::AccountSigningKey;
use pingers_gate::{LocationSharing, MinuteOfDay, PauseState, QuietHoursSetting};
use pingers_geo::{Coordinate, GeofenceCapacity, Meters};
use pingers_presence::{DataMode, EpochState, EpochTransition, SpotRef};
use pingers_proto::{CoverReadRatio, Epoch, ServerId, SizeClass};
use pingers_testkit::{
    FakePlatform, PartyBuilder, PartyFixture, SpotFixture, TestRng, random_array, seeded_rng,
    test_clock,
};
use pingers_time::{Clock, ManualClock, UnixSeconds};

use self::fakes::{FakeMeetups, FakeParties, FakeServer, RosterMember, World};
use crate::ports::{PartyInfo, RegionId, SpotInfo};
use crate::{
    BoardSnapshot, GeofenceEvent, GeofenceTransition, MailboxReads, WakeEnv, WakeError, WakeInput,
    WakeReport, WakeSettings, WakeState, WakeTrigger,
};

/// The budget most tests give a wake (an FCM high-priority message allows about this much).
const BUDGET: Duration = Duration::from_secs(30);

/// The Party epoch phones start at.
const EPOCH: Epoch = Epoch::new(3);

/// Why a test failed before reaching an assertion.
#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Wake(#[from] WakeError),
    #[error(transparent)]
    Fixture(#[from] pingers_testkit::FixtureError),
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    #[error(transparent)]
    Gate(#[from] pingers_gate::GateError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error("missing: {0}")]
    Missing(&'static str),
}

/// Member `index`'s account key, the same every time it is asked for.
fn member_key(index: u8) -> AccountSigningKey {
    AccountSigningKey::generate(&mut seeded_rng(10_u64.saturating_add(u64::from(index))))
}

/// The origin of the server the town's Party lives on.
const TOWN_SERVER: &str = "https://town.example";

/// A server origin for a test.
fn server_id(origin: &str) -> Result<ServerId, TestError> {
    Ok(ServerId::new(String::from(origin))?)
}

/// A Party with three Spots north of each other, two members, and the server they share.
struct Town {
    party: PartyFixture,
    seed: [u8; 32],
    server: ServerId,
    world: Arc<Mutex<World>>,
    clock: ManualClock,
}

impl Town {
    fn new() -> Result<Self, TestError> {
        let mut rng = seeded_rng(1);
        let party = PartyBuilder::new()
            .members(2)
            .circle_spot(
                "anchor",
                Coordinate::new(51.5007, -0.1246)?,
                Meters::from_whole(150),
            )?
            .circle_spot(
                "bakery",
                Coordinate::new(51.5107, -0.1246)?,
                Meters::from_whole(150),
            )?
            .circle_spot(
                "canal",
                Coordinate::new(51.5507, -0.1246)?,
                Meters::from_whole(150),
            )?
            .build(&mut rng)?;
        Ok(Self {
            party,
            seed: random_array(&mut rng),
            server: server_id(TOWN_SERVER)?,
            world: World::shared(),
            clock: test_clock(),
        })
    }

    fn spot(&self, name: &str) -> Result<&SpotFixture, TestError> {
        Ok(self.party.spot(name)?)
    }

    fn spot_ref(&self, name: &str) -> Result<SpotRef, TestError> {
        Ok(SpotRef {
            party: self.party.id,
            spot: self.spot(name)?.id,
        })
    }

    fn now(&self) -> UnixSeconds {
        Clock::now(&self.clock)
    }

    fn advance(&self, duration: Duration) {
        self.clock.advance(duration);
    }

    /// The epoch every phone starts at, entered long enough ago that no grace period runs.
    fn settled_epoch(&self) -> EpochState {
        EpochState {
            current: EPOCH,
            entered_at: UnixSeconds::new(self.now().get().saturating_sub(7_200)),
            transition: EpochTransition::Other,
        }
    }

    /// The phone of member `index` (0 is alice, 1 is bob).
    fn phone(&self, index: u8) -> Result<Phone, TestError> {
        let member = self
            .party
            .members
            .get(usize::from(index))
            .ok_or(TestError::Missing("member"))?;
        let roster = (0_u8..)
            .zip(&self.party.members)
            .map(|(position, member)| RosterMember {
                leaf: member.leaf,
                key: member_key(position).public_key(),
            })
            .collect();
        let info = PartyInfo {
            party: self.party.id,
            server: self.server.clone(),
            epoch: self.settled_epoch(),
            own_leaf: member.leaf,
            knock_class: SizeClass::Bytes1024,
            spots: self
                .party
                .spots
                .iter()
                .map(|spot| SpotInfo {
                    spot: spot.id,
                    fence: spot.fence.clone(),
                })
                .collect(),
        };
        Ok(Phone {
            home: self.server.clone(),
            platform: FakePlatform::new(self.clock.clone()),
            parties: FakeParties::new(info, self.seed, roster, member_key(index)),
            server: FakeServer::new(
                self.server.clone(),
                Arc::clone(&self.world),
                self.clock.clone(),
                index.saturating_add(1),
            ),
            meetups: FakeMeetups::default(),
            state: WakeState::new(),
            rng: seeded_rng(100_u64.saturating_add(u64::from(index))),
            clock: self.clock.clone(),
            settings: settings()?,
            budget: BUDGET,
        })
    }
}

fn settings() -> Result<WakeSettings, TestError> {
    Ok(WakeSettings {
        location_sharing: LocationSharing::On,
        pause: PauseState::NotPaused,
        quiet_hours: QuietHoursSetting::Off,
        local_time: MinuteOfDay::new(12 * 60)?,
        zones: Vec::new(),
        data_mode: DataMode::Normal,
        cover_read_ratio: CoverReadRatio::ALWAYS,
        mailbox_reads: MailboxReads::OnPush,
        geofence_capacity: GeofenceCapacity::ANDROID,
    })
}

/// One phone: the fakes its ports run on, and its pipeline state.
struct Phone {
    /// The town server.
    home: ServerId,
    platform: FakePlatform,
    parties: FakeParties,
    server: FakeServer,
    meetups: FakeMeetups,
    state: WakeState,
    rng: TestRng,
    clock: ManualClock,
    settings: WakeSettings,
    budget: Duration,
}

impl Phone {
    async fn wake(&mut self, trigger: WakeTrigger) -> Result<WakeReport, TestError> {
        let env = WakeEnv {
            device: &self.platform,
            parties: &self.parties,
            server: &self.server,
            meetups: &self.meetups,
            time: &self.clock,
        };
        let input = WakeInput {
            trigger,
            settings: self.settings.clone(),
            budget: self.budget,
        };
        Ok(self.state.run(&env, input, &mut self.rng).await?)
    }

    fn now(&self) -> UnixSeconds {
        Clock::now(&self.clock)
    }

    /// The OS reports entering the Spot with a fix inside; 30 s later a second fix inside
    /// confirms the entry (design F.2). Returns both wakes' reports.
    async fn enter(&mut self, spot: &SpotFixture, at: SpotRef) -> Result<Entry, TestError> {
        self.platform.script_fix(spot.fix_inside(self.now()));
        let candidate = self
            .wake(WakeTrigger::Geofence(GeofenceEvent {
                region: RegionId::Spot(at),
                transition: GeofenceTransition::Enter,
                at: self.now(),
            }))
            .await?;
        self.clock
            .advance(pingers_geo::SpotPresence::ENTRY_FIX_SPACING);
        self.platform.script_fix(spot.fix_inside(self.now()));
        let confirmed = self.wake(WakeTrigger::Scheduled).await?;
        Ok(Entry {
            candidate,
            confirmed,
        })
    }

    /// What this phone remembers of the town server's board (empty if it has no board there).
    fn board(&self) -> BoardSnapshot {
        self.board_on(&self.home)
    }

    /// What this phone remembers of `server`'s board (empty if it has no board there).
    fn board_on(&self, server: &ServerId) -> BoardSnapshot {
        self.state
            .snapshot()
            .boards
            .into_iter()
            .find(|board| board.server == *server)
            .unwrap_or_else(|| BoardSnapshot {
                server: server.clone(),
                leases: Vec::new(),
                sent_knocks: Vec::new(),
                decided_knocks: Vec::new(),
                last_upload: None,
            })
    }

    /// The real records in this phone's newest live lease on the town server.
    fn real_records(&self) -> usize {
        self.board().leases.last().map_or(0, |lease| {
            lease
                .slots
                .iter()
                .filter(|slot| slot.real.is_some())
                .count()
        })
    }
}

/// The two wakes of an entry.
struct Entry {
    candidate: WakeReport,
    confirmed: WakeReport,
}
