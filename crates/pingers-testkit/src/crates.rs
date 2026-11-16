// SPDX-License-Identifier: AGPL-3.0-only
//! [`FakeCrates`]: recording stand-ins for the four L3 crates behind pingers-client's ports
//! (feature `client`).
//!
//! Every port call is recorded as a [`CrateCall`] with its arguments and answered from the
//! fake's state: fixed ids for commands, the scripted [`WakeOutcome`] for wakes, and the
//! sections a test sets for snapshots. A test can make one crate fail to check that the
//! client passes the error on.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use pingers_client::ports::{
    AllowListChange, ClientEvent, JoinDecision, MapDownload, MapHosts, MapRoute, MapsPort,
    MeetupsPort, MeetupsSection, PartiesPort, PartiesSection, PartyDraft, PipelineSection,
    ServerSection, SettingChange, SpotDraft, SyncPort, WakeInput, WakeOutcome, WakePort,
};
use pingers_client::snapshot::{
    DailyWindow, HideTrail, InviteLink, InviteUrl, Name, PauseInFocus, QuietHoursPreference,
    RelayStatus, SafetyCode, Settings, SnapshotError, Toggle, WakePushes,
};
use pingers_client::{ClientError, Crates, PushTokens};
use pingers_gate::{FocusState, GateDecision, GateError, LocationSharing, MinuteOfDay};
use pingers_map_downloads::catalog::Catalog;
use pingers_map_downloads::install::DownloadProgress;
use pingers_map_downloads::{MapsError, RegionManifest};
use pingers_meetup::{LocationAnswer, LocationVisibility, MeetPointDraft};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, PartyId, SecretText,
    SpotId,
};
use pingers_time::UnixSeconds;

/// Why [`FakeCrates::new`] could not build its default sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FakeCratesError {
    /// A gate value.
    #[error(transparent)]
    Gate(#[from] GateError),
    /// A snapshot value.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
}

/// Which L3 crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CrateName {
    /// pingers-parties.
    Parties,
    /// pingers-sync.
    Sync,
    /// pingers-wake.
    Wake,
    /// pingers-meetup-session.
    Meetups,
    /// pingers-map-downloads.
    Maps,
}

/// One port call and its arguments.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    missing_docs,
    reason = "each variant is the port method of the same name"
)]
pub enum CrateCall {
    CreateParty(PartyDraft),
    CreateInvite(PartyId),
    OpenInvite(InviteUrl, Name),
    WithdrawJoin,
    DismissJoin,
    CheckJoins,
    DecideJoin(PartyId, MailboxId, JoinDecision),
    CancelInvite(PartyId, MailboxId),
    SetDisplayName(Name),
    RemoveMember(PartyId, LeafIndex),
    LeaveParty(PartyId),
    SaveSpot(PartyId, SpotDraft),
    DeleteSpot(PartyId, SpotId),
    SetAlwaysAllow(PartyId, LeafIndex, AllowListChange),
    SafetyCode(PartyId, LeafIndex),
    MarkChecked(PartyId, LeafIndex),
    /// The code, exposed: the fake keeps no secrets.
    RegisterWithCode(String),
    PushTokensChanged(Option<PushTokens>),
    SetWakePushes(WakePushes, Option<PushTokens>),
    ReportPushPriority(pingers_wake::PushPriority, pingers_wake::PushPriority),
    CheckReachability,
    Wake(WakeInput),
    ChangeSetting(SettingChange),
    SendChat(GroupId, ChatBody, Option<ChatMessageId>),
    SetMeetPoint(GroupId, MeetPointDraft),
    AskLocation(GroupId, LeafIndex),
    AnswerLocationRequest(GroupId, LocationRequestId, LocationAnswer),
    RevokeLocation(GroupId, LeafIndex),
    LeaveMeetup(GroupId),
    MapCatalog(MapHosts, MapRoute),
    DownloadMap(MapDownload),
    InstalledMaps(PathBuf),
    DeleteMap(PathBuf, String),
    ForgetMapHostKey,
}

/// The four fake crates. Clones share state.
#[derive(Clone, Debug)]
pub struct FakeCrates {
    state: Arc<Mutex<State>>,
}

