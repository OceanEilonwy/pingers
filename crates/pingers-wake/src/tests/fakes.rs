// SPDX-License-Identifier: AGPL-3.0-only
//! Fakes for the ports: shared server worlds (board and mailboxes, one per server), Parties,
//! Meetups, and `DevicePort` for pingers-testkit's `FakePlatform`.

use core::future::{Future, poll_fn, ready};
use core::task::Poll;
use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot};
use pingers_gate::FocusState;
use pingers_geo::Fix;
use pingers_presence::{
    Admission, EpochState, MailboxPurpose, MeetupKeyId, OutgoingKnock, SpotRef, VerifiedKnock,
};
use pingers_proto::{
    BoardCursor, BoardPage, Epoch, GroupId, KeyPackageBytes, LeafIndex, Lease, MailboxId, PartyId,
    PushHandle, PushRelayKeyId, ReadBoard, Record, RelayId, RelayRef, SealedPush, SealedPushBlob,
    SegmentInfo, ServerId, Subscription, SubscriptionId, TimeToLive, Urgency,
};
use pingers_testkit::{FakePlatform, Geofence};
use pingers_time::{Bucket, Clock, ManualClock, UnixSeconds};
use zeroize::Zeroizing;

use crate::PortError;
use crate::ports::{
    DevicePort, FixQuality, KnockKeyPackage, LeaseUpload, LocationMode, MeetupAdmission,
    MeetupPort, OsRegion, PartyInfo, PartyView, RegionId, ReplyPost, ServerPort,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl DevicePort for FakePlatform {
    fn focus_state(&self) -> FocusState {
        self.focus()
    }

    fn request_fix(
        &self,
        quality: FixQuality,
        timeout: Duration,
    ) -> impl Future<Output = Option<Fix>> + Send {
        let quality = match quality {
            FixQuality::Balanced => pingers_testkit::FixQuality::Balanced,
            FixQuality::High => pingers_testkit::FixQuality::High,
        };
        FakePlatform::request_fix(self, quality, timeout);
        ready(self.take_answered_fixes().pop())
    }

    fn set_os_geofences(&self, regions: &[OsRegion]) {
        let geofences: Vec<Geofence> = regions
            .iter()
            .map(|region| Geofence {
                id: match region.id {
                    RegionId::Spot(_) => String::from("spot"),
                    RegionId::Horizon => String::from("horizon"),
                },
                region: region.region,
            })
            .collect();
        FakePlatform::set_os_geofences(self, &geofences);
    }

    fn set_location_mode(&self, mode: LocationMode) {
        FakePlatform::set_location_mode(
            self,
            match mode {
                LocationMode::Idle => pingers_testkit::LocationMode::Idle,
                LocationMode::Dwell => pingers_testkit::LocationMode::Dwell,
            },
        );
    }

    fn schedule_wake(&self, after: Duration) {
        FakePlatform::schedule_wake(self, after);
    }
}

/// One server every phone talks to: the board by segment, and the mailboxes.
#[derive(Default)]
pub(crate) struct World {
    segments: BTreeMap<Bucket, Vec<Record>>,
    mailboxes: HashMap<MailboxId, Vec<Vec<u8>>>,
}

impl World {
    pub(crate) fn shared() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }

    /// A post in a mailbox, as another device or pingers-meetup-session would make it.
    pub(crate) fn post(&mut self, mailbox: MailboxId, post: Vec<u8>) {
        self.mailboxes.entry(mailbox).or_default().push(post);
    }

    /// The posts in a mailbox.
    pub(crate) fn posts(&self, mailbox: &MailboxId) -> usize {
        self.mailboxes.get(mailbox).map_or(0, Vec::len)
    }

    /// Every record on the board.
    pub(crate) fn record_count(&self) -> usize {
        self.segments.values().map(Vec::len).sum()
    }
}

/// Which server call a test wants to disturb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKind {
    PullLogs,
    Upload,
    Page,
    PostKnock,
    ReadMailbox,
    Refill,
}

/// What happens to that call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// It fails.
    Fail,
    /// It completes, but only after this much time has passed.
    Slow(Duration),
    /// This much time passes and it never completes.
    Hang(Duration),
    /// It never completes, and time passes while it waits: a second each time it is polled,
    /// so it lasts exactly until the wake gives up on it.
    Creep,
}

/// A call the server saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    PullLogs,
    Upload { read: ReadBoard },
    Page,
    PostKnock(MailboxId),
    ReadMailbox(MailboxId),
    Refill,
}

/// A call, the server it was for, and when it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServerCall {
    pub(crate) server: ServerId,
    pub(crate) call: Call,
    pub(crate) at: UnixSeconds,
}

