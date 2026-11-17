// SPDX-License-Identifier: AGPL-3.0-only
//! Two and three simulated members, each with its own encrypted store, over a fake server
//! (group logs and live fan-out) and fake live channels.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod catch_up;
mod psk;
mod scenarios;
mod server;
mod units;

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot};
use pingers_geo::{Coordinate, Fix, Meters, PresenceState};
use pingers_meetup::MeetupEvent;
use pingers_mls::{CipherSuitePolicy, MlsContext, MlsError};
use pingers_presence::{ActiveSpot, MeetupKey, SpotRef, SpotStatus};
use pingers_proto::{AccountId, Epoch, GroupId, LeafIndex, PartyId, ServerId, SpotId};
use pingers_store::{Database, DatabaseKey, Identity, MemberName, Party, PartyName, Verification};
use pingers_time::{Bucket, ManualClock, UnixSeconds};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::{
    CreateMeetup, JoinMeetup, MeetupHandle, MeetupView, Runtime, SessionError, create, join,
    knock_key_package,
};
use server::{FakeNetwork, FakeServer};

/// Why a test failed before reaching an assertion.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Store(#[from] pingers_store::StoreError),
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    #[error(transparent)]
    Meetup(#[from] pingers_meetup::MeetupError),
    #[error(transparent)]
    Time(#[from] pingers_time::TimeError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Presence(#[from] pingers_presence::PresenceError),
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("expected {0}")]
    Missing(&'static str),
}

pub(crate) type TestResult = Result<(), TestError>;

/// How long a test waits for something before failing instead of hanging. Real time: the
/// Meetup's own clocks are the manual clock.
const PATIENCE: Duration = Duration::from_secs(30);

const START: UnixSeconds = UnixSeconds::new(1_800_000_000);
const GROUP: GroupId = GroupId::from_bytes([0x47; 32]);

/// A temporary directory, removed on drop.
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-meetup-session-test-{}-{unique}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

/// One simulated phone: its store and its view of the network.
pub(crate) struct Device {
    pub(crate) key: AccountPublicKey,
    pub(crate) runtime: Runtime,
    // Field order: the database closes before its directory is removed.
    directory: TestDir,
}

impl Device {
    pub(crate) fn database(&self) -> &Arc<Database> {
        &self.runtime.database
    }

    /// Overwrites the store's file, as a failing disk would: every later session fails.
    pub(crate) fn break_store(&self) -> Result<(), TestError> {
        Ok(break_store_at(&self.store_file())?)
    }

    /// The store's file, for breaking it from inside a step.
    pub(crate) fn store_file(&self) -> PathBuf {
        self.directory.path.join("store.db")
    }

    /// The store's view of the Meetup: whether anything of it is left.
    pub(crate) fn leftovers(&self, group: &GroupId) -> Result<Leftovers, TestError> {
        let session = self.database().session()?;
        let signer = session
            .identity()
            .load()?
            .ok_or(TestError::Missing("an identity"))?
            .signing_key;
        let mls_group = match MlsContext::new(&session, &signer).meetup_group(group) {
            Ok(_group) => true,
            Err(MlsError::NotAMember) => false,
            Err(error) => return Err(SessionError::Mls(error).into()),
        };
        Ok(Leftovers {
            meetups: session.meetups().list()?.len(),
            chat: session.meetup_messages().list(group)?.len(),
            peers: session.meetup_peers().list(group)?.len(),
            self_keys: session.self_location_keys().current(group)?.is_some(),
            mls_group,
        })
    }

    pub(crate) fn chat_rows(&self, group: &GroupId) -> Result<usize, TestError> {
        Ok(self
            .database()
            .session()?
            .meetup_messages()
            .list(group)?
            .len())
    }
}

/// What the store still holds of a Meetup.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Leftovers {
    pub(crate) meetups: usize,
    pub(crate) chat: usize,
    pub(crate) peers: usize,
    pub(crate) self_keys: bool,
    pub(crate) mls_group: bool,
}

impl Leftovers {
    pub(crate) const NOTHING: Self = Self {
        meetups: 0,
        chat: 0,
        peers: 0,
        self_keys: false,
        mls_group: false,
    };
}

/// A Party of devices at leaves 0, 1, ... sharing one manual clock and one fake server.
pub(crate) struct World {
    pub(crate) clock: ManualClock,
    pub(crate) server: Arc<FakeServer>,
    pub(crate) party: PartyId,
    pub(crate) spot: SpotId,
    pub(crate) devices: Vec<Device>,
    /// The board segments each device says it saw; by default all saw the same one.
    pub(crate) boards: std::sync::Mutex<Vec<Vec<pingers_proto::SeenSegment>>>,
}

const NAMES: [&str; 3] = ["Alice", "Bob", "Chloé"];