#[derive(Debug)]
struct State {
    calls: Vec<CrateCall>,
    failing: Option<CrateName>,
    outcome: WakeOutcome,
    parties: PartiesSection,
    pipeline: PipelineSection,
    server: ServerSection,
    meetups: MeetupsSection,
    section_reads: usize,
    /// Where [`FakeCrates::raise`] sends the Meetup events the client forwards.
    raised: tokio::sync::mpsc::UnboundedSender<ClientEvent>,
    /// Handed to the client once, by `take_events`.
    raised_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<ClientEvent>>,
    priority_notice: Option<pingers_client::Notice>,
}

impl FakeCrates {
    /// The id every `create_party` returns.
    pub const PARTY: PartyId = PartyId::from_bytes([0xa1; 16]);
    /// The code every `safety_code` returns.
    pub const SAFETY_CODE: &str =
        "48210 93755 12064 38817 70459 21386 66102 94573 15028 83941 57260 30694";
    /// The id every `save_spot` returns.
    pub const SPOT: SpotId = SpotId::from_bytes([0xb2; 16]);
    /// The id every `send_chat` returns.
    pub const MESSAGE: ChatMessageId = ChatMessageId::from_bytes([0xc3; 16]);
    /// The id every `ask_location` returns.
    pub const REQUEST: LocationRequestId = LocationRequestId::from_bytes([0xd4; 16]);
    /// When every invite link from `create_invite` expires.
    pub const INVITE_EXPIRES: UnixSeconds = UnixSeconds::new(1_800_000_000);

    /// Fakes with no Parties, no Meetups, an empty wake outcome, sharing on with the gate
    /// answering `Share`, and a reachable relay.
    ///
    /// # Errors
    ///
    /// Never in practice: the default quiet-hours window is valid.
    pub fn new() -> Result<Self, FakeCratesError> {
        let settings = Settings {
            location_sharing: LocationSharing::On,
            pause_in_focus: PauseInFocus::On,
            focus: FocusState::Off,
            quiet_hours: QuietHoursPreference {
                state: Toggle::Off,
                window: DailyWindow::new(MinuteOfDay::new(23 * 60)?, MinuteOfDay::new(7 * 60)?)?,
            },
            visibility: LocationVisibility::AskEachTime,
            wake_pushes: WakePushes::On,
            hide_trail: HideTrail::On,
        };
        let (raised_sender, raised_receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = State {
            calls: Vec::new(),
            failing: None,
            outcome: WakeOutcome::default(),
            parties: PartiesSection::default(),
            pipeline: PipelineSection {
                settings,
                gate: GateDecision::Share,
                zones: Vec::new(),
                own_position: None,
            },
            server: ServerSection {
                fallbacks: Vec::new(),
                relay: RelayStatus::Reachable,
                map_host: None,
                operators: None,
                push_delivery: pingers_client::snapshot::PushDelivery::Full,
            },
            meetups: MeetupsSection::default(),
            section_reads: 0,
            raised: raised_sender,
            raised_receiver: Some(raised_receiver),
            priority_notice: None,
        };
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
        })
    }

    /// The ports, for [`pingers_client::Client::open`]'s `build`.
    #[must_use]
    pub fn crates(&self) -> Crates {
        Crates {
            parties: Arc::new(self.clone()),
            sync: Arc::new(self.clone()),
            wake: Arc::new(self.clone()),
            meetups: Arc::new(self.clone()),
            maps: Arc::new(self.clone()),
        }
    }

    /// Every call so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<CrateCall> {
        self.lock().calls.clone()
    }

    /// How many times the snapshot sections were read (four reads per published snapshot).
    #[must_use]
    pub fn section_reads(&self) -> usize {
        self.lock().section_reads
    }

    /// Makes every call to one crate fail with its [`ClientError`] variant (or none).
    pub fn set_failing(&self, failing: Option<CrateName>) {
        self.lock().failing = failing;
    }

    /// What every wake returns from now on.
    pub fn set_wake_outcome(&self, outcome: WakeOutcome) {
        self.lock().outcome = outcome;
    }

    /// pingers-parties' section from now on.
    pub fn set_parties(&self, section: PartiesSection) {
        self.lock().parties = section;
    }

    /// pingers-wake's section from now on.
    pub fn set_pipeline(&self, section: PipelineSection) {
        self.lock().pipeline = section;
    }

    /// pingers-sync's section from now on.
    pub fn set_server(&self, section: ServerSection) {
        self.lock().server = section;
    }

    /// What every later `report_push_priority` answers (`None` until set).
    pub fn set_priority_notice(&self, notice: Option<pingers_client::Notice>) {
        self.lock().priority_notice = notice;
    }

    /// pingers-meetup-session's section from now on.
    pub fn set_meetups(&self, section: MeetupsSection) {
        self.lock().meetups = section;
    }

    /// Raises `event` as a live Meetup would on its own: the client republishes the snapshot
    /// and tells the shell.
    pub fn raise(&self, event: ClientEvent) {
        let _sent = self.lock().raised.send(event);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records `call` and answers `value`, unless `crate_name` is failing.
    fn answer<T>(
        &self,
        crate_name: CrateName,
        call: CrateCall,
        value: T,
    ) -> Result<T, ClientError> {
        let mut state = self.lock();
        state.calls.push(call);
        if state.failing == Some(crate_name) {
            return Err(error(crate_name));
        }
        Ok(value)
    }

    fn section<T>(
        &self,
        crate_name: CrateName,
        read: impl FnOnce(&State) -> T,
    ) -> Result<T, ClientError> {
        let mut state = self.lock();
        state.section_reads = state.section_reads.saturating_add(1);
        if state.failing == Some(crate_name) {
            return Err(error(crate_name));
        }
        Ok(read(&state))
    }
}