/// A disturbance: on one server, or on every server when `server` is `None`.
struct Disturbance {
    server: Option<ServerId>,
    kind: CallKind,
    effect: Effect,
}

struct ServerInner {
    calls: Vec<ServerCall>,
    own_leases: Vec<(ServerId, Lease)>,
    uploaded: Vec<(ServerId, Lease)>,
    next_mailbox: u8,
    pool: usize,
    effects: Vec<Disturbance>,
}

/// One phone's view of the servers (pingers-sync): its lease and mailbox pool on each.
pub(crate) struct FakeServer {
    worlds: Vec<(ServerId, Arc<Mutex<World>>)>,
    clock: ManualClock,
    tag: u8,
    page_size: usize,
    inner: Mutex<ServerInner>,
}

impl FakeServer {
    pub(crate) fn new(
        server: ServerId,
        world: Arc<Mutex<World>>,
        clock: ManualClock,
        tag: u8,
    ) -> Self {
        Self {
            worlds: vec![(server, world)],
            clock,
            tag,
            page_size: usize::MAX,
            inner: Mutex::new(ServerInner {
                calls: Vec::new(),
                own_leases: Vec::new(),
                uploaded: Vec::new(),
                next_mailbox: 0,
                pool: usize::MAX,
                effects: Vec::new(),
            }),
        }
    }

    /// Another server this phone can reach.
    pub(crate) fn add_server(&mut self, server: ServerId, world: Arc<Mutex<World>>) {
        self.worlds.push((server, world));
    }

    pub(crate) fn set_page_size(&mut self, records: usize) {
        self.page_size = records;
    }

    pub(crate) fn set_pool(&self, mailboxes: usize) {
        lock(&self.inner).pool = mailboxes;
    }

    /// Disturbs `kind` on every server.
    pub(crate) fn disturb(&self, kind: CallKind, effect: Effect) {
        lock(&self.inner).effects.push(Disturbance {
            server: None,
            kind,
            effect,
        });
    }

    /// Disturbs `kind` on one server only.
    pub(crate) fn disturb_on(&self, server: &ServerId, kind: CallKind, effect: Effect) {
        lock(&self.inner).effects.push(Disturbance {
            server: Some(server.clone()),
            kind,
            effect,
        });
    }

    pub(crate) fn calm(&self) {
        lock(&self.inner).effects.clear();
    }

    /// Every call, to any server.
    pub(crate) fn calls(&self) -> Vec<Call> {
        lock(&self.inner)
            .calls
            .iter()
            .map(|seen| seen.call.clone())
            .collect()
    }

    /// Every call, with its server and time.
    pub(crate) fn server_calls(&self) -> Vec<ServerCall> {
        lock(&self.inner).calls.clone()
    }

    /// The calls to one server.
    pub(crate) fn calls_to(&self, server: &ServerId) -> Vec<Call> {
        lock(&self.inner)
            .calls
            .iter()
            .filter(|seen| seen.server == *server)
            .map(|seen| seen.call.clone())
            .collect()
    }

    pub(crate) fn take_calls(&self) -> Vec<Call> {
        core::mem::take(&mut lock(&self.inner).calls)
            .into_iter()
            .map(|seen| seen.call)
            .collect()
    }

    /// Every lease uploaded, to any server.
    pub(crate) fn uploaded(&self) -> Vec<Lease> {
        lock(&self.inner)
            .uploaded
            .iter()
            .map(|(_server, lease)| *lease)
            .collect()
    }

    /// The leases uploaded to one server.
    pub(crate) fn uploaded_to(&self, server: &ServerId) -> Vec<Lease> {
        lock(&self.inner)
            .uploaded
            .iter()
            .filter(|(to, _lease)| to == server)
            .map(|(_server, lease)| *lease)
            .collect()
    }

    fn world(&self, server: &ServerId) -> Result<&Arc<Mutex<World>>, PortError> {
        self.worlds
            .iter()
            .find(|(known, _world)| known == server)
            .map(|(_server, world)| world)
            .ok_or(PortError::Unreachable)
    }

