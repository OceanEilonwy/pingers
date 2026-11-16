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

use pingers_client::ports::{Foreground, JoinDecision, PartyDraft, SpotDraft};
use pingers_client::snapshot::{
    ChatEntry, ChatMessage, ClientSnapshot, MeetPlace, Meetup, Name, PartyColour, PeerAccess,
    PostQuantum, WakePushes,
};
use pingers_client::{Client, ClientConfig, ClientError, Crates, Instance, Outside, Trust};
use pingers_geo::{Circle, Coordinate, Fence, Fix, Meters};
use pingers_meetup::{AllowListChoice, AskBack, GrantScope, LocationAnswer, MeetPointDraft};
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
    #[error(transparent)]
    Transport(#[from] pingers_transport::TransportError),
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
            prover_self_test: pingers_sync::SelfTestPlan::new(
                core::num::NonZeroU8::MIN,
                core::time::Duration::from_millis(1_500),
            ),
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

    /// Waits, in real time, until the shell has been told `kind` at least once. Events follow
    /// the snapshot that shows them, so a step that saw the snapshot waits for its event here.
    async fn heard(&self, what: &'static str, kind: EventKind) -> TestResult {
        let mut ticks = tokio::time::interval(Duration::from_millis(20));
        within(what, async move {
            while self.events.count(kind) == 0 {
                ticks.tick().await;
            }
            Ok(())
        })
        .await
    }

    /// Waits, in real time, until the shell has been told exactly `event`.
    async fn told(&self, what: &'static str, event: RecordedEvent) -> TestResult {
        let mut ticks = tokio::time::interval(Duration::from_millis(20));
        within(what, async move {
            while !self.events.events().contains(&event) {
                ticks.tick().await;
            }
            Ok(())
        })
        .await
    }

    /// Whether this phone's store records its own subscription in `group` (so a push for the
    /// group is recognised).
    fn subscribed(&self, group: GroupId) -> Result<bool, TestError> {
        let database =
            Database::open(&self.database, DatabaseKey::from_bytes(Zeroizing::new(KEY)))?;
        Ok(database.session()?.subscriptions().get(&group)?.is_some())
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
    let_in(ana, ben, party, "Ben").await?;
    Ok(party)
}

/// `inviter` lets `joiner` (going by `name`) into `party` through an invite.
async fn let_in(inviter: &Phone, joiner: &Phone, party: PartyId, name: &str) -> TestResult {
    let link = inviter.client.create_invite(party).await?;
    joiner
        .client
        .open_invite(link.url, Name::new(name)?)
        .await?;
    inviter.wake().await?;
    let snapshot = inviter.client.snapshot();
    let request = snapshot
        .party(party)?
        .join_requests
        .first()
        .ok_or(TestError::Missing("the join request"))?
        .id;
    inviter
        .client
        .decide_join(party, request, JoinDecision::Admit)
        .await?;
    joiner.wake().await?;
    joiner.client.snapshot().party(party)?;
    Ok(())
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

/// What the first phone in the Meetup writes before the other has joined.
const GREETING: &str = "Here first";

/// Both phones report fixes in the Spot and wake, half a minute apart, until each is in the
/// Meetup: entry needs two fixes, the pools fill one mailbox per wake, the board shows the
/// other's record, a knock goes out and is answered, and the Welcome is read. The phone that
/// created the Meetup greets before the other joins, so the greeting reaches the other only
/// from the group log, when its channel first opens.
async fn meet<'phones>(
    ana: &'phones Phone,
    ben: &'phones Phone,
) -> Result<Meeting<'phones>, TestError> {
    let mut greeted = false;
    for _round in 0..40 {
        ana.clock.advance(Duration::from_secs(30));
        for (phone, other) in [(ana, ben), (ben, ana)] {
            phone.at_the_spot().await?;
            phone.wake().await?;
            let first = phone
                .client
                .snapshot()
                .meetups
                .first()
                .map(|shown| shown.id);
            if let Some(meetup) = first
                && !greeted
                && other.client.snapshot().meetups.is_empty()
            {
                let greeting = ChatBody::new(GREETING.to_owned())?;
                phone.client.send_chat(meetup, greeting, None).await?;
                greeted = true;
            }
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

/// The top-level messages of a Meetup's chat, as a phone shows it.
fn messages(shown: &Meetup) -> Vec<ChatMessage> {
    shown
        .chat
        .iter()
        .filter_map(|entry| match entry {
            ChatEntry::Message(message) => Some(message.clone()),
            ChatEntry::Joined { .. } | ChatEntry::MeetPointSet { .. } => None,
        })
        .collect()
}

/// Whether `shown` has a top-level message reading `body`.
fn has_message(shown: &Meetup, body: &str) -> bool {
    messages(shown)
        .iter()
        .any(|message| message.body.as_str() == body)
}

/// Whether the server still holds `group`: a request the gateway cannot authorise is refused
/// as unknown (404) once the group is gone, and as unauthorised while it exists.
async fn group_exists(
    harness: &InProcessHarness,
    clock: &ManualClock,
    group: GroupId,
) -> Result<bool, TestError> {
    use pingers_transport::{
        KeyPin, OhttpConfig, OhttpTransport, PrivacyClass, Relay, TlsRoots, Transport as _,
    };

    let relay = harness.endpoints().ohttp_relay;
    let ohttp = OhttpTransport::new(
        OhttpConfig::new(
            vec![Relay::new(
                &format!("http://{relay}/ohttp"),
                &format!("http://{relay}/v1/ohttp/config"),
            )?],
            KeyPin::FirstUse,
            TlsRoots::from_der(&[])?,
        ),
        Arc::new(clock.clone()),
        Box::new(ChaCha20Rng::seed_from_u64(91)),
    )?;
    let answer = ohttp
        .request(
            PrivacyClass::Anonymous,
            pingers_ohttp::BinaryRequest {
                method: "GET".to_owned(),
                path: format!("/v1/groups/{}/state", hex::encode(group.as_bytes())),
                headers: Vec::new(),
                body: Vec::new(),
            },
        )
        .await?;
    Ok(answer.status.get() != 404)
}

impl Meeting<'_> {
    /// Both shells heard of the Meetup and run the Meetup location service (at 1 Hz on the
    /// phone whose app is on screen), and both show the alert.
    async fn started(&self, on_screen: &Phone) -> TestResult {
        let meetup = self.meetup;
        for phone in [self.ana, self.ben] {
            let started = RecordedEvent::MeetupStarted { meetup };
            phone.told("the Meetup started", started).await?;
            let mode = if core::ptr::eq(phone, on_screen) {
                LocationMode::MeetupForeground
            } else {
                LocationMode::MeetupBackground
            };
            assert_eq!(phone.platform.location_mode(), mode);
            assert_eq!(phone.client.snapshot().alert, Some(meetup));
        }
        Ok(())
    }

    /// The greeting written before the second phone joined reached it from the group log;
    /// Ana writes, Ben's chat shows it with the event that refreshes his screen, and Ben's
    /// reply shows under her message on her phone.
    async fn chat(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        for phone in [ana, ben] {
            phone
                .until("the greeting", |snapshot| {
                    snapshot
                        .meetup(meetup)
                        .is_ok_and(|shown| has_message(shown, GREETING))
                })
                .await?;
        }
        let body = ChatBody::new("On the left wall".to_owned())?;
        let sent = ana.client.send_chat(meetup, body, None).await?;
        let seen = ben
            .until("Ana's message on Ben's phone", |snapshot| {
                snapshot
                    .meetup(meetup)
                    .is_ok_and(|shown| has_message(shown, "On the left wall"))
            })
            .await?;
        let message = messages(seen.meetup(meetup)?)
            .into_iter()
            .find(|message| message.id == sent)
            .ok_or(TestError::Missing("the message"))?;
        assert_eq!(message.author, self.ana_leaf);
        ben.heard("the message event", EventKind::MeetupMessage)
            .await?;
        let reply = ChatBody::new("Coming".to_owned())?;
        let replied = ben.client.send_chat(meetup, reply, Some(sent)).await?;
        let shown = ana
            .until("Ben's reply on Ana's phone", |snapshot| {
                snapshot.meetup(meetup).is_ok_and(|shown| {
                    messages(shown)
                        .iter()
                        .any(|message| message.id == sent && !message.replies.is_empty())
                })
            })
            .await?;
        let thread = messages(shown.meetup(meetup)?)
            .into_iter()
            .find(|message| message.id == sent)
            .ok_or(TestError::Missing("the thread"))?;
        let under: Vec<_> = thread
            .replies
            .iter()
            .map(|reply| (reply.id, reply.author))
            .collect();
        assert_eq!(under, vec![(replied, self.ben_leaf)]);
        // A reply is not a message of its own in the main thread.
        assert!(!has_message(shown.meetup(meetup)?, "Coming"));
        Ok(())
    }

    /// Ana sets a meet point; Ben's screens show where, who set it, and a line in the chat.
    async fn meet_point(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        let label = pingers_proto::MeetPointLabel::new("The big oak".to_owned())?;
        let point = MeetPointDraft::Fixed {
            position: Coordinate::new(SPOT_LATITUDE + 0.0003, SPOT_LONGITUDE)?,
            label: Some(label.clone()),
        };
        ana.client.set_meet_point(meetup, point).await?;
        let seen = ben
            .until("the meet point on Ben's phone", |snapshot| {
                snapshot
                    .meetup(meetup)
                    .is_ok_and(|shown| shown.meet_point.is_some())
            })
            .await?;
        let shown = seen.meetup(meetup)?;
        let point = shown
            .meet_point
            .clone()
            .ok_or(TestError::Missing("the meet point"))?;
        assert_eq!(point.set_by, self.ana_leaf);
        assert!(matches!(
            point.place,
            MeetPlace::Fixed { label: Some(ref named), .. } if *named == label
        ));
        assert!(shown.chat.iter().any(|entry| matches!(
            entry,
            ChatEntry::MeetPointSet { author, label: Some(named) }
                if *author == self.ana_leaf && *named == label
        )));
        Ok(())
    }

    /// Ben asks to see Ana's exact spot ("Request sent"); Ana's phone raises the request and
    /// she allows it.
    async fn ask_and_allow(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        let sent = ben.client.ask_location(meetup, self.ana_leaf).await?;
        let asking = ben.client.snapshot();
        let ana_shown = asking
            .meetup(meetup)?
            .others
            .iter()
            .find(|peer| peer.leaf == self.ana_leaf)
            .map(|peer| peer.access);
        assert_eq!(ana_shown, Some(PeerAccess::Asked));
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
        ana.told("the request raised", raised).await?;
        let allow = LocationAnswer::Allow {
            scope: GrantScope::ThisMeetup,
            ask_back: AskBack::DoNotAskBack,
            always: AllowListChoice::JustThisTime,
        };
        ana.client
            .answer_location_request(meetup, request.id, allow)
            .await?;
        ben.heard("access to Ana", EventKind::LocationAccessChanged)
            .await?;
        Ok(())
    }

    /// Location frames: Ana's app is on screen, so her frames come every 2 s; her next one
    /// carries her fix, and Ben sees her pin.
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
        // Well under the 60 s background cadence in all.
        for _frame in 0..10 {
            ana.at_the_spot().await?;
            ana.clock.advance(Duration::from_secs(3));
            let shown =
                tokio::time::timeout(Duration::from_secs(3), ben.until("Ana's position", shared))
                    .await;
            if matches!(shown, Ok(Ok(_))) {
                let moved = RecordedEvent::PeerLocation {
                    meetup,
                    peer: self.ana_leaf,
                };
                ben.told("Ana's position event", moved).await?;
                let sharing = ana.client.snapshot().meetup(meetup)?.sharing_with.clone();
                assert_eq!(sharing, vec![self.ben_leaf]);
                return Ok(());
            }
        }
        Err(TestError::Missing("Ana's position on Ben's phone"))
    }

    /// Ana stops sharing with Ben: her key rotates, and once her next frame comes under the
    /// new key his phone is told and her pin goes.
    async fn revoke(&self) -> TestResult {
        let Self {
            ana, ben, meetup, ..
        } = *self;
        ana.client.revoke_location(meetup, self.ben_leaf).await?;
        let lost = RecordedEvent::LocationAccessChanged {
            meetup,
            peer: self.ana_leaf,
            access: pingers_meetup::ExactLocation::Private,
        };
        for _frame in 0..10 {
            if ben.events.events().contains(&lost) {
                break;
            }
            ana.at_the_spot().await?;
            ana.clock.advance(Duration::from_secs(3));
            let _heard = tokio::time::timeout(
                Duration::from_secs(2),
                ben.told("access to Ana withdrawn", lost),
            )
            .await;
        }
        ben.told("access to Ana withdrawn", lost).await?;
        let shown = ben.client.snapshot();
        let access = shown
            .meetup(meetup)?
            .others
            .iter()
            .find(|peer| peer.leaf == self.ana_leaf)
            .map(|peer| peer.access);
        assert_eq!(access, Some(PeerAccess::Private));
        assert!(
            ana.client
                .snapshot()
                .meetup(meetup)?
                .sharing_with
                .is_empty()
        );
        Ok(())
    }

    /// Ben's app comes on screen: his phone switches to the 1 Hz Meetup location service.
    async fn ben_on_screen(&self) -> TestResult {
        self.ben.client.on_foreground(Foreground::Visible).await?;
        assert_eq!(
            self.ben.platform.location_mode(),
            LocationMode::MeetupForeground
        );
        Ok(())
    }

    /// Ana taps Leave: her keys go at once and Ben sees her go; then Ben, alone, walks out of
    /// the Spot, his phone leaves once the exit is confirmed, the group goes on the server,
    /// and the Meetup is over on both phones.
    async fn both_leave(&self, harness: &InProcessHarness) -> TestResult {
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
        assert!(group_exists(harness, &ben.clock, meetup).await?);
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
            phone.told("the Meetup ended", ended).await?;
            assert_eq!(phone.platform.location_mode(), mode);
            let summary = snapshot
                .ended_meetup
                .clone()
                .ok_or(TestError::Missing("the summary"))?;
            assert_eq!(summary.spot_name.as_str(), "The crag");
            // The greeting, Ana's message and Ben's reply.
            assert_eq!((summary.people, summary.messages), (2, 3));
        }
        // The last one out deleted the group on the server.
        assert!(!group_exists(harness, &ben.clock, meetup).await?);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_meet_chat_share_a_location_and_end_the_meetup() -> TestResult {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let off = |seed| Who {
        seed,
        pushes: Pushes::Off,
    };
    let ana = Phone::new(&harness, &config, off(3)).await?;
    let ben = Phone::new(&harness, &config, off(4)).await?;
    let party = party_of_two(&ana, &ben).await?;
    spot(&ana, party).await?;
    // Ana has the app open when the Meetup forms.
    ana.client.on_foreground(Foreground::Visible).await?;
    let meeting = meet(&ana, &ben).await?;
    meeting.started(&ana).await?;
    meeting.chat().await?;
    meeting.meet_point().await?;
    meeting.ask_and_allow().await?;
    meeting.frames().await?;
    meeting.revoke().await?;
    meeting.ben_on_screen().await?;
    meeting.both_leave(&harness).await
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
    // Ana's phone recognises pushes for the Meetup by its own subscription.
    assert!(ana.subscribed(meetup)?);
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

    // Alone for half an hour, Ana's Meetup ends on its own: on her phone, where its
    // subscription goes too, and on the server.
    for _minutes in 0..8 {
        if ana.client.snapshot().meetups.is_empty() {
            break;
        }
        ana.clock.advance(Duration::from_mins(5));
        let _settled = tokio::time::timeout(
            Duration::from_secs(1),
            ana.until("the Meetup over", |snapshot| snapshot.meetups.is_empty()),
        )
        .await;
    }
    ana.told("the Meetup ended", RecordedEvent::MeetupEnded { meetup })
        .await?;
    assert!(ana.client.snapshot().meetups.is_empty());
    assert_eq!(ana.meetup_rows()?, 0);
    assert!(!ana.subscribed(meetup)?);
    assert_eq!(ana.platform.location_mode(), LocationMode::Dwell);
    assert!(!group_exists(&harness, &ana.clock, meetup).await?);
    Ok(())
}

/// Ben turns Focus on: sharing is paused, so his phone leaves every Meetup at once, and Ana
/// sees him go.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_leaves_every_meetup() -> TestResult {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let off = |seed| Who {
        seed,
        pushes: Pushes::Off,
    };
    let ana = Phone::new(&harness, &config, off(7)).await?;
    let ben = Phone::new(&harness, &config, off(8)).await?;
    let party = party_of_two(&ana, &ben).await?;
    spot(&ana, party).await?;
    let meetup = meet(&ana, &ben).await?.meetup;
    ben.platform.set_focus(pingers_gate::FocusState::On);
    ben.client.on_dnd_changed().await?;
    ben.told("the Meetup ended", RecordedEvent::MeetupEnded { meetup })
        .await?;
    assert!(ben.client.snapshot().meetups.is_empty());
    assert_eq!(ben.meetup_rows()?, 0);
    ana.until("Ben gone", |snapshot| {
        snapshot
            .meetup(meetup)
            .is_ok_and(|shown| shown.others.is_empty())
    })
    .await?;
    Ok(())
}