/// A failure of `crate_name`, carrying an error of that crate's own type (pingers-sync has
/// none yet, so its failure is the client's `Unavailable::Sync`).
fn error(crate_name: CrateName) -> ClientError {
    match crate_name {
        CrateName::Parties => ClientError::Parties(pingers_parties::PartiesError::Internal),
        CrateName::Sync => ClientError::Unavailable(pingers_client::Unavailable::Sync),
        CrateName::Wake => ClientError::Wake(pingers_wake::WakeError::Presence(
            pingers_presence::PresenceError::NotInside,
        )),
        CrateName::Meetups => {
            ClientError::MeetupSession(pingers_meetup_session::SessionError::Internal)
        }
        CrateName::Maps => ClientError::Maps(MapsError::Internal),
    }
}

/// No map hosts: catalogs are empty and downloads find nothing; the maps directory is real.
#[async_trait]
impl MapsPort for FakeCrates {
    async fn catalog(&self, hosts: MapHosts, route: MapRoute) -> Result<Catalog, ClientError> {
        let call = CrateCall::MapCatalog(hosts, route);
        self.answer(CrateName::Maps, call, Catalog::default())
    }

    async fn download(
        &self,
        job: MapDownload,
        _progress: Arc<dyn DownloadProgress>,
    ) -> Result<RegionManifest, ClientError> {
        self.answer(CrateName::Maps, CrateCall::DownloadMap(job), ())?;
        Err(ClientError::Maps(MapsError::NotOffered))
    }

    async fn installed(&self, directory: PathBuf) -> Result<Vec<RegionManifest>, ClientError> {
        let call = CrateCall::InstalledMaps(directory.clone());
        self.answer(CrateName::Maps, call, ())?;
        Ok(pingers_map_downloads::install::installed(&directory).await?)
    }

    async fn delete(&self, directory: PathBuf, id: String) -> Result<(), ClientError> {
        let call = CrateCall::DeleteMap(directory.clone(), id.clone());
        self.answer(CrateName::Maps, call, ())?;
        Ok(pingers_map_downloads::install::delete(&directory, &id).await?)
    }

    async fn forget_key_pin(&self) -> Result<(), ClientError> {
        self.answer(CrateName::Maps, CrateCall::ForgetMapHostKey, ())
    }
}

