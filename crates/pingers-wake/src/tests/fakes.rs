// SPDX-License-Identifier: AGPL-3.0-only
//! Fakes for the ports: a shared server world (board and mailboxes), Parties, Meetups, and
//! `DevicePort` for pingers-testkit's `FakePlatform`.

use core::future::{Future, ready};
use core::time::Duration;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot};
use pingers_gate::FocusState;
use pingers_geo::Fix;
use pingers_presence::{EpochState, MailboxPurpose, OutgoingKnock, SpotRef};
use pingers_proto::{
    BoardCursor, BoardPage, Epoch, GroupId, KeyPackageBytes, LeafIndex, Lease, MailboxId, PartyId,
    ReadBoard, Record, SegmentInfo,
};
use pingers_testkit::{FakePlatform, Geofence};
use pingers_time::{Bucket, ManualClock};
use zeroize::Zeroizing;

use crate::PortError;
use crate::ports::{
    DevicePort, FixQuality, LeaseUpload, LocationMode, MeetupAdmission, MeetupPort, OsRegion,
    PartyInfo, PartyView, RegionId, ReplyPost, ServerPort,
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

/// The server both phones talk to: the board by segment, and the mailboxes.
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

struct ServerInner {
    calls: Vec<Call>,
    own_lease: Option<Lease>,
    uploaded: Vec<Lease>,
    next_mailbox: u8,
    pool: usize,
    effects: Vec<(CallKind, Effect)>,
}

/// One phone's view of the server (pingers-sync): its lease, its mailbox pool.
pub(crate) struct FakeServer {
    world: Arc<Mutex<World>>,
    clock: ManualClock,
    tag: u8,
    page_size: usize,
    inner: Mutex<ServerInner>,
}

impl FakeServer {
    pub(crate) fn new(world: Arc<Mutex<World>>, clock: ManualClock, tag: u8) -> Self {
        Self {
            world,
            clock,
            tag,
            page_size: usize::MAX,
            inner: Mutex::new(ServerInner {
                calls: Vec::new(),
                own_lease: None,
                uploaded: Vec::new(),
                next_mailbox: 0,
                pool: usize::MAX,
                effects: Vec::new(),
            }),
        }
    }

    pub(crate) fn set_page_size(&mut self, records: usize) {
        self.page_size = records;
    }

    pub(crate) fn set_pool(&self, mailboxes: usize) {
        lock(&self.inner).pool = mailboxes;
    }

    pub(crate) fn disturb(&self, kind: CallKind, effect: Effect) {
        lock(&self.inner).effects.push((kind, effect));
    }

    pub(crate) fn calm(&self) {
        lock(&self.inner).effects.clear();
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        lock(&self.inner).calls.clone()
    }

    pub(crate) fn take_calls(&self) -> Vec<Call> {
        core::mem::take(&mut lock(&self.inner).calls)
    }

    pub(crate) fn uploaded(&self) -> Vec<Lease> {
        lock(&self.inner).uploaded.clone()
    }

    /// Records the call and applies its effect: `Ok(true)` to go on, `Ok(false)` to hang.
    fn enter(&self, kind: CallKind, call: Call) -> Result<bool, PortError> {
        let effect = {
            let mut inner = lock(&self.inner);
            inner.calls.push(call);
            inner
                .effects
                .iter()
                .find(|(disturbed, _effect)| *disturbed == kind)
                .map(|(_kind, effect)| *effect)
        };
        match effect {
            None => Ok(true),
            Some(Effect::Fail) => Err(PortError::Unreachable),
            Some(Effect::Slow(takes)) => {
                self.clock.advance(takes);
                Ok(true)
            }
            Some(Effect::Hang(takes)) => {
                self.clock.advance(takes);
                Ok(false)
            }
        }
    }

    async fn gate(&self, kind: CallKind, call: Call) -> Result<(), PortError> {
        if self.enter(kind, call)? {
            Ok(())
        } else {
            core::future::pending().await
        }
    }

    fn page_from(&self, offset: usize) -> BoardPage {
        let world = lock(&self.world);
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
        BoardPage {
            segments,
            records,
            next,
        }
    }
}

impl ServerPort for FakeServer {
    async fn pull_group_logs(&self) -> Result<(), PortError> {
        self.gate(CallKind::PullLogs, Call::PullLogs).await
    }

    async fn upload_lease(&self, upload: LeaseUpload) -> Result<Option<BoardPage>, PortError> {
        let read = upload.read;
        if !self.enter(CallKind::Upload, Call::Upload { read })? {
            // The request reached the server; the answer never comes back.
            self.store(&upload.lease);
            return core::future::pending().await;
        }
        self.store(&upload.lease);
        Ok((read == ReadBoard::Read).then(|| self.page_from(0)))
    }

    async fn board_page(&self, cursor: BoardCursor) -> Result<BoardPage, PortError> {
        self.gate(CallKind::Page, Call::Page).await?;
        let offset = cursor
            .as_bytes()
            .try_into()
            .map(usize::from_be_bytes)
            .map_err(|_length| PortError::Refused)?;
        Ok(self.page_from(offset))
    }

    async fn post_knock(&self, knock: OutgoingKnock) -> Result<(), PortError> {
        self.gate(CallKind::PostKnock, Call::PostKnock(knock.mailbox))
            .await?;
        lock(&self.world).post(knock.mailbox, knock.post);
        Ok(())
    }

    async fn read_mailbox(&self, mailbox: MailboxId) -> Result<Vec<Vec<u8>>, PortError> {
        self.gate(CallKind::ReadMailbox, Call::ReadMailbox(mailbox))
            .await?;
        Ok(lock(&self.world)
            .mailboxes
            .get(&mailbox)
            .cloned()
            .unwrap_or_default())
    }

    fn take_mailbox(&self, _purpose: MailboxPurpose, _lease_start: Bucket) -> Option<MailboxId> {
        let mut inner = lock(&self.inner);
        inner.pool = inner.pool.checked_sub(1)?;
        inner.next_mailbox = inner.next_mailbox.wrapping_add(1);
        let (tag, count) = (self.tag, inner.next_mailbox);
        Some(MailboxId::from_bytes([
            tag, count, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]))
    }

    async fn refill_mailbox_pool(&self) -> Result<(), PortError> {
        self.gate(CallKind::Refill, Call::Refill).await
    }
}

impl FakeServer {
    /// Appends a lease and revokes this phone's previous one.
    fn store(&self, lease: &Lease) {
        let mut inner = lock(&self.inner);
        let mut world = lock(&self.world);
        if let Some(old) = inner.own_lease.take()
            && let Some(segment) = world.segments.get_mut(&old.start)
        {
            segment.retain(|record| !old.records.contains(record));
        }
        world
            .segments
            .entry(lease.start)
            .or_default()
            .extend_from_slice(&lease.records);
        inner.own_lease = Some(*lease);
        inner.uploaded.push(*lease);
    }
}

/// A Party member as the roster knows it.
#[derive(Clone, Copy)]
pub(crate) struct RosterMember {
    pub(crate) leaf: LeafIndex,
    pub(crate) key: AccountPublicKey,
}

/// One phone's Parties: one Party, shared seed for presence roots.
pub(crate) struct FakeParties {
    party: Mutex<PartyInfo>,
    seed: [u8; 32],
    roster: Vec<RosterMember>,
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
            party: Mutex::new(party),
            seed,
            roster,
            signing_key,
        }
    }

    pub(crate) fn set_epoch(&self, epoch: EpochState) {
        lock(&self.party).epoch = epoch;
    }

    pub(crate) fn edit(&self, change: impl FnOnce(&mut PartyInfo)) {
        change(&mut lock(&self.party));
    }

    pub(crate) fn info(&self) -> PartyInfo {
        lock(&self.party).clone()
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
        vec![self.info()]
    }

    fn presence_root(&self, party: PartyId, epoch: Epoch) -> Option<PresenceRoot> {
        let info = lock(&self.party);
        (party == info.party && epoch <= info.epoch.current).then(|| root_for(self.seed, epoch))
    }

    fn signature_key(
        &self,
        party: PartyId,
        leaf: LeafIndex,
        _epoch: Epoch,
    ) -> Option<AccountPublicKey> {
        if party != lock(&self.party).party {
            return None;
        }
        self.roster
            .iter()
            .find(|member| member.leaf == leaf)
            .map(|member| member.key)
    }

    fn signing_key(&self) -> &AccountSigningKey {
        &self.signing_key
    }

    fn key_package(&self, _party: PartyId) -> Result<KeyPackageBytes, PortError> {
        Ok(KeyPackageBytes::from_bytes(vec![0x4b; 200]))
    }
}

