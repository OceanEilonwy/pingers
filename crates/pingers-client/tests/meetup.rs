// SPDX-License-Identifier: AGPL-3.0-only
//! A Meetup end to end through two real clients against the in-process harness
//! (pingers-server, the OHTTP relay, pingers-push-relay): both are in a Party and enter a
//! Spot; a Meetup forms through the board, a knock and a Welcome; they chat over the live
//! channel; one approves the other's request for its exact location and its position frames
//! arrive; both leave, the Meetup ends and its keys are deleted on both phones.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::future::Future;
use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_client::ports::{JoinDecision, PartyDraft, SpotDraft};
use pingers_client::snapshot::{
    ChatEntry, ClientSnapshot, Meetup, Name, PartyColour, PeerAccess, PostQuantum, WakePushes,
};
use pingers_client::{Client, ClientConfig, ClientError, Crates, Instance, Outside, Trust};
use pingers_geo::{Circle, Coordinate, Fence, Fix, Meters};
use pingers_meetup::{AllowListChoice, AskBack, GrantScope, LocationAnswer};
use pingers_proto::{
    ChatBody, GroupId, LeafIndex, PartyId, ProtoError, PushPlatform, PushToken, ServerId,
};
use pingers_store::{Database, DatabaseKey, StoreError};
use pingers_testkit::{
    EventKind, FakePlatform, HarnessConfig, InProcessHarness, LocationMode, PushRegistration,
    RecordedEvent, RecordingEvents,
};
use pingers_time::{Clock as _, ManualClock};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use tokio::sync::watch;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Harness(#[from] pingers_testkit::HarnessError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Snapshot(#[from] pingers_client::snapshot::SnapshotError),
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    #[error("missing: {0}")]
    Missing(&'static str),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

type TestResult = Result<(), TestError>;

const KEY: [u8; 32] = [4; 32];

/// How long real time may pass while a step crosses the network.
const PATIENCE: Duration = Duration::from_secs(30);

/// Where the Spot is, and a point inside it.
const SPOT_LATITUDE: f64 = 51.5007;
const SPOT_LONGITUDE: f64 = -0.1246;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pingers-client-meetup-{}-{unique}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.0));
    }
}

/// One person's phone: a client on the real crates that registers its own account.
struct Phone {
    client: Client,
    platform: FakePlatform,
    events: RecordingEvents,
    database: PathBuf,
    clock: ManualClock,
    pushes: Pushes,
    _dir: TestDir,
}

/// Whether a phone turns wake-up pushes on, and its UnifiedPush endpoint if so.
#[derive(Clone, Copy)]
enum Pushes {
    Off,
    On(&'static str),
}

/// One phone of the scenario: its random streams come from `seed`.
#[derive(Clone, Copy)]
struct Who {
    seed: u64,
    pushes: Pushes,
}

impl Phone {
    async fn new(
        harness: &InProcessHarness,
        config: &HarnessConfig,
        who: Who,
    ) -> Result<Self, TestError> {
        let Who { seed, pushes } = who;
        let dir = TestDir::new()?;
        let endpoints = harness.endpoints();
        let database = dir.0.join("pingers.db");
        let instance = Instance {
            server: ServerId::new(endpoints.server_id.clone())?,
            relay: format!("http://{}/ohttp", endpoints.ohttp_relay),
            push_relay: Some(format!("https://{}", endpoints.push_relay)),
            gateway_pins: None,
            push_relay_pins: None,
            trust: Trust::from_pem(&endpoints.push_relay_ca)?,
            registration_code: None,
        };
        let clock = config.clock.clone();
        let platform = FakePlatform::new(clock.clone());
        platform
            .secure_store_put("pingers.database-key", &KEY)
            .map_err(|_locked| TestError::Missing("secure store"))?;
        // Each person draws from their own stream, or two would make the same ids.
        let seeds = Arc::new(AtomicU64::new(
            500_u64.saturating_add(seed.saturating_mul(100_000)),
        ));
        let outside = Outside {
            clock: Arc::new(clock.clone()),
            timer: Arc::new(clock.clone()),
            rng: Arc::new(move || {
                Box::new(ChaCha20Rng::seed_from_u64(
                    seeds.fetch_add(1, Ordering::Relaxed),
                ))
            }),
        };
        let events = RecordingEvents::new();
        let crates_platform: Arc<dyn pingers_client::Platform> = Arc::new(platform.clone());
        let opened = instance.clone();
        let client = Client::open_with(
            ClientConfig {
                database: database.clone(),
                instance: Some(instance),
            },
            Arc::new(platform.clone()),
            Arc::new(events.clone()),
            move |database| Crates::real(database, crates_platform, outside, Some(&opened)),
        )
        .await?;
        match pushes {
            // Every wake reads the mailboxes a step waits on either way.
            Pushes::Off => client.set_wake_pushes(WakePushes::NoWakePushes).await?,
            Pushes::On(endpoint) => {
                platform.set_push_registration(Some(PushRegistration {
                    platform: PushPlatform::UnifiedPush,
                    token: PushToken::from_bytes(endpoint.as_bytes().to_vec()),
                }));
                client.set_wake_pushes(WakePushes::On).await?;
            }
        }
        Ok(Self {
            client,
            platform,
            events,
            database,
            clock,
            pushes,
            _dir: dir,
        })
    }