#[async_trait]
impl PartiesPort for FakeCrates {
    async fn create_party(&self, draft: PartyDraft) -> Result<PartyId, ClientError> {
        self.answer(
            CrateName::Parties,
            CrateCall::CreateParty(draft),
            Self::PARTY,
        )
    }

    async fn create_invite(&self, party: PartyId) -> Result<InviteLink, ClientError> {
        let link = InviteLink {
            url: InviteUrl::new("https://pingers.test/join#secret")
                .map_err(|_invalid| error(CrateName::Parties))?,
            expires_at: Self::INVITE_EXPIRES,
        };
        self.answer(CrateName::Parties, CrateCall::CreateInvite(party), link)
    }

    async fn open_invite(&self, link: InviteUrl, my_name: Name) -> Result<(), ClientError> {
        self.answer(CrateName::Parties, CrateCall::OpenInvite(link, my_name), ())
    }

    async fn withdraw_join(&self) -> Result<(), ClientError> {
        self.answer(CrateName::Parties, CrateCall::WithdrawJoin, ())
    }

    async fn dismiss_join(&self) -> Result<(), ClientError> {
        self.answer(CrateName::Parties, CrateCall::DismissJoin, ())
    }

    async fn check_joins(&self) -> Result<Vec<ClientEvent>, ClientError> {
        self.answer(CrateName::Parties, CrateCall::CheckJoins, Vec::new())
    }

    async fn decide_join(
        &self,
        party: PartyId,
        request: MailboxId,
        decision: JoinDecision,
    ) -> Result<(), ClientError> {
        let call = CrateCall::DecideJoin(party, request, decision);
        self.answer(CrateName::Parties, call, ())
    }

    async fn cancel_invite(&self, party: PartyId, invite: MailboxId) -> Result<(), ClientError> {
        let call = CrateCall::CancelInvite(party, invite);
        self.answer(CrateName::Parties, call, ())
    }

    /// Records the call and shows the name in the section, as pingers-parties would.
    async fn set_display_name(&self, name: Name) -> Result<(), ClientError> {
        let call = CrateCall::SetDisplayName(name.clone());
        self.answer(CrateName::Parties, call, ())?;
        self.lock().parties.display_name = Some(name);
        Ok(())
    }

    async fn remove_member(&self, party: PartyId, member: LeafIndex) -> Result<(), ClientError> {
        self.answer(
            CrateName::Parties,
            CrateCall::RemoveMember(party, member),
            (),
        )
    }

    async fn leave_party(&self, party: PartyId) -> Result<(), ClientError> {
        self.answer(CrateName::Parties, CrateCall::LeaveParty(party), ())
    }

    async fn save_spot(&self, party: PartyId, spot: SpotDraft) -> Result<SpotId, ClientError> {
        self.answer(
            CrateName::Parties,
            CrateCall::SaveSpot(party, spot),
            Self::SPOT,
        )
    }

    async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), ClientError> {
        self.answer(CrateName::Parties, CrateCall::DeleteSpot(party, spot), ())
    }

    async fn set_always_allow(
        &self,
        party: PartyId,
        member: LeafIndex,
        choice: AllowListChange,
    ) -> Result<(), ClientError> {
        let call = CrateCall::SetAlwaysAllow(party, member, choice);
        self.answer(CrateName::Parties, call, ())
    }

    async fn safety_code(
        &self,
        party: PartyId,
        member: LeafIndex,
    ) -> Result<SafetyCode, ClientError> {
        let code = SafetyCode::new(Self::SAFETY_CODE).map_err(|_invalid| ClientError::Internal)?;
        self.answer(
            CrateName::Parties,
            CrateCall::SafetyCode(party, member),
            code,
        )
    }

    async fn mark_checked(&self, party: PartyId, member: LeafIndex) -> Result<(), ClientError> {
        self.answer(
            CrateName::Parties,
            CrateCall::MarkChecked(party, member),
            (),
        )
    }

    async fn section(&self) -> Result<PartiesSection, ClientError> {
        self.section(CrateName::Parties, |state| state.parties.clone())
    }
}

#[async_trait]
impl SyncPort for FakeCrates {
    async fn push_tokens_changed(&self, tokens: Option<PushTokens>) -> Result<(), ClientError> {
        self.answer(CrateName::Sync, CrateCall::PushTokensChanged(tokens), ())
    }

