// SPDX-License-Identifier: AGPL-3.0-only
//! One simulated phone: the protocol crates wired together the way pingers-client will, with
//! anonymous requests going through the relay.

use std::sync::Arc;
use std::time::Duration;

use pingers_crypto::{
    AccountPublicKey, AccountSigningKey, GrantKeyPair, MailboxReadKey, PushTokenToSeal, RevokeKey,
    derive_meetup_psk, derive_spot_key, push_bind_for_subscription, seal_push_token,
};
use pingers_geo::SpotPresence;
use pingers_geo::{Circle, Coordinate, Fence, Fix, Meters, PresenceInput, PresenceState};
use pingers_mls::{
    CipherSuitePolicy, KeyPackagePurpose, MeetupCreation, MeetupPskBinding, MeetupPskId,
    MemoryStorage, MlsContext, ProcessedMessage, Roster, SelfRemoval, group_storage_prefix,
};
use pingers_presence::{
    ActiveSpot, BoardMatch, BuiltLease, FreshFix, KnockContext, KnockDecision, KnockFilter,
    KnockInputs, KnockSituation, LEASE_BUCKETS, LEASE_DURATION, LeaseInputs, LivenessEvidence,
    MailboxPurpose, MailboxRequest, OwnRecords, PushSealing, RealSlot, RosterLookup, Sharing,
    SlotContent, SpotRef, SpotStatus, TrialMatcher, VerifiedKnock, build_knock, build_lease,
    decide_knock, knock_targets, mailbox_covers_lease, open_knock, prepare_mailbox, verify_knock,
};
use pingers_proto::{
    AppMessage, BoardSyncRequest, CommitRequest, CreateGroup, Epoch, GroupId, LeafIndex, LeaseId,
    LeaseReplacement, MailboxId, MeetupHint, Padded, PartyId, PushClass, PushHandle, PushPlatform,
    PushRelayKeyId, PushToken, PushTokenPlaintext, ReadBoard, Record, RecordRandomness, RelayRef,
    SealedPush, SecretBytes, SequenceNumber, SizeClass, SpotId, Subscription, SubscriptionId,
    TimeToLive, Urgency, WelcomeBytes,
};
use pingers_testkit::{random_array, seeded_rng};
use pingers_time::{Bucket, UnixSeconds};
use rand_chacha::ChaCha20Rng;

use crate::recorder::{Net, Recorder};
use crate::{E2eError, Stack, wire};

const SUITE: CipherSuitePolicy = CipherSuitePolicy::Classical;
/// Two fixes this far apart confirm entry (design F.2).
const ENTRY_GAP: Duration = Duration::from_secs(31);
/// How long Meetup subscriptions are kept.
const SUBSCRIPTION_LIFETIME: Duration = Duration::from_hours(2);

/// A Spot: which Party it belongs to, and its circle. Only devices ever hold the geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// The Party.
    pub party: PartyId,
    /// The Spot.
    pub spot: SpotId,
    /// Centre latitude, degrees.
    pub latitude: f64,
    /// Centre longitude, degrees.
    pub longitude: f64,
    /// Radius in metres.
    pub radius_m: f64,
}

impl Place {
    fn center(&self) -> Result<Coordinate, E2eError> {
        Ok(Coordinate::new(self.latitude, self.longitude)?)
    }

    /// A point about 2 km north: clearly outside, beyond any exit buffer.
    fn away(&self) -> Result<Coordinate, E2eError> {
        Ok(Coordinate::new(self.latitude + 0.02, self.longitude)?)
    }

    fn fence(&self) -> Result<Fence, E2eError> {
        Ok(Fence::Circle(Circle::new(
            self.center()?,
            Meters::new(self.radius_m)?,
        )?))
    }
}

/// A mailbox this device registered, with the key that reads it.
struct OwnedMailbox {
    id: MailboxId,
    read_key: MailboxReadKey,
    registered_at: UnixSeconds,
    time_to_live: TimeToLive,
    last_read: Option<SequenceNumber>,
}

/// The live lease and what replaces or revokes it.
struct HeldLease {
    built: BuiltLease,
    id: LeaseId,
    revoke_key: RevokeKey,
}

/// A knock sent, waiting for a Welcome.
struct PendingJoin {
    reply: OwnedMailbox,
    target: BoardMatch,
    grant: GrantKeyPair,
    subscription: SubscriptionId,
}

/// What a Meetup's creator puts in the new group's delivery log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreationLog {
    /// Nothing, as design G.4 draws it.
    Nothing,
    /// The creation commit at epoch 0 (the workaround the scenarios use).
    PostCreationCommit,
}

/// The Meetup this device is in.
struct MeetupState {
    group: GroupId,
    /// Kept until the Meetup ends (design B.6.1); the scenarios do not share locations.
    _grant: GrantKeyPair,
    own_subscription: SubscriptionId,
    /// Subscriptions of members this device added, by signature key, for removing them.
    member_subscriptions: Vec<(AccountPublicKey, SubscriptionId)>,
    /// The last delivery log position processed.
    cursor: SequenceNumber,
    /// Log positions of this device's own commits (already merged locally).
    own_sequences: Vec<SequenceNumber>,
    /// Leaves whose leave proposals wait to be committed.
    leavers: Vec<LeafIndex>,
}

/// The result of one board sync.
#[derive(Clone, Debug)]
pub struct SyncReport {
    /// Records that matched a Spot this device is inside (own records skipped).
    pub matches: Vec<BoardMatch>,
    /// Every record read, across pages.
    pub records: Vec<Record>,
    /// Pages fetched (the first came with the sync).
    pub pages: usize,
    /// The id the server gave the new lease.
    pub lease_id: LeaseId,
}