    /// A wake that reads the mailboxes: a scheduled one with wake-up pushes off; with them
    /// on, the push the harness relay sent (its handle is not captured, so the client reads
    /// it as one that does not say what it is for, and reads every mailbox).
    async fn wake(&self) -> TestResult {
        match self.pushes {
            Pushes::Off => self.client.on_wake().await?,
            Pushes::On(_) => {
                self.client
                    .on_push(pingers_proto::PushHandle::from_bytes([0; 8]))
                    .await?;
            }
        }
        Ok(())
    }

    /// A fix in the Spot, taken now.
    async fn at_the_spot(&self) -> TestResult {
        self.at(SPOT_LATITUDE).await
    }

    /// A fix at `latitude` on the Spot's meridian, taken now.
    async fn at(&self, latitude: f64) -> TestResult {
        let fix = Fix {
            coordinate: Coordinate::new(latitude, SPOT_LONGITUDE)?,
            accuracy: Meters::new(8.0)?,
            taken_at: self.clock.now(),
        };
        self.client.on_location(fix).await?;
        Ok(())
    }

    /// Waits, in real time, until the published snapshot satisfies `check`.
    async fn until(
        &self,
        what: &'static str,
        mut check: impl FnMut(&ClientSnapshot) -> bool,
    ) -> Result<Arc<ClientSnapshot>, TestError> {
        let mut snapshots: watch::Receiver<Arc<ClientSnapshot>> = self.client.subscribe();
        within(what, async move {
            loop {
                let snapshot = Arc::clone(&snapshots.borrow_and_update());
                if check(&snapshot) {
                    return Ok(snapshot);
                }
                snapshots
                    .changed()
                    .await
                    .map_err(|_closed| TestError::Missing(what))?;
            }
        })
        .await
    }