#[derive(Default)]
struct MeetupsInner {
    meetups: Vec<(SpotRef, GroupId)>,
    admitted: Vec<MeetupAdmission>,
    welcomes: Vec<ReplyPost>,
    leave_calls: usize,
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

    pub(crate) fn admitted(&self) -> Vec<MeetupAdmission> {
        lock(&self.inner).admitted.clone()
    }

    pub(crate) fn welcomes(&self) -> Vec<ReplyPost> {
        lock(&self.inner).welcomes.clone()
    }

    pub(crate) fn leave_calls(&self) -> usize {
        lock(&self.inner).leave_calls
    }
}

impl MeetupPort for FakeMeetups {
    fn current_meetup(&self, spot: SpotRef) -> Option<GroupId> {
        lock(&self.inner)
            .meetups
            .iter()
            .find(|(at, _group)| *at == spot)
            .map(|(_at, group)| *group)
    }

    fn admit(
        &self,
        admission: MeetupAdmission,
    ) -> impl Future<Output = Result<(), PortError>> + Send {
        lock(&self.inner).admitted.push(admission);
        ready(Ok(()))
    }

    fn welcome(&self, reply: ReplyPost) -> impl Future<Output = Result<(), PortError>> + Send {
        lock(&self.inner).welcomes.push(reply);
        ready(Ok(()))
    }

    fn leave_all(&self) -> impl Future<Output = Result<(), PortError>> + Send {
        let mut inner = lock(&self.inner);
        inner.leave_calls = inner.leave_calls.saturating_add(1);
        inner.meetups.clear();
        ready(Ok(()))
    }
}