/// Each phone reports a fix in the Spot and wakes, half a minute apart, `rounds` times.
async fn dwell(phones: &[&Phone], rounds: usize) -> TestResult {
    for _round in 0..rounds {
        if let Some(first) = phones.first() {
            first.clock.advance(Duration::from_secs(30));
        }
        for phone in phones {
            phone.at_the_spot().await?;
            phone.wake().await?;
        }
    }
    Ok(())
}

/// `phone` reports `fixes` fixes in the Spot, half a minute apart; each runs a wake.
async fn walk_in(phone: &Phone, fixes: usize) -> TestResult {
    for _fix in 0..fixes {
        phone.clock.advance(Duration::from_secs(30));
        phone.at_the_spot().await?;
    }
    Ok(())
}

/// The distinct Meetups a phone's shell heard start.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other event, and any added later, is not a start"
)]
fn started_meetups(phone: &Phone) -> Vec<GroupId> {
    let mut started: Vec<GroupId> = phone
        .events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            RecordedEvent::MeetupStarted { meetup } => Some(meetup),
            _ => None,
        })
        .collect();
    started.sort_unstable();
    started.dedup();
    started
}

/// Ana and Cat are in the Spot without having met (neither could knock yet) when Ben
/// arrives and knocks both: each starts a Meetup with him, so two form at one Spot. A phone in
/// both keeps the one with the smaller group id and leaves the other (design B.5), and in the
/// end Ana and Ben are in one Meetup, the same one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_meetups_at_one_spot_come_down_to_one() -> TestResult {
    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let off = |seed| Who {
        seed,
        pushes: Pushes::Off,
    };
    let ana = Phone::new(&harness, &config, off(9)).await?;
    let ben = Phone::new(&harness, &config, off(10)).await?;
    let cat = Phone::new(&harness, &config, off(11)).await?;
    let party = party_of_two(&ana, &ben).await?;
    let_in(&ana, &cat, party, "Cat").await?;
    ben.wake().await?;
    spot(&ana, party).await?;
    // Cat walks in to an empty Spot and stops waking; Ana walks in after her and knocks her,
    // but Cat does not wake to answer. Each fix is a wake of its own.
    walk_in(&cat, 3).await?;
    walk_in(&ana, 3).await?;
    // Ben fills his mailbox pools first (one mailbox a wake), so he can knock both at once
    // when he comes in.
    for _wake in 0..8 {
        ben.clock.advance(Duration::from_secs(30));
        ben.wake().await?;
    }
    walk_in(&ben, 3).await?;
    // Ana answers Ben (a Meetup of two), then Cat answers both knocks (a second Meetup, of
    // three): Ana and Ben are each in two at one Spot.
    walk_in(&ana, 1).await?;
    walk_in(&cat, 1).await?;
    // Everyone wakes until no phone is in two Meetups and Ana and Ben share one.
    for _round in 0..20 {
        dwell(&[&ana, &cat, &ben], 1).await?;
        let ana_in: Vec<GroupId> = ana.client.snapshot().meetups.iter().map(|m| m.id).collect();
        let ben_in: Vec<GroupId> = ben.client.snapshot().meetups.iter().map(|m| m.id).collect();
        let duplicates_seen = started_meetups(&ana).len() > 1 || started_meetups(&ben).len() > 1;
        if duplicates_seen && ana_in.len() == 1 && ana_in == ben_in {
            let kept = ana_in
                .first()
                .copied()
                .ok_or(TestError::Missing("the Meetup"))?;
            // The kept one is the smallest of those that formed.
            for phone in [&ana, &ben] {
                assert_eq!(started_meetups(phone).first(), Some(&kept));
            }
            assert!(cat.client.snapshot().meetups.len() <= 1);
            return Ok(());
        }
    }
    Err(TestError::Missing(
        "one Meetup for Ana and Ben after two formed",
    ))
}