/// A knock found in this device's knock mailbox, and the decision on it.
#[derive(Clone, Debug)]
pub struct Answer {
    /// The verified knock.
    pub knock: VerifiedKnock,
    /// Accept, decline or wait.
    pub decision: KnockDecision,
}

/// What became of a commit adding or removing members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnockOutcome {
    /// The delivery service took it as the epoch's commit.
    Accepted,
    /// Another member's commit won the epoch (HTTP 409); this one was dropped.
    Conflict,
}

/// The Party roster at one epoch, as the knock check reads it.
struct EpochRoster {
    roster: Roster,
    epoch: Epoch,
}

impl RosterLookup for EpochRoster {
    fn signature_key(&self, leaf: LeafIndex, epoch: Epoch) -> Option<AccountPublicKey> {
        (epoch == self.epoch)
            .then(|| self.roster.signature_key(leaf).copied())
            .flatten()
    }

    fn current_epoch(&self) -> Epoch {
        self.epoch
    }
}

/// One simulated phone.
pub struct Device {
    name: String,
    key: AccountSigningKey,
    storage: MemoryStorage,
    rng: ChaCha20Rng,
    net: Net,
    push_token: Vec<u8>,
    party: Option<(GroupId, PartyId)>,
    place: Option<Place>,
    presence: Option<SpotPresence>,
    position: Option<Coordinate>,
    spot: Option<ActiveSpot>,
    lease: Option<HeldLease>,
    knock_mailbox: Option<OwnedMailbox>,
    retired_mailboxes: Vec<OwnedMailbox>,
    pending_joins: Vec<PendingJoin>,
    meetup: Option<MeetupState>,
    knocked_mailboxes: Vec<MailboxId>,
    knocked_records: Vec<RecordRandomness>,
    registered: Vec<MailboxId>,
}

impl Device {
    /// A device with its own transport through the stack's relay, recording into `recorder`.
    ///
    /// # Errors
    ///
    /// [`E2eError::Transport`] if the transport cannot be built.
    pub fn new(
        name: &str,
        seed: u64,
        stack: &Stack,
        recorder: Arc<Recorder>,
    ) -> Result<Self, E2eError> {
        let transport = Arc::new(stack.transport(seed.wrapping_add(0x5eed))?);
        Ok(Self::with_transport(name, seed, transport, recorder))
    }

    /// A device sharing `transport` (each request still has its own HPKE context): for load
    /// runs with many devices.
    #[must_use]
    pub fn with_transport(
        name: &str,
        seed: u64,
        transport: Arc<pingers_transport::OhttpTransport>,
        recorder: Arc<Recorder>,
    ) -> Self {
        let mut rng = seeded_rng(seed);
        let key = AccountSigningKey::generate(&mut rng);
        let push_token = format!(
            "fcm-token:{name}:{}",
            hex::encode(random_array::<16>(&mut rng))
        )
        .into_bytes();
        Self {
            name: name.to_owned(),
            key,
            storage: MemoryStorage::new(),
            rng,
            net: Net {
                device: name.to_owned(),
                transport,
                recorder,
            },
            push_token,
            party: None,
            place: None,
            presence: None,
            position: None,
            spot: None,
            lease: None,
            knock_mailbox: None,
            retired_mailboxes: Vec::new(),
            pending_joins: Vec::new(),
            meetup: None,
            knocked_mailboxes: Vec::new(),
            knocked_records: Vec::new(),
            registered: Vec::new(),
        }
    }

    /// The device's name in the scenario.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The raw push token the platform gave this device: it must never reach the server.
    #[must_use]
    pub fn push_token(&self) -> &[u8] {
        &self.push_token
    }

    /// The account public key (the leaf signature key in every group).
    #[must_use]
    pub fn account_key(&self) -> AccountPublicKey {
        self.key.public_key()
    }

    /// The Spot state, if a Spot is being watched.
    #[must_use]
    pub fn presence_state(&self) -> Option<PresenceState> {
        self.presence.as_ref().map(SpotPresence::state)
    }

    /// Whether the device holds the Spot's keys (only while `Inside`).
    #[must_use]
    pub const fn holds_spot_keys(&self) -> bool {
        self.spot.is_some()
    }

    /// The live lease's id and its four records, if a lease is held.
    #[must_use]
    pub fn lease(&self) -> Option<(LeaseId, [Record; 4])> {
        self.lease
            .as_ref()
            .map(|held| (held.id, held.built.lease.records))
    }

    /// Whether the live lease has a real record.
    #[must_use]
    pub fn lease_has_real_record(&self) -> bool {
        self.own_record().is_some()
    }

    /// The current knock mailbox, if the device has a real record.
    #[must_use]
    pub fn knock_mailbox(&self) -> Option<MailboxId> {
        self.knock_mailbox.as_ref().map(|mailbox| mailbox.id)
    }

    /// Every mailbox id this device ever registered, deleted or not.
    #[must_use]
    pub fn registered_mailboxes(&self) -> &[MailboxId] {
        &self.registered
    }

    /// Every mailbox id this device still holds (current, retired, and reply mailboxes).
    #[must_use]
    pub fn mailbox_ids(&self) -> Vec<MailboxId> {
        self.knock_mailbox
            .iter()
            .chain(&self.retired_mailboxes)
            .chain(self.pending_joins.iter().map(|join| &join.reply))
            .map(|mailbox| mailbox.id)
            .collect()
    }

    /// The Meetup group, if the device is in one.
    #[must_use]
    pub fn meetup(&self) -> Option<GroupId> {
        self.meetup.as_ref().map(|meetup| meetup.group)
    }