impl World {
    pub(crate) fn new(count: usize) -> Result<Self, TestError> {
        let clock = ManualClock::new(START);
        let server = Arc::new(FakeServer::default());
        let party = PartyId::from_bytes([0x11; 16]);
        let mut rng = pingers_testkit::seeded_rng(100);
        let keys: Vec<AccountSigningKey> = (0..count)
            .map(|_| AccountSigningKey::generate(&mut rng))
            .collect();
        let mut devices = Vec::new();
        for (index, (name, key)) in NAMES.iter().zip(&keys).enumerate() {
            let directory = TestDir::new()?;
            let fill = u8::try_from(index).unwrap_or(0).saturating_add(1);
            let database = Database::open(
                &directory.path.join("store.db"),
                DatabaseKey::from_bytes(Zeroizing::new([fill; 32])),
            )?;
            let own_leaf = LeafIndex::new(u32::try_from(index).unwrap_or(0));
            {
                let session = database.session()?;
                session.identity().save(&Identity {
                    account: AccountId::from_bytes([fill; 16]),
                    signing_key: AccountSigningKey::from_bytes(Zeroizing::new(*key.expose_bytes())),
                })?;
                let server = ServerId::new("https://pingers.example".into())?;
                session.servers().put(&pingers_store::ServerRecord {
                    server: server.clone(),
                    ohttp_relays: Vec::new(),
                    push_relay: None,
                    gateway_pins: Vec::new(),
                    push_relay_pins: Vec::new(),
                    map_host_pins: Vec::new(),
                    trust: pingers_store::ServerTrust::BuiltIn,
                })?;
                session.parties().insert(&Party {
                    id: party,
                    group: GroupId::from_bytes([0x22; 32]),
                    name: PartyName::new("Friends".into())?,
                    own_leaf,
                    created_at: START,
                    location_visibility: None,
                    server,
                })?;
                for (other_index, (other_name, other)) in NAMES.iter().zip(&keys).enumerate() {
                    if other_index != index {
                        session.members().put(&pingers_store::Member {
                            party,
                            leaf: LeafIndex::new(u32::try_from(other_index).unwrap_or(0)),
                            signature_key: other.public_key(),
                            name: MemberName::new((*other_name).to_owned())?,
                            avatar: None,
                            verification: Verification::Unverified,
                        })?;
                    }
                }
            }
            let network = Arc::new(FakeNetwork::new(Arc::clone(&server), name));
            devices.push(Device {
                key: key.public_key(),
                runtime: Runtime {
                    database: Arc::new(database),
                    network,
                    clock: Arc::new(clock.clone()),
                    timer: Arc::new(clock.clone()),
                },
                directory,
            });
        }
        Ok(Self {
            clock,
            server,
            party,
            spot: SpotId::from_bytes([0x33; 16]),
            boards: std::sync::Mutex::new(vec![common_board(); count]),
            devices,
        })
    }

    pub(crate) fn device(&self, index: usize) -> Result<&Device, TestError> {
        self.devices
            .get(index)
            .ok_or(TestError::Missing("a device"))
    }

    pub(crate) fn now(&self) -> UnixSeconds {
        pingers_time::Clock::now(&self.clock)
    }

    /// The Spot as a device `Inside` it sees it at the Party's epoch 1.
    pub(crate) fn status(&self) -> SpotStatus {
        SpotStatus {
            spot: SpotRef {
                party: self.party,
                spot: self.spot,
            },
            epoch: Epoch::new(1),
            state: PresenceState::Inside,
        }
    }

    /// `PSK_meet(b)` for the record the knock answered, as each device derives it on its own
    /// from the Party's presence root while `Inside` the Spot (design B.4.1, B.5).
    pub(crate) fn meetup_key(&self) -> Result<MeetupKey, TestError> {
        key_for(&self.status(), record_start()?)
    }

    /// The board segments device `index` says it saw.
    pub(crate) fn board_of(&self, index: usize) -> Vec<pingers_proto::SeenSegment> {
        self.boards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(index)
            .cloned()
            .unwrap_or_default()
    }

    /// Device `index` says it saw `board`.
    pub(crate) fn sees(&self, index: usize, board: Vec<pingers_proto::SeenSegment>) {
        if let Some(slot) = self
            .boards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(index)
        {
            *slot = board;
        }
    }

    /// Alice creates the Meetup with Bob, and Bob joins it; both channels are open.
    pub(crate) async fn meetup_of_two(&self) -> Result<[Live; 2], TestError> {
        let alice = self.device(0)?;
        let bob = self.device(1)?;
        let knock = knock_key_package(
            bob.database(),
            knocking(1),
            Box::new(pingers_testkit::seeded_rng(2)),
        )
        .await?;
        let created = create(
            &alice.runtime,
            CreateMeetup {
                key: self.meetup_key()?,
                group: GROUP,
                knocker: knock.key_package,
                subscriptions: Vec::new(),
                own_subscription: Some(subscription_of(0)),
                board_seen: self.board_of(0),
                suite: CipherSuitePolicy::Classical,
            },
            Box::new(pingers_testkit::seeded_rng(1)),
        )
        .await?;
        let alice_live = Live {
            handle: created.handle,
            events: created.events,
        };
        let bob_live = self.join(1, created.welcome, knock.grant_key).await?;
        alice_live.connected().await?;
        bob_live.connected().await?;
        Ok([alice_live, bob_live])
    }

