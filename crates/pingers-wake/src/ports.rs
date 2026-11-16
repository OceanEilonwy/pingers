// SPDX-License-Identifier: AGPL-3.0-only
//! What a wake needs from the rest of the client, as small traits.
//!
//! pingers-client implements each one by delegating to one crate: [`DevicePort`] to the shell's
//! `Platform`, [`PartyView`] to pingers-parties, [`ServerPort`] to pingers-sync and
//! [`MeetupPort`] to pingers-meetup-session. Tests implement them with fakes.

use core::future::Future;
use core::time::Duration;

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot, RevokeKey};
use pingers_gate::FocusState;
use pingers_geo::{Circle, Fence, Fix};
use pingers_presence::{
    Admission, EpochState, MailboxPurpose, MeetupKey, OutgoingKnock, SpotRef, VerifiedKnock,
};
use pingers_proto::{
    BoardCursor, BoardPage, Epoch, GroupId, KeyPackageBytes, LeafIndex, Lease, MailboxId, PartyId,
    ReadBoard, ServerId, SizeClass, SpotId,
};
use pingers_time::Bucket;

use crate::PortError;

/// How hard the platform should try for a fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixQuality {
    /// Balanced power and accuracy: the pipeline's routine fix (design E.2).
    Balanced,
    /// GPS quality: the liveness check before answering a knock (design B.4.4).
    High,
}

/// The location mode a wake asks for. Meetup modes belong to pingers-meetup-session; a wake
/// leaves the mode alone while a Meetup is live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationMode {
    /// Region monitoring only.
    Idle,
    /// Inside a Spot, no continuous GPS.
    Dwell,
}

/// What an OS geofence stands for, so its transitions can be routed back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionId {
    /// A Spot's region.
    Spot(SpotRef),
    /// The horizon region: leaving it means "choose the geofences again" (design E.2).
    Horizon,
}

/// One OS geofence to register.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OsRegion {
    /// Reported back with each transition.
    pub id: RegionId,
    /// The circle.
    pub region: Circle,
}

/// The shell, as a wake sees it.
pub trait DevicePort: Send + Sync {
    /// The interruption filter, read fresh (design B.10).
    fn focus_state(&self) -> FocusState;

    /// Takes a fix, or `None` if the platform has none within `timeout`. The wake also bounds
    /// the wait by its budget and drops the future when the budget runs out.
    fn request_fix(
        &self,
        quality: FixQuality,
        timeout: Duration,
    ) -> impl Future<Output = Option<Fix>> + Send;

    /// Replaces every registered OS geofence.
    fn set_os_geofences(&self, regions: &[OsRegion]);

    /// Switches the location mode.
    fn set_location_mode(&self, mode: LocationMode);

    /// Schedules the next background wake.
    fn schedule_wake(&self, after: Duration);
}

/// One Spot of a Party, as the wake needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotInfo {
    /// The Spot.
    pub spot: SpotId,
    /// Its fence.
    pub fence: Fence,
}

/// One Party, as the wake needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct PartyInfo {
    /// The Party.
    pub party: PartyId,
    /// The server the Party lives on: its board, mailboxes and group log.
    pub server: ServerId,
    /// Its newest processed epoch and how it was reached (for the grace period, B.4.7).
    pub epoch: EpochState,
    /// This device's leaf in the Party group.
    pub own_leaf: LeafIndex,
    /// The knock size class: 1 KiB classical, 4 KiB X-Wing.
    pub knock_class: SizeClass,
    /// The Party's Spots.
    pub spots: Vec<SpotInfo>,
}

/// A read-only view of Parties and their keys (pingers-parties).
pub trait PartyView: Send + Sync {
    /// Every Party with its Spots, after the latest group-log sync.
    fn parties(&self) -> Vec<PartyInfo>;

    /// The Party's presence root at `epoch` (the MLS exporter), if this device still has it.
    fn presence_root(&self, party: PartyId, epoch: Epoch) -> Option<PresenceRoot>;

    /// The signature key of the member at `leaf` in the Party's roster at `epoch`.
    fn signature_key(
        &self,
        party: PartyId,
        leaf: LeafIndex,
        epoch: Epoch,
    ) -> Option<AccountPublicKey>;

    /// This device's account key, which signs knocks.
    fn signing_key(&self) -> &AccountSigningKey;

    /// A fresh Meetup `KeyPackage` for a knock in this Party.
    ///
    /// # Errors
    ///
    /// [`PortError::Unavailable`] if MLS state cannot produce one.
    fn key_package(&self, party: PartyId) -> Result<KeyPackageBytes, PortError>;
}