    /// Records the call and applies its effect: whether it goes on, hangs or creeps.
    fn enter(&self, server: &ServerId, kind: CallKind, call: Call) -> Result<Proceed, PortError> {
        self.world(server)?;
        let effect = {
            let mut inner = lock(&self.inner);
            inner.calls.push(ServerCall {
                server: server.clone(),
                call,
                at: Clock::now(&self.clock),
            });
            inner
                .effects
                .iter()
                .find(|disturbance| {
                    disturbance.kind == kind
                        && disturbance
                            .server
                            .as_ref()
                            .is_none_or(|disturbed| disturbed == server)
                })
                .map(|disturbance| disturbance.effect)
        };
        match effect {
            None => Ok(Proceed::Go),
            Some(Effect::Fail) => Err(PortError::Unreachable),
            Some(Effect::Slow(takes)) => {
                self.clock.advance(takes);
                Ok(Proceed::Go)
            }
            Some(Effect::Hang(takes)) => {
                self.clock.advance(takes);
                Ok(Proceed::Hang)
            }
            Some(Effect::Creep) => Ok(Proceed::Creep),
        }
    }

    /// Never completes; with `Creep`, a second passes each time it is polled.
    async fn stall(&self, how: Proceed) {
        let creep = how == Proceed::Creep;
        poll_fn(|context| {
            if creep {
                self.clock.advance(Duration::from_secs(1));
                context.waker().wake_by_ref();
            }
            Poll::<()>::Pending
        })
        .await;
    }

    async fn gate(&self, server: &ServerId, kind: CallKind, call: Call) -> Result<(), PortError> {
        match self.enter(server, kind, call)? {
            Proceed::Go => Ok(()),
            stalled @ (Proceed::Hang | Proceed::Creep) => {
                self.stall(stalled).await;
                Ok(())
            }
        }
    }

    fn page_from(&self, server: &ServerId, offset: usize) -> Result<BoardPage, PortError> {
        let world = lock(self.world(server)?);
        let mut segments = Vec::new();
        let mut records = Vec::new();
        let mut index = 0_usize;
        let end = offset.saturating_add(self.page_size);
        for (start, segment) in &world.segments {
            let mut count = 0_u32;
            for record in segment {
                if index >= offset && index < end {
                    records.push(*record);
                    count = count.saturating_add(1);
                }
                index = index.saturating_add(1);
            }
            if count > 0 {
                segments.push(SegmentInfo {
                    start: *start,
                    record_count: count,
                    closed_hash: None,
                });
            }
        }
        let next = (index > end).then(|| BoardCursor::from_bytes(end.to_be_bytes().to_vec()));
        Ok(BoardPage {
            segments,
            records,
            next,
        })
    }
}

/// Whether a call goes on, hangs, or creeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Proceed {
    Go,
    Hang,
    Creep,
}

impl ServerPort for FakeServer {
    async fn pull_group_logs(&self, server: &ServerId) -> Result<(), PortError> {
        self.gate(server, CallKind::PullLogs, Call::PullLogs).await
    }

    async fn upload_lease(
        &self,
        server: &ServerId,
        upload: LeaseUpload,
    ) -> Result<Option<BoardPage>, PortError> {
        let read = upload.read;
        let how = self.enter(server, CallKind::Upload, Call::Upload { read })?;
        // The request reaches the server even when the answer never comes back.
        self.store(server, &upload.lease)?;
        if how != Proceed::Go {
            self.stall(how).await;
        }
        if read == ReadBoard::Read {
            Ok(Some(self.page_from(server, 0)?))
        } else {
            Ok(None)
        }
    }

    async fn board_page(
        &self,
        server: &ServerId,
        cursor: BoardCursor,
    ) -> Result<BoardPage, PortError> {
        self.gate(server, CallKind::Page, Call::Page).await?;
        let offset = cursor
            .as_bytes()
            .try_into()
            .map(usize::from_be_bytes)
            .map_err(|_length| PortError::Refused)?;
        self.page_from(server, offset)
    }

    async fn post_knock(&self, server: &ServerId, knock: OutgoingKnock) -> Result<(), PortError> {
        self.gate(server, CallKind::PostKnock, Call::PostKnock(knock.mailbox))
            .await?;
        lock(self.world(server)?).post(knock.mailbox, knock.post);
        Ok(())
    }

    async fn read_mailbox(
        &self,
        server: &ServerId,
        mailbox: MailboxId,
    ) -> Result<Vec<Vec<u8>>, PortError> {
        self.gate(server, CallKind::ReadMailbox, Call::ReadMailbox(mailbox))
            .await?;
        Ok(lock(self.world(server)?)
            .mailboxes
            .get(&mailbox)
            .cloned()
            .unwrap_or_default())
    }

    fn take_mailbox(
        &self,
        _server: &ServerId,
        _purpose: MailboxPurpose,
        _lease_start: Bucket,
    ) -> Option<MailboxId> {
        let mut inner = lock(&self.inner);
        inner.pool = inner.pool.checked_sub(1)?;
        inner.next_mailbox = inner.next_mailbox.wrapping_add(1);
        let (tag, count) = (self.tag, inner.next_mailbox);
        Some(MailboxId::from_bytes([
            tag, count, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]))
    }