    /// Device `index` joins with `welcome`.
    pub(crate) async fn join(
        &self,
        index: usize,
        welcome: pingers_proto::WelcomeBytes,
        grant_key: pingers_crypto::GrantSecretKey,
    ) -> Result<Live, TestError> {
        let key = self.meetup_key()?;
        self.join_with(index, Reply { welcome, grant_key }, key)
            .await
    }

    /// Device `index` tries to join with `welcome` under `key`.
    pub(crate) async fn join_with(
        &self,
        index: usize,
        reply: Reply,
        key: MeetupKey,
    ) -> Result<Live, TestError> {
        let Reply { welcome, grant_key } = reply;
        let joined = join(
            &self.device(index)?.runtime,
            JoinMeetup {
                key,
                welcome,
                grant_key,
                own_subscription: Some(subscription_of(index)),
                board_seen: self.board_of(index),
            },
            Box::new(pingers_testkit::seeded_rng(
                u64::try_from(index).unwrap_or(0).saturating_add(10),
            )),
        )
        .await?;
        Ok(Live {
            handle: joined.handle,
            events: joined.events,
        })
    }
}

/// What device `index` knocks with: the classical suite and its own subscription.
pub(crate) fn knocking(index: usize) -> crate::Knocking {
    crate::Knocking {
        suite: CipherSuitePolicy::Classical,
        subscription: Some(subscription_of(index)),
    }
}

/// Device `index`'s own subscription in the Meetup.
pub(crate) fn subscription_of(index: usize) -> pingers_proto::SubscriptionId {
    let fill = 0xa0_u8.saturating_add(u8::try_from(index).unwrap_or(0));
    pingers_proto::SubscriptionId::from_bytes([fill; 16])
}

/// What a joiner got back for its knock: the Welcome, and the grant secret of the KeyPackage
/// it knocked with.
pub(crate) struct Reply {
    pub(crate) welcome: pingers_proto::WelcomeBytes,
    pub(crate) grant_key: pingers_crypto::GrantSecretKey,
}

/// `PSK_meet` for any Spot status and record start bucket, derived through pingers-presence.
pub(crate) fn key_for(status: &SpotStatus, start: Bucket) -> Result<MeetupKey, TestError> {
    Ok(ActiveSpot::derive(&presence_root(), status)?.meetup_key(start)?)
}

/// The Party's presence root at epoch 1, which every member exports from the Party group.
pub(crate) fn presence_root() -> PresenceRoot {
    PresenceRoot::from_bytes(Zeroizing::new([0x99; 32]))
}

/// The start bucket of the record the knock answered.
pub(crate) fn record_start() -> Result<Bucket, TestError> {
    Ok(Bucket::from_index(2_000_000)?)
}

pub(crate) fn fix(at: UnixSeconds) -> Result<Fix, TestError> {
    Ok(Fix {
        coordinate: Coordinate::new(51.5007, -0.1246)?,
        accuracy: Meters::new(5.0)?,
        taken_at: at,
    })
}

/// A running Meetup on one device, with its events.
pub(crate) struct Live {
    pub(crate) handle: MeetupHandle,
    pub(crate) events: mpsc::UnboundedReceiver<MeetupEvent>,
}

impl Live {
    /// Waits until the view satisfies `condition`.
    pub(crate) async fn view_until(
        &self,
        what: &'static str,
        condition: impl FnMut(&MeetupView) -> bool,
    ) -> Result<MeetupView, TestError> {
        let mut view = self.handle.view();
        let result = tokio::time::timeout(PATIENCE, view.wait_for(condition))
            .await
            .map_err(|_| TestError::Timeout(what))?
            .map_err(|_| TestError::Missing(what))?;
        Ok(result.clone())
    }

    pub(crate) async fn connected(&self) -> Result<MeetupView, TestError> {
        self.view_until("the channel to open", |view| {
            view.channel == crate::ChannelState::Connected
        })
        .await
    }

    /// Waits for the next event matching `wanted`, dropping others.
    pub(crate) async fn event(
        &mut self,
        what: &'static str,
        mut wanted: impl FnMut(&MeetupEvent) -> bool,
    ) -> Result<MeetupEvent, TestError> {
        tokio::time::timeout(PATIENCE, async {
            while let Some(event) = self.events.recv().await {
                if wanted(&event) {
                    return Ok(event);
                }
            }
            Err(TestError::Missing(what))
        })
        .await
        .map_err(|_| TestError::Timeout(what))?
    }

    /// Every event received so far.
    pub(crate) fn drain(&mut self) -> Vec<MeetupEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }
}

/// The one closed board segment every device saw unless a test says otherwise.
pub(crate) fn common_board() -> Vec<pingers_proto::SeenSegment> {
    vec![pingers_proto::SeenSegment {
        start: Bucket::containing(START),
        hash: pingers_proto::SegmentHash::from_bytes([0x5e; 32]),
    }]
}

/// Overwrites the store file at `path`, as a failing disk would.
pub(crate) fn break_store_at(path: &std::path::Path) -> Result<(), std::io::Error> {
    std::fs::write(path, [0_u8; 4096])
}