    /// The Meetup rows left in this phone's store.
    fn meetup_rows(&self) -> Result<usize, TestError> {
        let database =
            Database::open(&self.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        Ok(database.session()?.meetups().list()?.len())
    }
}

async fn within<T>(
    what: &'static str,
    step: impl Future<Output = Result<T, TestError>>,
) -> Result<T, TestError> {
    tokio::time::timeout(PATIENCE, step)
        .await
        .map_err(|_elapsed| TestError::Timeout(what))?
}

/// Ana makes a Party and lets Ben in through an invite.
async fn party_of_two(ana: &Phone, ben: &Phone) -> Result<PartyId, TestError> {
    let party = ana
        .client
        .create_party(PartyDraft {
            name: Name::new("Climbing crew")?,
            my_name: Name::new("Ana")?,
            colour: PartyColour::Teal,
            post_quantum: PostQuantum::Off,
        })
        .await?;
    let link = ana.client.create_invite(party).await?;
    ben.client.open_invite(link.url, Name::new("Ben")?).await?;
    ana.wake().await?;
    let snapshot = ana.client.snapshot();
    let request = snapshot
        .party(party)?
        .join_requests
        .first()
        .ok_or(TestError::Missing("Ben's join request"))?
        .id;
    ana.client
        .decide_join(party, request, JoinDecision::Admit)
        .await?;
    ben.wake().await?;
    ben.client.snapshot().party(party)?;
    Ok(party)
}

/// Ana adds a Spot to the Party; Ben learns of it from the Party log on his next wake.
async fn spot(ana: &Phone, party: PartyId) -> TestResult {
    ana.client
        .save_spot(
            party,
            SpotDraft {
                id: None,
                name: Name::new("The crag")?,
                fence: Fence::Circle(Circle::new(
                    Coordinate::new(SPOT_LATITUDE, SPOT_LONGITUDE)?,
                    Meters::new(150.0)?,
                )?),
            },
        )
        .await?;
    Ok(())
}

/// Both phones in one Meetup, with their leaves.
struct Meeting<'phones> {
    ana: &'phones Phone,
    ben: &'phones Phone,
    meetup: GroupId,
    ana_leaf: LeafIndex,
    ben_leaf: LeafIndex,
}

/// The other members' names, as a phone shows them.
fn names(shown: &Meetup) -> Vec<String> {
    shown
        .others
        .iter()
        .map(|peer| peer.name.as_str().to_owned())
        .collect()
}

/// Both phones report fixes in the Spot and wake, half a minute apart, until each is in the
/// Meetup: entry needs two fixes, the pools fill one mailbox per wake, the board shows the
/// other's record, a knock goes out and is answered, and the Welcome is read.
async fn meet<'phones>(
    ana: &'phones Phone,
    ben: &'phones Phone,
) -> Result<Meeting<'phones>, TestError> {
    for _round in 0..40 {
        ana.clock.advance(Duration::from_secs(30));
        for phone in [ana, ben] {
            phone.at_the_spot().await?;
            phone.wake().await?;
        }
        let ana_in = ana.client.snapshot().meetups.first().cloned();
        let ben_in = ben.client.snapshot().meetups.first().cloned();
        if let (Some(ana_in), Some(ben_in)) = (ana_in, ben_in) {
            assert_eq!(ana_in.id, ben_in.id);
            assert_eq!(names(&ana_in), ["Ben"]);
            assert_eq!(names(&ben_in), ["Ana"]);
            return Ok(Meeting {
                ana,
                ben,
                meetup: ana_in.id,
                ana_leaf: ana_in.me.leaf,
                ben_leaf: ben_in.me.leaf,
            });
        }
    }
    Err(TestError::Missing("a Meetup on both phones"))
}

