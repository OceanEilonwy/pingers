// SPDX-License-Identifier: AGPL-3.0-only
//! Two and three simulated members, each with its own encrypted store, over a fake server
//! (group logs and live fan-out) and fake live channels.

#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

mod catch_up;
mod scenarios;
mod server;
mod units;

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_crypto::{AccountPublicKey, AccountSigningKey, MeetupPsk};
use pingers_geo::{Coordinate, Fix, Meters};
use pingers_meetup::MeetupEvent;
use pingers_mls::{CipherSuitePolicy, MeetupPskId, MlsContext, MlsError};
use pingers_proto::{
    AccountId, Epoch, GroupId, KeyConfigHash, LeafIndex, PartyId, ServerId, SpotId,
};
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
    _directory: TestDir,
}

impl Device {
    pub(crate) fn database(&self) -> &Arc<Database> {
        &self.runtime.database
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
                    server: ServerId::new("https://pingers.example".into())?,
                    server_pin: KeyConfigHash::from_bytes([5; 32]),
                })?;
                session.parties().insert(&Party {
                    id: party,
                    group: GroupId::from_bytes([0x22; 32]),
                    name: PartyName::new("Friends".into())?,
                    own_leaf,
                    created_at: START,
                    location_visibility: None,
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
                    transport: Arc::<FakeNetwork>::clone(&network),
                    network,
                    clock: Arc::new(clock.clone()),
                    timer: Arc::new(clock.clone()),
                },
                _directory: directory,
            });
        }
        Ok(Self {
            clock,
            server,
            party,
            spot: SpotId::from_bytes([0x33; 16]),
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

    fn psk_id(&self) -> Result<MeetupPskId, TestError> {
        Ok(MeetupPskId {
            party: self.party,
            epoch: Epoch::new(1),
            spot: self.spot,
            bucket: Bucket::from_index(2_000_000)?,
        })
    }

    /// Alice creates the Meetup with Bob, and Bob joins it; both channels are open.
    pub(crate) async fn meetup_of_two(&self) -> Result<[Live; 2], TestError> {
        let alice = self.device(0)?;
        let bob = self.device(1)?;
        let knock = knock_key_package(
            bob.database(),
            CipherSuitePolicy::Classical,
            Box::new(pingers_testkit::seeded_rng(2)),
        )
        .await?;
        let created = create(
            &alice.runtime,
            CreateMeetup {
                party: self.party,
                spot: self.spot,
                group: GROUP,
                psk_id: self.psk_id()?,
                psk: psk(),
                knocker: knock.key_package,
                subscriptions: Vec::new(),
                own_subscription: None,
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
        let joined = join(
            &self.device(index)?.runtime,
            JoinMeetup {
                party: self.party,
                spot: self.spot,
                welcome,
                psk_id: self.psk_id()?,
                psk: psk(),
                grant_key,
                own_subscription: None,
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

fn psk() -> MeetupPsk {
    MeetupPsk::from_bytes(Zeroizing::new([0x99; 32]))
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