    async fn maintain_push_registrations(&self, server: &ServerId) -> Result<(), PortError> {
        self.gate(server, CallKind::Refill, Call::Refill).await
    }
}

impl FakeServer {
    /// Appends a lease to `server`'s board and revokes this phone's previous one there.
    fn store(&self, server: &ServerId, lease: &Lease) -> Result<(), PortError> {
        let mut inner = lock(&self.inner);
        let mut world = lock(self.world(server)?);
        if let Some(index) = inner.own_leases.iter().position(|(on, _old)| on == server) {
            let (_server, old) = inner.own_leases.swap_remove(index);
            if let Some(segment) = world.segments.get_mut(&old.start) {
                segment.retain(|record| !old.records.contains(record));
            }
        }
        world
            .segments
            .entry(lease.start)
            .or_default()
            .extend_from_slice(&lease.records);
        inner.own_leases.push((server.clone(), *lease));
        inner.uploaded.push((server.clone(), *lease));
        Ok(())
    }
}

/// A Party member as the roster knows it.
#[derive(Clone, Copy)]
pub(crate) struct RosterMember {
    pub(crate) leaf: LeafIndex,
    pub(crate) key: AccountPublicKey,
}

/// One Party a phone is in, with the seed of its presence roots and its roster.
struct FakeParty {
    info: PartyInfo,
    seed: [u8; 32],
    roster: Vec<RosterMember>,
}

/// One phone's Parties. The first is the one `set_epoch`, `edit` and `info` act on.
pub(crate) struct FakeParties {
    parties: Mutex<Vec<FakeParty>>,
    signing_key: AccountSigningKey,
}

impl FakeParties {
    pub(crate) fn new(
        party: PartyInfo,
        seed: [u8; 32],
        roster: Vec<RosterMember>,
        signing_key: AccountSigningKey,
    ) -> Self {
        Self {
            parties: Mutex::new(vec![FakeParty {
                info: party,
                seed,
                roster,
            }]),
            signing_key,
        }
    }

    /// Joins another Party.
    pub(crate) fn add(&self, party: PartyInfo, seed: [u8; 32], roster: Vec<RosterMember>) {
        lock(&self.parties).push(FakeParty {
            info: party,
            seed,
            roster,
        });
    }

    /// Leaves a Party.
    pub(crate) fn remove(&self, party: PartyId) {
        lock(&self.parties).retain(|known| known.info.party != party);
    }

    pub(crate) fn set_epoch(&self, epoch: EpochState) {
        self.edit(|info| info.epoch = epoch);
    }

    pub(crate) fn edit(&self, change: impl FnOnce(&mut PartyInfo)) {
        if let Some(first) = lock(&self.parties).first_mut() {
            change(&mut first.info);
        }
    }

    /// The first Party, unless the phone left it.
    pub(crate) fn info(&self) -> Option<PartyInfo> {
        lock(&self.parties).first().map(|first| first.info.clone())
    }
}

/// The presence root at `epoch`: a different value per epoch, as the MLS exporter gives.
pub(crate) fn root_for(seed: [u8; 32], epoch: Epoch) -> PresenceRoot {
    let mut bytes = seed;
    for (byte, epoch_byte) in bytes.iter_mut().zip(epoch.get().to_be_bytes()) {
        *byte ^= epoch_byte;
    }
    PresenceRoot::from_bytes(Zeroizing::new(bytes))
}

impl PartyView for FakeParties {
    fn parties(&self) -> Vec<PartyInfo> {
        lock(&self.parties)
            .iter()
            .map(|known| known.info.clone())
            .collect()
    }

    fn presence_root(&self, party: PartyId, epoch: Epoch) -> Option<PresenceRoot> {
        lock(&self.parties)
            .iter()
            .find(|known| known.info.party == party && epoch <= known.info.epoch.current)
            .map(|known| root_for(known.seed, epoch))
    }

    fn signature_key(
        &self,
        party: PartyId,
        leaf: LeafIndex,
        _epoch: Epoch,
    ) -> Option<AccountPublicKey> {
        lock(&self.parties)
            .iter()
            .find(|known| known.info.party == party)
            .and_then(|known| {
                known
                    .roster
                    .iter()
                    .find(|member| member.leaf == leaf)
                    .map(|member| member.key)
            })
    }

    fn signing_key(&self) -> &AccountSigningKey {
        &self.signing_key
    }
}

