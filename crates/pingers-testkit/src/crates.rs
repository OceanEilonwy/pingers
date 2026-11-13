// SPDX-License-Identifier: AGPL-3.0-only
//! [`FakeCrates`]: recording stand-ins for the four L3 crates behind pingers-client's ports
//! (feature `client`).
//!
//! Every port call is recorded as a [`CrateCall`] with its arguments and answered from the
//! fake's state: fixed ids for commands, the scripted [`WakeOutcome`] for wakes, and the
//! sections a test sets for snapshots. A test can make one crate fail to check that the
//! client passes the error on.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use pingers_client::ports::{
    AllowListChange, JoinDecision, MeetupsPort, MeetupsSection, PartiesPort, PartiesSection,
    PartyDraft, PipelineSection, ServerSection, SettingChange, SpotDraft, SyncPort, WakeInput,
    WakeOutcome, WakePort,
};
use pingers_client::snapshot::{
    DailyWindow, HideTrail, InviteLink, InviteUrl, Name, PauseInFocus, QuietHoursPreference,
    RelayStatus, Settings, SnapshotError, Toggle, WakePushes,
};
use pingers_client::{ClientError, Crates, PushTokens};
use pingers_gate::{FocusState, GateDecision, GateError, LocationSharing, MinuteOfDay};
use pingers_meetup::{LocationAnswer, LocationVisibility, MeetPointDraft};
use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, PartyId, SpotId,
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
    AcceptInvite,
    DecideJoin(PartyId, MailboxId, JoinDecision),
    RemoveMember(PartyId, LeafIndex),
    LeaveParty(PartyId),
    SaveSpot(PartyId, SpotDraft),
    DeleteSpot(PartyId, SpotId),
    SetAlwaysAllow(PartyId, LeafIndex, AllowListChange),
    PushTokensChanged(Option<PushTokens>),
    SetWakePushes(WakePushes, Option<PushTokens>),
    Wake(WakeInput),
    ChangeSetting(SettingChange),
    SendChat(GroupId, ChatBody, Option<ChatMessageId>),
    SetMeetPoint(GroupId, MeetPointDraft),
    AskLocation(GroupId, LeafIndex),
    AnswerLocationRequest(GroupId, LocationRequestId, LocationAnswer),
    RevokeLocation(GroupId, LeafIndex),
    LeaveMeetup(GroupId),
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
}

impl FakeCrates {
    /// The id every `create_party` and `accept_invite` returns.
    pub const PARTY: PartyId = PartyId::from_bytes([0xa1; 16]);
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
            },
            meetups: MeetupsSection::default(),
            section_reads: 0,
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

    /// pingers-meetup-session's section from now on.
    pub fn set_meetups(&self, section: MeetupsSection) {
        self.lock().meetups = section;
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

    async fn accept_invite(&self) -> Result<PartyId, ClientError> {
        self.answer(CrateName::Parties, CrateCall::AcceptInvite, Self::PARTY)
    }

    async fn decide_join(
        &self,
        party: PartyId,
        invite: MailboxId,
        decision: JoinDecision,
    ) -> Result<(), ClientError> {
        let call = CrateCall::DecideJoin(party, invite, decision);
        self.answer(CrateName::Parties, call, ())
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
}