/// A lease to upload with `POST /v1/board/sync`. pingers-sync keeps the revoke key, so the next
/// upload can replace this lease.
pub struct LeaseUpload {
    /// The lease.
    pub lease: Lease,
    /// Its revoke key.
    pub revoke_key: RevokeKey,
    /// Whether to read the board after the append.
    pub read: ReadBoard,
}

/// The servers, through pingers-sync (anonymous requests through OHTTP). Every call names the
/// server it is for: each server has its own board, mailboxes, mailbox pool and group logs.
pub trait ServerPort: Send + Sync {
    /// Pulls and processes the group log of every Party on `server` (design B.4.7: before
    /// every board sync).
    fn pull_group_logs(
        &self,
        server: &ServerId,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Uploads a lease to `server`'s board, replacing this device's previous lease there, and
    /// returns the first board page if a read was asked for.
    fn upload_lease(
        &self,
        server: &ServerId,
        upload: LeaseUpload,
    ) -> impl Future<Output = Result<Option<BoardPage>, PortError>> + Send;

    /// The page of `server`'s board at `cursor`.
    fn board_page(
        &self,
        server: &ServerId,
        cursor: BoardCursor,
    ) -> impl Future<Output = Result<BoardPage, PortError>> + Send;

    /// Posts a knock to its mailbox on `server`.
    fn post_knock(
        &self,
        server: &ServerId,
        knock: OutgoingKnock,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Every post in one of this device's mailboxes on `server` (pingers-sync holds the read
    /// keys).
    fn read_mailbox(
        &self,
        server: &ServerId,
        mailbox: MailboxId,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, PortError>> + Send;

    /// Takes a registered mailbox on `server` from its pool for `purpose` that lives at least
    /// as long as a lease starting in `lease_start`, or `None` if the pool has none.
    fn take_mailbox(
        &self,
        server: &ServerId,
        purpose: MailboxPurpose,
        lease_start: Bucket,
    ) -> Option<MailboxId>;

    /// Makes at most one push registration on `server` (design E.2, F.3): seals one stale
    /// mailbox or subscription there again (a new push token or relay key, pushes switched on
    /// or off, a seal running out), or else registers one new mailbox for its pool. One per
    /// server per wake, so a server cannot group a device's registrations by when they were
    /// made.
    fn maintain_push_registrations(
        &self,
        server: &ServerId,
    ) -> impl Future<Output = Result<(), PortError>> + Send;
}

/// An accepted knock, for pingers-meetup-session to create or extend the Meetup group and post
/// the Welcome to the knocker's reply mailbox.
///
/// Not `Clone` or `Debug`: it carries `PSK_meet` (style guide §8).
pub struct MeetupAdmission {
    /// `PSK_meet(b)` and its id `(party, epoch, spot, b)`, where the epoch is that of the
    /// record the knock answered and `b` its start bucket (design B.5). The knocker derives the
    /// same key from the same record.
    pub key: MeetupKey,
    /// Create a Meetup, or add to the current one.
    pub admission: Admission,
    /// The verified knock: key package, reply mailbox, subscription, knocker key.
    pub knock: VerifiedKnock,
}

/// A post read from the reply mailbox of a knock this device sent: the Welcome. Handed on only
/// while the device is still Inside the Spot, since the key comes from its Spot keys.
///
/// Not `Clone` or `Debug`: it carries `PSK_meet` (style guide §8).
pub struct ReplyPost {
    /// `PSK_meet(b)` and its id for the record the knock answered: the Spot, the epoch the
    /// knock was sent under, and the record's start bucket.
    pub key: MeetupKey,
    /// The post.
    pub post: Vec<u8>,
}

/// Live Meetups (pingers-meetup-session).
pub trait MeetupPort: Send + Sync {
    /// The Meetup this device is in at `spot`, if any.
    fn current_meetup(&self, spot: SpotRef) -> Option<GroupId>;

    /// Admits an accepted knocker.
    fn admit(
        &self,
        admission: MeetupAdmission,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Joins from a Welcome.
    fn welcome(&self, reply: ReplyPost) -> impl Future<Output = Result<(), PortError>> + Send;

    /// The device left `spot`: leave the Meetup there (design B.5).
    fn leave(&self, spot: SpotRef) -> impl Future<Output = Result<(), PortError>> + Send;

    /// Sharing is suppressed: leave every Meetup (design B.10, B.11).
    fn leave_all(&self) -> impl Future<Output = Result<(), PortError>> + Send;
}