    fn context(&self) -> MlsContext<'_, MemoryStorage> {
        MlsContext::new(&self.storage, &self.key)
    }

    fn party(&self) -> Result<(GroupId, PartyId), E2eError> {
        self.party
            .ok_or_else(|| E2eError::step(format!("{} is in no Party", self.name)))
    }

    fn meetup_state(&self) -> Result<&MeetupState, E2eError> {
        self.meetup
            .as_ref()
            .ok_or_else(|| E2eError::step(format!("{} is in no Meetup", self.name)))
    }

    /// The Meetup's epoch and member count.
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] outside a Meetup, or an MLS storage error.
    pub fn meetup_epoch_and_size(&self) -> Result<(Epoch, usize), E2eError> {
        let group = self.context().meetup_group(&self.meetup_state()?.group)?;
        Ok((group.epoch(), group.roster().entries().len()))
    }

    /// The Meetup's current state key, which changes with every epoch.
    ///
    /// # Errors
    ///
    /// As [`Device::meetup_epoch_and_size`].
    pub fn meetup_state_key(&self) -> Result<Vec<u8>, E2eError> {
        let group = self.context().meetup_group(&self.meetup_state()?.group)?;
        Ok(group.state_key()?.expose_bytes().to_vec())
    }

    /// Whether any MLS state of `group` is left in this device's storage.
    ///
    /// # Errors
    ///
    /// [`E2eError::Mls`] if the storage prefix cannot be formed.
    pub fn has_group_state(&self, group: &GroupId) -> Result<bool, E2eError> {
        Ok(self
            .storage
            .contains_key_fragment(&group_storage_prefix(group)?)
            || self.storage.contains_key_fragment(group.as_bytes()))
    }

    // -----------------------------------------------------------------------------------------
    // Location

    /// Starts watching `place` and walks in: two fixes inside, [`ENTRY_GAP`] apart, so the Spot
    /// goes `Inside` and its keys are derived.
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] if the Spot does not go `Inside`; geo, MLS or crypto errors.
    pub async fn arrive(&mut self, stack: &Stack, place: Place) -> Result<(), E2eError> {
        self.place = Some(place);
        self.presence = Some(SpotPresence::new(place.fence()?, stack.now()));
        self.fix(stack, place.center()?)?;
        stack.advance(ENTRY_GAP).await;
        self.fix(stack, place.center()?)?;
        if self.presence_state() == Some(PresenceState::Inside) && self.spot.is_some() {
            Ok(())
        } else {
            Err(E2eError::step(format!(
                "{} did not get Inside: {:?}",
                self.name,
                self.presence_state()
            )))
        }
    }

    /// Walks away until the Spot leaves `Inside`; the Spot's keys are dropped.
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] if the Spot stays `Inside`.
    pub async fn depart(&mut self, stack: &Stack) -> Result<(), E2eError> {
        let place = self
            .place
            .ok_or_else(|| E2eError::step("no Spot watched"))?;
        for _ in 0..6 {
            self.fix(stack, place.away()?)?;
            if self.presence_state() != Some(PresenceState::Inside) {
                return Ok(());
            }
            stack.advance(ENTRY_GAP).await;
        }
        Err(E2eError::step(format!("{} is still Inside", self.name)))
    }

    fn fix(&mut self, stack: &Stack, coordinate: Coordinate) -> Result<(), E2eError> {
        let presence = self
            .presence
            .as_mut()
            .ok_or_else(|| E2eError::step("no Spot watched"))?;
        let _change = presence.handle(PresenceInput::Fix(Fix {
            coordinate,
            accuracy: Meters::new(10.0)?,
            taken_at: stack.now(),
        }))?;
        self.position = Some(coordinate);
        self.refresh_spot_keys()
    }

    /// Derives the Spot's keys on entering `Inside` and drops them on leaving (design B.4.3,
    /// "match only where you are").
    fn refresh_spot_keys(&mut self) -> Result<(), E2eError> {
        let inside = self.presence_state() == Some(PresenceState::Inside);
        if !inside {
            self.spot = None;
            return Ok(());
        }
        if self.spot.is_some() {
            return Ok(());
        }
        let (party_group, party_id) = self.party()?;
        let place = self.place.ok_or_else(|| E2eError::step("no Spot"))?;
        let group = self.context().party_group(&party_group)?;
        let spot = ActiveSpot::derive(
            &group.presence_root(&party_id)?,
            &SpotStatus {
                spot: SpotRef {
                    party: party_id,
                    spot: place.spot,
                },
                epoch: group.epoch(),
                state: PresenceState::Inside,
            },
        )?;
        self.spot = Some(spot);
        Ok(())
    }

    fn evidence(&self) -> Result<LivenessEvidence, E2eError> {
        let fresh_fix = match (self.position, self.place) {
            (Some(position), Some(place)) => {
                if place.fence()?.contains(position) {
                    FreshFix::InsideSpot
                } else {
                    FreshFix::OutsideSpot
                }
            }
            _ => FreshFix::Unavailable,
        };
        Ok(LivenessEvidence {
            spot_state: self.presence_state().unwrap_or(PresenceState::Outside),
            fresh_fix,
            sharing: Sharing::Allowed,
        })
    }

    // -----------------------------------------------------------------------------------------
    // Mailboxes

    fn token(&self, class: PushClass, stack: &Stack) -> PushTokenPlaintext {
        PushTokenPlaintext {
            platform: PushPlatform::Fcm,
            class,
            token: PushToken::from_bytes(self.push_token.clone()),
            not_after: UnixSeconds::new(stack.now().get().saturating_add(30 * 24 * 3600)),
        }
    }

    async fn register_mailbox(
        &mut self,
        stack: &Stack,
        purpose: MailboxPurpose,
        class: PushClass,
    ) -> Result<OwnedMailbox, E2eError> {
        let token = self.token(class, stack);
        let (relay, relay_key) = stack.push_relay();
        let time_to_live = TimeToLive::new(LEASE_DURATION)?;
        let registration = prepare_mailbox(
            &MailboxRequest {
                purpose,
                time_to_live,
                push: PushSealing::Seal {
                    relay: RelayRef::Listed(relay),
                    relay_key,
                    token: &token,
                    urgency: Urgency::Alert,
                },
            },
            &mut self.rng,
        )?;
        wire::register_mailbox(&self.net, &registration.spec).await?;
        self.registered.push(registration.spec.mailbox_id);
        Ok(OwnedMailbox {
            id: registration.spec.mailbox_id,
            read_key: registration.read_key,
            registered_at: stack.now(),
            time_to_live,
            last_read: None,
        })
    }

    fn subscription(&mut self, stack: &Stack) -> Result<Subscription, E2eError> {
        let id = SubscriptionId::from_bytes(random_array(&mut self.rng));
        let token = self.token(PushClass::Standard, stack);
        let (relay, relay_key) = stack.push_relay();
        let blob = seal_push_token(
            relay_key,
            &PushTokenToSeal {
                bind: push_bind_for_subscription(&id),
                plaintext: &token,
            },
            &mut self.rng,
        )?;
        Ok(Subscription {
            id,
            push: SealedPush {
                relay: RelayRef::Listed(relay),
                key_id: PushRelayKeyId::new(relay_key.key_id()),
                blob,
                urgency: Urgency::Alert,
                handle: PushHandle::from_bytes(random_array(&mut self.rng)),
            },
            time_to_live: TimeToLive::new(SUBSCRIPTION_LIFETIME)?,
        })
    }

    /// Deletes every mailbox this device still holds (knock, retired and reply mailboxes).
    /// Mailboxes the server already expired are skipped. Returns how many were deleted.
    ///
    /// # Errors
    ///
    /// A transport error, or a status other than 204 or 404.
    pub async fn delete_mailboxes(&mut self) -> Result<usize, E2eError> {
        let mut owned: Vec<OwnedMailbox> = self.knock_mailbox.take().into_iter().collect();
        owned.append(&mut self.retired_mailboxes);
        owned.extend(self.pending_joins.drain(..).map(|join| join.reply));
        let mut deleted = 0_usize;
        for mailbox in owned {
            match wire::delete_mailbox(&self.net, &mailbox.id, &mailbox.read_key).await {
                Ok(()) => deleted = deleted.saturating_add(1),
                Err(E2eError::Status { status: 404, .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(deleted)
    }

    // -----------------------------------------------------------------------------------------
    // Board

    fn own_record(&self) -> Option<RecordRandomness> {
        self.lease.as_ref().and_then(|held| {
            held.built
                .slots
                .iter()
                .find(|slot| matches!(slot.content, SlotContent::Real { .. }))
                .map(|slot| slot.randomness)
        })
    }

    /// `POST /v1/board/sync`: replaces the live lease (a real record while `Inside`, naming a
    /// knock mailbox and the Meetup hint; otherwise four dummies), then reads every page and
    /// trial-matches it against the Spot the device is inside.
    ///
    /// # Errors
    ///
    /// Transport, status, encoding or protocol errors.
    pub async fn sync(&mut self, stack: &Stack, read: bool) -> Result<SyncReport, E2eError> {
        let bucket = Bucket::containing(stack.now());
        if self.spot.is_some() {
            let covered = self.knock_mailbox.as_ref().is_some_and(|mailbox| {
                mailbox_covers_lease(mailbox.registered_at, mailbox.time_to_live, bucket)
            });
            if !covered {
                let fresh = self
                    .register_mailbox(stack, MailboxPurpose::Knock, PushClass::Location)
                    .await?;
                if let Some(old) = self.knock_mailbox.replace(fresh) {
                    self.retired_mailboxes.push(old);
                }
            }
        } else if let Some(old) = self.knock_mailbox.take() {
            self.retired_mailboxes.push(old);
        }
        let revoke_key = RevokeKey::generate(&mut self.rng);
        let hint = self
            .meetup
            .as_ref()
            .map(|meetup| MeetupHint::for_group(&meetup.group));
        let real_slots: Vec<RealSlot<'_>> = self
            .spot
            .iter()
            .zip(&self.knock_mailbox)
            .map(|(spot, mailbox)| RealSlot {
                spot,
                knock_mailbox: mailbox.id,
                meetup_hint: hint,
            })
            .collect();
        let built = build_lease(
            &LeaseInputs {
                start: bucket,
                real: real_slots,
                revoke_key: &revoke_key,
            },
            &mut self.rng,
        )?;
        let replaces = self.lease.as_ref().and_then(|held| {
            let still_live = held
                .built
                .lease
                .start
                .checked_add(LEASE_BUCKETS)
                .is_some_and(|end| bucket < end);
            still_live.then(|| LeaseReplacement {
                lease_id: held.id,
                revoke_key: SecretBytes::new(*held.revoke_key.expose_bytes()),
            })
        });
        let response = wire::board_sync(
            &self.net,
            &BoardSyncRequest {
                lease: built.lease,
                replaces,
                read: if read {
                    ReadBoard::Read
                } else {
                    ReadBoard::Skip
                },
            },
        )
        .await?;
        let own = OwnRecords::new(built.slots.iter().map(|slot| slot.randomness).collect());
        self.lease = Some(HeldLease {
            built,
            id: response.lease_id,
            revoke_key,
        });
        let mut records = Vec::new();
        let mut matches = Vec::new();
        let mut pages = 0_usize;
        let mut next = response.page;
        while let Some(page) = next {
            pages = pages.saturating_add(1);
            matches
                .extend(TrialMatcher::new(self.spot.as_slice(), bucket).match_page(&page, &own)?);
            records.extend_from_slice(&page.records);
            next = match &page.next {
                Some(cursor) => Some(wire::board_page(&self.net, cursor).await?),
                None => None,
            };
        }
        Ok(SyncReport {
            matches,
            records,
            pages,
            lease_id: response.lease_id,
        })
    }

    /// `DELETE /v1/board/lease/{id}`: takes the live lease off the board at once.
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] with no lease; transport or status errors.
    pub async fn revoke_lease(&mut self) -> Result<(), E2eError> {
        let held = self
            .lease
            .take()
            .ok_or_else(|| E2eError::step(format!("{} holds no lease", self.name)))?;
        wire::revoke_lease(&self.net, &held.id, &held.revoke_key).await
    }

    // -----------------------------------------------------------------------------------------
    // Knocks

    /// Knocks the records `knock_targets` picks from `matches` (at most 3, preferring Meetup
    /// hints, skipping fellow members): each knock carries a fresh Meetup KeyPackage, a fresh
    /// reply mailbox and a fresh sealed subscription. Returns how many were posted.
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] when not inside; transport, MLS or protocol errors.
    pub async fn knock(
        &mut self,
        stack: &Stack,
        matches: &[BoardMatch],
    ) -> Result<usize, E2eError> {
        if self.spot.is_none() {
            return Err(E2eError::step(format!("{} is not inside", self.name)));
        }
        let own_meetups: Vec<GroupId> = self.meetup().into_iter().collect();
        let targets: Vec<BoardMatch> = knock_targets(
            matches,
            &KnockFilter {
                already_knocked: &self.knocked_mailboxes,
                own_meetups: &own_meetups,
            },
        )
        .into_iter()
        .cloned()
        .collect();
        let (party_group, _) = self.party()?;
        let own_leaf = self.context().party_group(&party_group)?.own_leaf();
        for target in &targets {
            let reply = self
                .register_mailbox(stack, MailboxPurpose::Reply, PushClass::Standard)
                .await?;
            let grant = GrantKeyPair::generate(&mut self.rng)?;
            let key_package = MlsContext::new(&self.storage, &self.key).create_key_package(
                KeyPackagePurpose::Meetup {
                    grant_key: &grant.public,
                },
                SUITE,
                &mut self.rng,
            )?;
            let subscription = self.subscription(stack)?;
            let subscription_id = subscription.id;
            let own_record = self.own_record();
            let spot = self
                .spot
                .as_ref()
                .ok_or_else(|| E2eError::step("not inside"))?;
            let knock = build_knock(
                &KnockInputs {
                    matched: target,
                    spot,
                    own_record,
                    own_leaf,
                    key_package,
                    reply_mailbox: reply.id,
                    meetup_subscription: Some(subscription),
                    signing_key: &self.key,
                    class: SizeClass::Bytes1024,
                },
                &mut self.rng,
            )?;
            wire::post_to_mailbox(&self.net, &knock.mailbox, knock.post).await?;
            self.knocked_mailboxes.push(target.beacon.knock_mailbox);
            self.knocked_records.push(target.record.randomness);
            self.pending_joins.push(PendingJoin {
                reply,
                target: target.clone(),
                grant,
                subscription: subscription_id,
            });
        }
        Ok(targets.len())
    }

    /// Woken for its knock mailbox: reads the new posts, opens and verifies each knock against
    /// the Party roster, re-checks liveness, and decides. Without the Spot's keys (the device
    /// left) nothing can be opened and nothing is answered.
    ///
    /// # Errors
    ///
    /// Transport, status, MLS or protocol errors (a knock that fails to open or verify is an
    /// error here: the scenarios post no garbage).
    pub async fn answer(&mut self) -> Result<Vec<Answer>, E2eError> {
        let (Some(spot), Some(own_record), Some(held), Some(mailbox)) = (
            self.spot.as_ref(),
            self.own_record(),
            self.lease.as_ref(),
            self.knock_mailbox.as_ref(),
        ) else {
            return Ok(Vec::new());
        };
        let messages =
            wire::read_mailbox(&self.net, &mailbox.id, &mailbox.read_key, mailbox.last_read)
                .await?;
        let (party_group, _) = self.party()?;
        let group = self.context().party_group(&party_group)?;
        let roster = EpochRoster {
            roster: group.roster(),
            epoch: group.epoch(),
        };
        let context = KnockContext {
            spot,
            start: held.built.lease.start,
            own_record,
        };
        let evidence = self.evidence()?;
        let current_meetup = self.meetup();
        let mut answers = Vec::new();
        let mut last = mailbox.last_read;
        for message in &messages.messages {
            last = Some(message.sequence);
            let verified = verify_knock(
                &context,
                open_knock(&context, message.body.as_bytes())?,
                &roster,
            )?;
            let decision = decide_knock(
                &verified,
                &evidence,
                &KnockSituation {
                    own_record: &own_record,
                    knocked_records: &self.knocked_records,
                    current_meetup,
                },
            );
            answers.push(Answer {
                knock: verified,
                decision,
            });
        }
        if let Some(mailbox) = self.knock_mailbox.as_mut() {
            mailbox.last_read = last;
        }
        Ok(answers)
    }

    // -----------------------------------------------------------------------------------------
    // Meetups

    fn psk_binding_parts(
        &self,
        spot: SpotId,
        epoch: Epoch,
        bucket: Bucket,
    ) -> Result<(MeetupPskId, pingers_crypto::MeetupPsk, Roster), E2eError> {
        let (party_group, party_id) = self.party()?;
        let group = self.context().party_group(&party_group)?;
        if group.epoch() != epoch {
            return Err(E2eError::step("the Party epoch moved since the knock"));
        }
        let psk = derive_meetup_psk(
            &derive_spot_key(&group.presence_root(&party_id)?, &spot)?,
            bucket,
        )?;
        Ok((
            MeetupPskId {
                party: party_id,
                epoch,
                spot,
                bucket,
            },
            psk,
            group.roster(),
        ))
    }

    async fn post_welcome(
        &self,
        reply: &MailboxId,
        welcome: &WelcomeBytes,
    ) -> Result<(), E2eError> {
        let padded = Padded::pad_smallest(welcome.as_bytes(), &SizeClass::MAILBOX_POST)?;
        wire::post_to_mailbox(&self.net, reply, padded.into_bytes())
            .await
            .map(drop)
    }

    /// Accepts a knock by creating the Meetup (design G.4): a new MLS group bound to the Party
    /// and Spot by `PSK_meet(b)`, the knocker added in the first commit, the group registered
    /// with both sealed subscriptions, and the Welcome posted to the knocker's reply mailbox.
    ///
    /// # Errors
    ///
    /// MLS, delivery, transport or protocol errors.
    pub async fn create_meetup(
        &mut self,
        stack: &Stack,
        answer: &Answer,
    ) -> Result<GroupId, E2eError> {
        self.create_meetup_with(stack, answer, CreationLog::PostCreationCommit)
            .await
    }

    /// [`Device::create_meetup`] exactly as design G.4 draws it: `POST /groups` and the
    /// Welcome, with no commit in the group log. The log then starts at epoch 0 while the
    /// members are at epoch 1 (see the scenario test that shows the resulting conflict).
    ///
    /// # Errors
    ///
    /// As [`Device::create_meetup`].
    pub async fn create_meetup_as_drawn(
        &mut self,
        stack: &Stack,
        answer: &Answer,
    ) -> Result<GroupId, E2eError> {
        self.create_meetup_with(stack, answer, CreationLog::Nothing)
            .await
    }

    async fn create_meetup_with(
        &mut self,
        stack: &Stack,
        answer: &Answer,
        log: CreationLog,
    ) -> Result<GroupId, E2eError> {
        let place = self.place.ok_or_else(|| E2eError::step("no Spot"))?;
        let start = self
            .lease
            .as_ref()
            .map(|held| held.built.lease.start)
            .ok_or_else(|| E2eError::step("no lease"))?;
        let (psk_id, psk, roster) =
            self.psk_binding_parts(place.spot, answer.knock.knock.party_epoch, start)?;
        let own_subscription = self.subscription(stack)?;
        let grant = GrantKeyPair::generate(&mut self.rng)?;
        let group_id = GroupId::from_bytes(random_array(&mut self.rng));
        let knocker_subscription = answer.knock.knock.meetup_subscription.clone();
        let mut subscriptions = vec![own_subscription.clone()];
        subscriptions.extend(knocker_subscription.clone());
        let (welcome, own_sequences) = {
            let context = MlsContext::new(&self.storage, &self.key);
            let created = context.create_meetup_group(
                &MeetupCreation {
                    group_id,
                    psk: MeetupPskBinding {
                        id: psk_id,
                        value: &psk,
                    },
                    party_roster: &roster,
                    first_member: &answer.knock.knock.key_package,
                    own_grant_key: &grant.public,
                    suite: SUITE,
                },
                &mut self.rng,
            )?;
            let mut group = created.group;
            let welcome = created
                .commit
                .welcome
                .ok_or_else(|| E2eError::step("no Welcome"))?;
            let mut own_sequences = Vec::new();
            match log {
                CreationLog::Nothing => {
                    group.merge_pending_commit()?;
                    stack.delivery().create_group(CreateGroup {
                        group_id,
                        auth_key: group.log_auth_key_pair()?.public_key(),
                        subscriptions,
                        state: None,
                    })?;
                }
                CreationLog::PostCreationCommit => {
                    // The delivery log starts at epoch 0, where the group was before its first
                    // commit; posting that commit brings the log to the members' epoch.
                    stack.delivery().create_group(CreateGroup {
                        group_id,
                        auth_key: group.log_auth_key_pair()?.public_key(),
                        subscriptions,
                        state: None,
                    })?;
                    let request = CommitRequest {
                        epoch: group.epoch(),
                        commit: created.commit.commit.clone(),
                        next_auth_key: group.pending_log_auth_key_pair()?.public_key(),
                        state: None,
                        subscriptions_added: Vec::new(),
                        subscriptions_removed: Vec::new(),
                        wake: Urgency::Alert,
                    };
                    match stack.delivery().commit(&group_id, request, &mut self.rng)? {
                        pingers_delivery::CommitOutcome::Accepted { sequence, .. } => {
                            own_sequences.push(sequence);
                        }
                        pingers_delivery::CommitOutcome::Conflict { .. } => {
                            return Err(E2eError::step("a new group's first commit conflicted"));
                        }
                    }
                    group.merge_pending_commit()?;
                }
            }
            (welcome, own_sequences)
        };
        self.post_welcome(&answer.knock.knock.reply_mailbox, &welcome)
            .await?;
        self.meetup = Some(MeetupState {
            group: group_id,
            _grant: grant,
            own_subscription: own_subscription.id,
            member_subscriptions: knocker_subscription
                .map(|subscription| (answer.knock.knocker_key, subscription.id))
                .into_iter()
                .collect(),
            cursor: SequenceNumber::new(0),
            own_sequences,
            leavers: Vec::new(),
        });
        Ok(group_id)
    }

    /// Accepts a knock by adding the knocker to this device's Meetup (design G.5): one commit
    /// per epoch wins at the delivery service; a loser drops its commit. Commits at the epoch
    /// the device is at (call [`Device::catch_up`] first to commit on top of the log).
    ///
    /// # Errors
    ///
    /// MLS, delivery, transport or protocol errors.
    pub async fn add_to_meetup(
        &mut self,
        stack: &Stack,
        answer: &Answer,
    ) -> Result<KnockOutcome, E2eError> {
        let group_id = self.meetup_state()?.group;
        let (party_group, _) = self.party()?;
        let party_roster = self.context().party_group(&party_group)?.roster();
        let subscription = answer.knock.knock.meetup_subscription.clone();
        let (outcome, welcome) = {
            let context = MlsContext::new(&self.storage, &self.key);
            let mut group = context.meetup_group(&group_id)?;
            let epoch = group.epoch();
            let commit = group.add_members(
                core::slice::from_ref(&answer.knock.knock.key_package),
                &party_roster,
                &mut self.rng,
            )?;
            let request = CommitRequest {
                epoch,
                commit: commit.commit.clone(),
                next_auth_key: group.pending_log_auth_key_pair()?.public_key(),
                state: None,
                subscriptions_added: subscription.iter().cloned().collect(),
                subscriptions_removed: Vec::new(),
                wake: Urgency::Alert,
            };
            match stack.delivery().commit(&group_id, request, &mut self.rng)? {
                pingers_delivery::CommitOutcome::Accepted { sequence, .. } => {
                    group.merge_pending_commit()?;
                    (Some(sequence), commit.welcome)
                }
                pingers_delivery::CommitOutcome::Conflict { .. } => {
                    group.discard_pending_commit()?;
                    (None, None)
                }
            }
        };
        let Some(sequence) = outcome else {
            return Ok(KnockOutcome::Conflict);
        };
        let welcome = welcome.ok_or_else(|| E2eError::step("no Welcome"))?;
        self.post_welcome(&answer.knock.knock.reply_mailbox, &welcome)
            .await?;
        if let Some(meetup) = self.meetup.as_mut() {
            meetup.own_sequences.push(sequence);
            meetup.member_subscriptions.extend(
                subscription.map(|subscription| (answer.knock.knocker_key, subscription.id)),
            );
        }
        Ok(KnockOutcome::Accepted)
    }

    /// After knocking: reads the reply mailboxes and joins the Meetup from the first Welcome,
    /// checking `PSK_meet(b)` and that every member is in the Party roster. Returns whether it
    /// joined.
    ///
    /// # Errors
    ///
    /// Transport, MLS or protocol errors.
    pub async fn join_meetup(&mut self) -> Result<bool, E2eError> {
        for index in 0..self.pending_joins.len() {
            let Some(pending) = self.pending_joins.get(index) else {
                break;
            };
            let messages =
                wire::read_mailbox(&self.net, &pending.reply.id, &pending.reply.read_key, None)
                    .await?;
            let Some(message) = messages.messages.first() else {
                continue;
            };
            let welcome = WelcomeBytes::from_bytes(message.body.payload()?.to_vec());
            let (psk_id, psk, roster) = self.psk_binding_parts(
                pending.target.spot.spot,
                pending.target.epoch,
                pending.target.start,
            )?;
            let group_id = {
                let group = MlsContext::new(&self.storage, &self.key).join_meetup_group(
                    &welcome,
                    &MeetupPskBinding {
                        id: psk_id,
                        value: &psk,
                    },
                    &roster,
                )?;
                group.group_id()
            };
            let pending = self.pending_joins.remove(index);
            self.retired_mailboxes.push(pending.reply);
            self.meetup = Some(MeetupState {
                group: group_id,
                _grant: pending.grant,
                own_subscription: pending.subscription,
                member_subscriptions: Vec::new(),
                cursor: SequenceNumber::new(0),
                own_sequences: Vec::new(),
                leavers: Vec::new(),
            });
            return Ok(true);
        }
        Ok(false)
    }

    /// Processes the Meetup's delivery log since the last call: other members' commits are
    /// merged, leave proposals stored. Entries from before this device's epoch are skipped.
    /// Returns how many entries were processed.
    ///
    /// # Errors
    ///
    /// Delivery or MLS errors.
    pub fn catch_up(&mut self, stack: &Stack) -> Result<usize, E2eError> {
        let Some(meetup) = self.meetup.as_ref() else {
            return Ok(0);
        };
        let group_id = meetup.group;
        let log = stack.delivery().messages(&group_id, meetup.cursor)?;
        let mut processed = 0_usize;
        let mut cursor = meetup.cursor;
        let mut leavers = Vec::new();
        let mut removed = false;
        {
            let context = MlsContext::new(&self.storage, &self.key);
            let mut group = context.meetup_group(&group_id)?;
            for entry in &log.messages {
                cursor = entry.sequence;
                if meetup.own_sequences.contains(&entry.sequence) || entry.epoch < group.epoch() {
                    continue;
                }
                match group.process(&entry.body)? {
                    ProcessedMessage::Commit(summary) => {
                        removed |= summary.self_removal == SelfRemoval::Removed;
                    }
                    ProcessedMessage::Proposal { sender } => leavers.push(sender),
                    ProcessedMessage::Application { .. } | _ => {}
                }
                processed = processed.saturating_add(1);
                if removed {
                    break;
                }
            }
        }
        if removed {
            self.context().delete_group(&group_id)?;
            self.meetup = None;
            return Ok(processed);
        }
        if let Some(meetup) = self.meetup.as_mut() {
            meetup.cursor = cursor;
            meetup.leavers.extend(leavers);
        }
        Ok(processed)
    }

    /// Leaves the Meetup (design G.6): a self-remove proposal goes to the group, then every
    /// bit of the group's MLS state is deleted from this device.
    ///
    /// # Errors
    ///
    /// Delivery or MLS errors.
    pub fn leave_meetup(&mut self, stack: &Stack) -> Result<GroupId, E2eError> {
        self.catch_up(stack)?;
        let group_id = self.meetup_state()?.group;
        {
            let context = MlsContext::new(&self.storage, &self.key);
            let mut group = context.meetup_group(&group_id)?;
            let epoch = group.epoch();
            let proposal = group.propose_leave(&mut self.rng)?;
            let posted = stack.delivery().post(
                &group_id,
                AppMessage {
                    epoch,
                    body: proposal,
                    delivery: pingers_proto::Delivery::Stored,
                    wake: None,
                },
                &mut self.rng,
            )?;
            drop(posted);
            context.delete_group(&group_id)?;
        }
        self.meetup = None;
        Ok(group_id)
    }

    /// Commits the leave proposals seen so far, removing the leavers' subscriptions (design
    /// G.6, G.7).
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] with nothing to commit; delivery or MLS errors.
    pub fn commit_removals(&mut self, stack: &Stack) -> Result<KnockOutcome, E2eError> {
        self.catch_up(stack)?;
        let meetup = self.meetup_state()?;
        if meetup.leavers.is_empty() {
            return Err(E2eError::step("no leave proposal to commit"));
        }
        let group_id = meetup.group;
        let sequence = {
            let context = MlsContext::new(&self.storage, &self.key);
            let mut group = context.meetup_group(&group_id)?;
            let roster = group.roster();
            let removed_keys: Vec<AccountPublicKey> = meetup
                .leavers
                .iter()
                .filter_map(|leaf| roster.signature_key(*leaf).copied())
                .collect();
            let subscriptions_removed = meetup
                .member_subscriptions
                .iter()
                .filter(|(key, _)| removed_keys.contains(key))
                .map(|(_, id)| *id)
                .collect();
            let epoch = group.epoch();
            let commit = group.commit_pending_proposals(&mut self.rng)?;
            let request = CommitRequest {
                epoch,
                commit: commit.commit,
                next_auth_key: group.pending_log_auth_key_pair()?.public_key(),
                state: None,
                subscriptions_added: Vec::new(),
                subscriptions_removed,
                wake: Urgency::Quiet,
            };
            match stack.delivery().commit(&group_id, request, &mut self.rng)? {
                pingers_delivery::CommitOutcome::Accepted { sequence, .. } => {
                    group.merge_pending_commit()?;
                    Some(sequence)
                }
                pingers_delivery::CommitOutcome::Conflict { .. } => {
                    group.discard_pending_commit()?;
                    None
                }
            }
        };
        let Some(sequence) = sequence else {
            return Ok(KnockOutcome::Conflict);
        };
        if let Some(meetup) = self.meetup.as_mut() {
            meetup.own_sequences.push(sequence);
            meetup.leavers.clear();
        }
        Ok(KnockOutcome::Accepted)
    }

    /// Ends the Meetup as its last member (design G.7): `DELETE /v1/groups/{id}`, then every bit
    /// of its MLS state is deleted from this device.
    ///
    /// # Errors
    ///
    /// Delivery or MLS errors.
    pub fn end_meetup(&mut self, stack: &Stack) -> Result<GroupId, E2eError> {
        let group_id = self.meetup_state()?.group;
        stack.delivery().delete_group(&group_id)?;
        self.context().delete_group(&group_id)?;
        self.meetup = None;
        Ok(group_id)
    }

    /// This device's own subscription id in its Meetup.
    ///
    /// # Errors
    ///
    /// [`E2eError::Step`] outside a Meetup.
    pub fn own_subscription(&self) -> Result<SubscriptionId, E2eError> {
        Ok(self.meetup_state()?.own_subscription)
    }
}