    async fn set_wake_pushes(
        &self,
        setting: WakePushes,
        tokens: Option<PushTokens>,
    ) -> Result<(), ClientError> {
        let call = CrateCall::SetWakePushes(setting, tokens);
        self.answer(CrateName::Sync, call, ())
    }

    /// Answers what [`FakeCrates::set_priority_notice`] last set.
    async fn report_push_priority(
        &self,
        original: pingers_wake::PushPriority,
        delivered: pingers_wake::PushPriority,
    ) -> Result<Option<pingers_client::Notice>, ClientError> {
        let notice = self.lock().priority_notice;
        let call = CrateCall::ReportPushPriority(original, delivered);
        self.answer(CrateName::Sync, call, notice)
    }

    /// Answers the relay status of the section [`FakeCrates::set_server`] last set.
    async fn check_reachability(&self) -> Result<RelayStatus, ClientError> {
        let relay = self.lock().server.relay;
        self.answer(CrateName::Sync, CrateCall::CheckReachability, relay)
    }

    async fn register_with_code(&self, code: SecretText) -> Result<(), ClientError> {
        let call = CrateCall::RegisterWithCode(code.expose().to_owned());
        self.answer(CrateName::Sync, call, ())
    }

    async fn section(&self) -> Result<ServerSection, ClientError> {
        self.section(CrateName::Sync, |state| state.server.clone())
    }
}

#[async_trait]
impl WakePort for FakeCrates {
    async fn run(&self, input: WakeInput) -> Result<WakeOutcome, ClientError> {
        let outcome = self.lock().outcome.clone();
        self.answer(CrateName::Wake, CrateCall::Wake(input), outcome)
    }

    async fn change_setting(&self, change: SettingChange) -> Result<(), ClientError> {
        self.answer(CrateName::Wake, CrateCall::ChangeSetting(change), ())
    }

    async fn section(&self) -> Result<PipelineSection, ClientError> {
        self.section(CrateName::Wake, |state| state.pipeline.clone())
    }
}

#[async_trait]
impl MeetupsPort for FakeCrates {
    async fn send_chat(
        &self,
        meetup: GroupId,
        body: ChatBody,
        parent: Option<ChatMessageId>,
    ) -> Result<ChatMessageId, ClientError> {
        let call = CrateCall::SendChat(meetup, body, parent);
        self.answer(CrateName::Meetups, call, Self::MESSAGE)
    }

    async fn set_meet_point(
        &self,
        meetup: GroupId,
        point: MeetPointDraft,
    ) -> Result<(), ClientError> {
        self.answer(
            CrateName::Meetups,
            CrateCall::SetMeetPoint(meetup, point),
            (),
        )
    }

    async fn ask_location(
        &self,
        meetup: GroupId,
        peer: LeafIndex,
    ) -> Result<LocationRequestId, ClientError> {
        let call = CrateCall::AskLocation(meetup, peer);
        self.answer(CrateName::Meetups, call, Self::REQUEST)
    }

    async fn answer_location_request(
        &self,
        meetup: GroupId,
        request: LocationRequestId,
        answer: LocationAnswer,
    ) -> Result<(), ClientError> {
        let call = CrateCall::AnswerLocationRequest(meetup, request, answer);
        self.answer(CrateName::Meetups, call, ())
    }

    async fn revoke_location(&self, meetup: GroupId, peer: LeafIndex) -> Result<(), ClientError> {
        self.answer(
            CrateName::Meetups,
            CrateCall::RevokeLocation(meetup, peer),
            (),
        )
    }

    async fn leave_meetup(&self, meetup: GroupId) -> Result<(), ClientError> {
        self.answer(CrateName::Meetups, CrateCall::LeaveMeetup(meetup), ())
    }

    async fn section(&self) -> Result<MeetupsSection, ClientError> {
        self.section(CrateName::Meetups, |state| state.meetups.clone())
    }

    fn take_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<ClientEvent>> {
        self.lock().raised_receiver.take()
    }
}

#[cfg(test)]
mod tests;