/// The server showed Ana and Ben different boards: each phone kept another hash for the same
/// closed segment. The one who joins sends the segments it read as its first message (design
/// B.4.3); the one already in the Meetup finds they differ and its shell is told.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_shown_different_boards_are_told() -> TestResult {
    use pingers_proto::{SeenSegment, SegmentHash};

    let config = HarnessConfig::default();
    let harness = InProcessHarness::start(&config)?;
    let off = |seed| Who {
        seed,
        pushes: Pushes::Off,
    };
    let ana = Phone::new(&harness, &config, off(12)).await?;
    let ben = Phone::new(&harness, &config, off(13)).await?;
    let party = party_of_two(&ana, &ben).await?;
    spot(&ana, party).await?;
    // A day ahead, so no segment the real board shows replaces it or pushes it out.
    let start = ana
        .clock
        .now()
        .checked_add(Duration::from_hours(24))
        .ok_or(TestError::Missing("a time a day ahead"))?
        .bucket();
    for (phone, hash) in [(&ana, [1; 32]), (&ben, [2; 32])] {
        let database = Database::open(
            &phone.database,
            DatabaseKey::from_bytes(Zeroizing::new(KEY)),
        )?;
        database.session()?.board_seen().record(&SeenSegment {
            start,
            hash: SegmentHash::from_bytes(hash),
        })?;
    }
    meet(&ana, &ben).await?;
    let told = RecordedEvent::Notice(pingers_client::Notice::BoardEquivocation);
    let heard = |phone: &Phone| phone.events.events().contains(&told);
    for _frame in 0..20 {
        if heard(&ana) || heard(&ben) {
            return Ok(());
        }
        for phone in [&ana, &ben] {
            phone.clock.advance(Duration::from_secs(3));
            phone.at_the_spot().await?;
        }
        let _heard = tokio::time::timeout(
            Duration::from_millis(500),
            ana.told("the boards differ", told),
        )
        .await;
    }
    Err(TestError::Missing("a notice that the boards differ"))
}