impl Meeting<'_> {
    /// Both shells heard of the Meetup, run the Meetup location service and show the alert.
    fn started(&self) {
        let meetup = self.meetup;
        for phone in [self.ana, self.ben] {
            let started = RecordedEvent::MeetupStarted { meetup };
            assert!(phone.events.events().contains(&started));
            assert_eq!(
                phone.platform.location_mode(),
                LocationMode::MeetupBackground
            );
            assert_eq!(phone.client.snapshot().alert, Some(meetup));
        }
    }

    /// Ana writes; Ben's chat shows it, with the event that refreshes his screen.
    async fn chat(&self) -> TestResult {
        let meetup = self.meetup;
        let body = ChatBody::new("On the left wall".to_owned())?;
        self.ana.client.send_chat(meetup, body, None).await?;
        let seen = self
            .ben
            .until("Ana's message on Ben's phone", |snapshot| {
                snapshot
                    .meetup(meetup)
                    .is_ok_and(|shown| !shown.chat.is_empty())
            })
            .await?;
        let message = seen
            .meetup(meetup)?
            .chat
            .iter()
            .find_map(|entry| match entry {
                ChatEntry::Message(message) => Some(message.clone()),
                ChatEntry::Joined { .. } | ChatEntry::MeetPointSet { .. } => None,
            });
        let message = message.ok_or(TestError::Missing("the message"))?;
        assert_eq!(message.author, self.ana_leaf);
        assert_eq!(message.body.as_str(), "On the left wall");
        assert!(self.ben.events.count(EventKind::MeetupMessage) >= 1);
        Ok(())
    }

    /// Ben asks to see Ana's exact spot; Ana's phone raises the request and she allows it.
    async fn ask_and_allow(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        let sent = ben.client.ask_location(meetup, self.ana_leaf).await?;
        let asked = ana
            .until("Ben's request on Ana's phone", |snapshot| {
                !snapshot.location_requests.is_empty()
            })
            .await?;
        let request = asked
            .location_requests
            .first()
            .ok_or(TestError::Missing("the request"))?;
        assert_eq!((request.id, request.from), (sent, self.ben_leaf));
        let raised = RecordedEvent::LocationRequest {
            meetup,
            from: self.ben_leaf,
            request: sent,
        };
        assert!(ana.events.events().contains(&raised));
        let allow = LocationAnswer::Allow {
            scope: GrantScope::ThisMeetup,
            ask_back: AskBack::DoNotAskBack,
            always: AllowListChoice::JustThisTime,
        };
        ana.client
            .answer_location_request(meetup, request.id, allow)
            .await?;
        ben.until("access to Ana", |_snapshot| {
            ben.events.count(EventKind::LocationAccessChanged) >= 1
        })
        .await?;
        Ok(())
    }

    /// Location frames: Ana's next background frame carries her fix, and Ben sees her pin.
    async fn frames(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        let shared = |snapshot: &ClientSnapshot| {
            snapshot.meetup(meetup).is_ok_and(|shown| {
                shown
                    .others
                    .iter()
                    .any(|peer| matches!(peer.access, PeerAccess::Shared(_)))
            })
        };
        for _frame in 0..10 {
            ana.at_the_spot().await?;
            ben.at_the_spot().await?;
            ana.clock.advance(Duration::from_secs(61));
            let shown =
                tokio::time::timeout(Duration::from_secs(3), ben.until("Ana's position", shared))
                    .await;
            if matches!(shown, Ok(Ok(_))) {
                let moved = RecordedEvent::PeerLocation {
                    meetup,
                    peer: self.ana_leaf,
                };
                assert!(ben.events.events().contains(&moved));
                let sharing = ana.client.snapshot().meetup(meetup)?.sharing_with.clone();
                assert_eq!(sharing, vec![self.ben_leaf]);
                return Ok(());
            }
        }
        Err(TestError::Missing("Ana's position on Ben's phone"))
    }

    /// Ana taps Leave: her keys go at once and Ben sees her go; then Ben, alone, walks out of
    /// the Spot, his phone leaves once the exit is confirmed, and the Meetup is over on both.
    async fn both_leave(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        ana.client.leave_meetup(meetup).await?;
        ben.until("Ana gone from Ben's Meetup", |snapshot| {
            snapshot
                .meetup(meetup)
                .is_ok_and(|shown| shown.others.is_empty())
        })
        .await?;
        for _fix in 0..10 {
            if ben.client.snapshot().meetups.is_empty() {
                break;
            }
            ben.clock.advance(Duration::from_secs(30));
            ben.at(SPOT_LATITUDE + 0.01).await?;
        }
        // Ana is still in the Spot, dwelling; Ben is out of it, idle.
        for (phone, mode) in [(ana, LocationMode::Dwell), (ben, LocationMode::Idle)] {
            let snapshot = phone.client.snapshot();
            assert!(snapshot.meetups.is_empty());
            assert_eq!(snapshot.alert, None);
            assert_eq!(phone.meetup_rows()?, 0);
            let ended = RecordedEvent::MeetupEnded { meetup };
            assert!(phone.events.events().contains(&ended));
            assert_eq!(phone.platform.location_mode(), mode);
            let summary = snapshot
                .ended_meetup
                .clone()
                .ok_or(TestError::Missing("the summary"))?;
            assert_eq!(summary.spot_name.as_str(), "The crag");
            assert_eq!((summary.people, summary.messages), (2, 1));
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_meet_chat_share_a_location_and_end_the_meetup() -> TestResult {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let ana = Phone::new(
        &harness,
        &config,
        Who {
            seed: 3,
            pushes: Pushes::Off,
        },
    )
    .await?;
    let ben = Phone::new(
        &harness,
        &config,
        Who {
            seed: 4,
            pushes: Pushes::Off,
        },
    )
    .await?;
    let party = party_of_two(&ana, &ben).await?;
    spot(&ana, party).await?;
    let meeting = meet(&ana, &ben).await?;
    meeting.started();
    meeting.chat().await?;
    meeting.ask_and_allow().await?;
    meeting.frames().await?;
    meeting.both_leave().await
}

const ANA_PUSH: &str = "https://push.harness.invalid/up/ana";
const BEN_PUSH: &str = "https://push.harness.invalid/up/ben";

/// Whether the harness's push relay sent `endpoint` a push within `window` of real time.
async fn pushed_within(
    harness: &InProcessHarness,
    endpoint: &str,
    already: usize,
    window: Duration,
) -> bool {
    let token = PushToken::from_bytes(endpoint.as_bytes().to_vec());
    let count = || {
        harness
            .pushes()
            .iter()
            .filter(|push| push.token == token)
            .count()
    };
    let mut ticks = tokio::time::interval(Duration::from_millis(50));
    tokio::time::timeout(window, async {
        loop {
            ticks.tick().await;
            if count() > already {
                return;
            }
        }
    })
    .await
    .is_ok()
}

/// How many pushes `endpoint` has had.
fn pushes_to(harness: &InProcessHarness, endpoint: &str) -> usize {
    let token = PushToken::from_bytes(endpoint.as_bytes().to_vec());
    harness
        .pushes()
        .iter()
        .filter(|push| push.token == token)
        .count()
}

/// Ben's phone dies in the Meetup. While he is a member, Ana's chat wakes him through his
/// subscription; once her phone removes him as silent, its commit names his subscription
/// (read from his leaf) and the real server stops waking him at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_removed_as_silent_is_no_longer_woken() -> TestResult {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let ana = Phone::new(
        &harness,
        &config,
        Who {
            seed: 5,
            pushes: Pushes::On(ANA_PUSH),
        },
    )
    .await?;
    let ben = Phone::new(
        &harness,
        &config,
        Who {
            seed: 6,
            pushes: Pushes::On(BEN_PUSH),
        },
    )
    .await?;
    let party = party_of_two(&ana, &ben).await?;
    spot(&ana, party).await?;
    let meetup = meet(&ana, &ben).await?.meetup;
    drop(ben);

    // Still a member: Ana's message wakes Ben's phone.
    let before = pushes_to(&harness, BEN_PUSH);
    ana.client
        .send_chat(meetup, ChatBody::new("Still there?".to_owned())?, None)
        .await?;
    assert!(pushed_within(&harness, BEN_PUSH, before, PATIENCE).await);

    // Ten minutes without a frame from him: Ana's phone removes him.
    for _minute in 0..15 {
        if ana
            .client
            .snapshot()
            .meetup(meetup)
            .is_ok_and(|shown| shown.others.is_empty())
        {
            break;
        }
        ana.clock.advance(Duration::from_secs(60));
        ana.at_the_spot().await?;
        let _settled = tokio::time::timeout(
            Duration::from_secs(1),
            ana.until("Ben removed", |snapshot| {
                snapshot
                    .meetup(meetup)
                    .is_ok_and(|shown| shown.others.is_empty())
            }),
        )
        .await;
    }
    let shown = ana.client.snapshot();
    assert!(shown.meetup(meetup)?.others.is_empty());

    // No longer a member: the server dropped his subscription with the removal.
    let before = pushes_to(&harness, BEN_PUSH);
    ana.client
        .send_chat(meetup, ChatBody::new("Gone, then".to_owned())?, None)
        .await?;
    assert!(!pushed_within(&harness, BEN_PUSH, before, Duration::from_secs(3)).await);
    Ok(())
}