/// What a fake pingers-meetup-session keeps of an admission: the PSK is copied out so tests
/// can compare what the poster and the knocker derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub(crate) key: MeetupKeyId,
    pub(crate) psk: [u8; 32],
    pub(crate) admission: Admission,
    pub(crate) knock: VerifiedKnock,
}

/// What a fake pingers-meetup-session keeps of a Welcome hand-off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Replied {
    pub(crate) key: MeetupKeyId,
    pub(crate) psk: [u8; 32],
    pub(crate) reply_mailbox: MailboxId,
    pub(crate) post: Vec<u8>,
}

#[derive(Default)]
struct MeetupsInner {
    meetups: Vec<(SpotRef, GroupId)>,
    admitted: Vec<Admitted>,
    welcomes: Vec<Replied>,
    /// The reply mailboxes key packages were made for, in order.
    knock_keys: Vec<MailboxId>,
    leave_calls: usize,
    left: Vec<SpotRef>,
}

/// pingers-meetup-session, recording what it was handed.
#[derive(Default)]
pub(crate) struct FakeMeetups {
    inner: Mutex<MeetupsInner>,
}

impl FakeMeetups {
    pub(crate) fn join(&self, spot: SpotRef, group: GroupId) {
        lock(&self.inner).meetups.push((spot, group));
    }

    pub(crate) fn admitted(&self) -> Vec<Admitted> {
        lock(&self.inner).admitted.clone()
    }

    pub(crate) fn welcomes(&self) -> Vec<Replied> {
        lock(&self.inner).welcomes.clone()
    }

    /// The reply mailboxes knock key packages were made for.
    pub(crate) fn knock_keys(&self) -> Vec<MailboxId> {
        lock(&self.inner).knock_keys.clone()
    }

    /// The Spots whose Meetup this device left one by one.
    pub(crate) fn left(&self) -> Vec<SpotRef> {
        lock(&self.inner).left.clone()
    }

    pub(crate) fn leave_calls(&self) -> usize {
        lock(&self.inner).leave_calls
    }
}

/// The Meetup subscription the fake hands out for a knock answered in `reply_mailbox`.
pub(crate) fn knock_subscription(reply_mailbox: MailboxId) -> Option<Subscription> {
    let fill = reply_mailbox.as_bytes().first().copied().unwrap_or(0);
    let time_to_live = TimeToLive::new(Duration::from_hours(24)).ok()?;
    Some(Subscription {
        id: SubscriptionId::from_bytes([fill; 16]),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(1)),
            key_id: PushRelayKeyId::new(1),
            blob: SealedPushBlob::from_bytes([fill; 320]),
            urgency: Urgency::Alert,
            handle: PushHandle::from_bytes([fill; 8]),
        },
        time_to_live,
    })
}

impl MeetupPort for FakeMeetups {
    fn current_meetup(&self, spot: SpotRef) -> Option<GroupId> {
        lock(&self.inner)
            .meetups
            .iter()
            .find(|(at, _group)| *at == spot)
            .map(|(_at, group)| *group)
    }

    fn knock_key_package(
        &self,
        _party: PartyId,
        reply_mailbox: MailboxId,
    ) -> impl Future<Output = Result<KnockKeyPackage, PortError>> + Send {
        lock(&self.inner).knock_keys.push(reply_mailbox);
        ready(Ok(KnockKeyPackage {
            key_package: KeyPackageBytes::from_bytes(vec![0x4b; 200]),
            subscription: knock_subscription(reply_mailbox),
        }))
    }

    fn admit(
        &self,
        admission: MeetupAdmission,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        lock(&self.inner).admitted.push(Admitted {
            key: admission.key.id(),
            psk: *admission.key.psk().expose_bytes(),
            admission: admission.admission,
            knock: admission.knock,
        });
        ready(Ok(()))
    }

    fn welcome(&self, reply: ReplyPost) -> impl Future<Output = Result<(), PortError>> + Send {
        lock(&self.inner).welcomes.push(Replied {
            key: reply.key.id(),
            psk: *reply.key.psk().expose_bytes(),
            reply_mailbox: reply.reply_mailbox,
            post: reply.post,
        });
        ready(Ok(()))
    }

    fn leave(&self, spot: SpotRef) -> impl Future<Output = Result<(), PortError>> + Send {
        let mut inner = lock(&self.inner);
        inner.left.push(spot);
        inner.meetups.retain(|(at, _group)| *at != spot);
        ready(Ok(()))
    }

    fn leave_all(&self) -> impl Future<Output = Result<(), PortError>> + Send {
        let mut inner = lock(&self.inner);
        inner.leave_calls = inner.leave_calls.saturating_add(1);
        inner.meetups.clear();
        ready(Ok(()))
    }
}