/// Forms a Party of `devices` over MLS (design G.1 without the server: the group routes are not
/// wired): the first creates the group and adds the others in one commit; they join from the
/// Welcome. Every device then shares the Party's presence exporter.
///
/// # Errors
///
/// [`E2eError::Step`] with no devices; MLS errors.
pub fn form_party(devices: &mut [&mut Device], seed: u64) -> Result<PartyId, E2eError> {
    let mut rng = seeded_rng(seed);
    let party_group = GroupId::from_bytes(random_array(&mut rng));
    let party_id = PartyId::from_bytes(random_array(&mut rng));
    let (creator, others) = devices
        .split_first_mut()
        .ok_or_else(|| E2eError::step("a Party needs a member"))?;
    let mut key_packages = Vec::new();
    for device in others.iter_mut() {
        key_packages.push(
            MlsContext::new(&device.storage, &device.key).create_key_package(
                KeyPackagePurpose::Party,
                SUITE,
                &mut device.rng,
            )?,
        );
    }
    let welcome = {
        let context = MlsContext::new(&creator.storage, &creator.key);
        let mut group = context.create_party_group(party_group, SUITE, &mut creator.rng)?;
        if key_packages.is_empty() {
            None
        } else {
            let commit = group.add_members(&key_packages, &mut creator.rng)?;
            group.merge_pending_commit()?;
            commit.welcome
        }
    };
    creator.party = Some((party_group, party_id));
    for device in others.iter_mut() {
        let welcome = welcome
            .as_ref()
            .ok_or_else(|| E2eError::step("no Welcome"))?;
        MlsContext::new(&device.storage, &device.key).join_party_group(welcome)?;
        device.party = Some((party_group, party_id));
    }
    Ok(party_id)
}
