package org.pingers.app.data

import android.content.Context
import android.content.res.Resources
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.pingers.app.BuildConfig
import org.pingers.app.R
import org.pingers.app.platform.forLog
import org.pingers.app.platform.PhoneLog
import uniffi.pingers.AreaEditor
import uniffi.pingers.CodeAnswer
import uniffi.pingers.Core
import uniffi.pingers.CoreException
import uniffi.pingers.DeviceSettingsView
import uniffi.pingers.Events
import uniffi.pingers.HomeWakeupsView
import uniffi.pingers.LeavePartyView
import uniffi.pingers.NoticeView
import uniffi.pingers.PartyCodeView
import uniffi.pingers.PartySettingsView
import uniffi.pingers.PrivacyText
import uniffi.pingers.QuietHoursDraft
import uniffi.pingers.QuietHoursEditorView
import uniffi.pingers.SafetyCodesView
import uniffi.pingers.ServerChoice
import uniffi.pingers.SettingChange
import uniffi.pingers.SetupLinkState
import uniffi.pingers.SetupStep
import uniffi.pingers.SetupView
import uniffi.pingers.Snapshot
import uniffi.pingers.TimeOfDay
import uniffi.pingers.WakeupsView
import uniffi.pingers.YourNameView
import uniffi.pingers.ZoneTimes
import uniffi.pingers.ZoneTimesEditorView

/**
 * The repository over the Rust core (pingers-ffi). It takes the process's [Core] from
 * [CoreHost] and listens to its [Events], maps each pingers-screens view model to its UiState (see
 * CoreMapping.kt), and turns each action into one Core call. Drafts the user is typing (New
 * party, the Spot editor, the meet point, an answer) stay here and go to Rust with each read,
 * so pingers-screens decides what they allow.
 *
 * Every event from the core takes a fresh [Snapshot] and republishes the flows. Calls run on
 * [scope] (the activity's, so the main thread); the core's suspend calls run on its own tokio
 * runtime.
 *
 * The section-4 screens also need the phone's own facts (PhoneFacts): they are read when the
 * repository starts and again on [refreshPhone], which the activity calls on each resume.
 */
class CoreRepository(
    private val context: Context,
    private val scope: CoroutineScope,
) : PingersRepository {
    private val res: Resources = context.resources

    /** What the core raised that the UI should act on (a route, a notification). */
    sealed interface Event {
        data class MeetupStarted(val alert: CoPresenceAlert?) : Event
        data object LocationRequested : Event
        data object MeetupEnded : Event
    }

    private val _events = MutableSharedFlow<Event>(extraBufferCapacity = 8)
    val events: SharedFlow<Event> = _events.asSharedFlow()

    private val sink = object : Events {
        override fun partiesChanged() = refreshLater()
        override fun joinRequested(party: uniffi.pingers.PartyId) = refreshLater()
        // The refresh publishes [letIn]; MainActivity routes to You're in when it appears, and
        // CoreHost posts the notification (it also runs with no activity).
        override fun letIn(party: uniffi.pingers.PartyId) = refreshLater()
        override fun meetupStarted(meetup: uniffi.pingers.MeetupId) = refreshLater {
            currentMeetup = meetup.toUi()
            _events.tryEmit(Event.MeetupStarted(_home.value.alert))
        }
        override fun meetupUpdated(meetup: uniffi.pingers.MeetupId) = refreshLater()
        override fun meetupMessage(meetup: uniffi.pingers.MeetupId) = refreshLater()
        override fun peerLocation(meetup: uniffi.pingers.MeetupId, peer: uniffi.pingers.MemberId) = refreshLater()
        override fun locationRequest(meetup: uniffi.pingers.MeetupId, from: uniffi.pingers.MemberId, request: uniffi.pingers.LocationRequestId) =
            refreshLater {
                currentRequest = meetup.toUi() to request.toUi()
                answer = null
                publishLocationRequest()
                _events.tryEmit(Event.LocationRequested)
            }
        override fun locationAccessChanged(meetup: uniffi.pingers.MeetupId, peer: uniffi.pingers.MemberId) = refreshLater()
        override fun meetupEnded(meetup: uniffi.pingers.MeetupId) = refreshLater { _events.tryEmit(Event.MeetupEnded) }
        // CoreHost has kept it (PhoneFacts.record); Home's notice and Wake-ups read it from there.
        override fun notice(notice: uniffi.pingers.Notice) = refreshLater()
        override fun privacyStatusChanged() = refreshLater()

        // CoreHost posts the notification for the whole process; the screens refresh on the
        // event that came with it.
        override fun notify(notification: uniffi.pingers.CoreNotification) = Unit
    }

    /** Null until [Core.open] finishes, or if it failed (then [openError] says why). */
    private var core: Core? = null
    private var openError: String? = null

    private var snapshot: Snapshot? = null

    // What the user is looking at and typing.
    private var currentParty: PartyId? = null
    private var currentMeetup: MeetupId? = null
    private var currentRequest: Pair<MeetupId, RequestId>? = null
    private var answer: LocationRequestState? = null
    private var dismissedAlert: MeetupId? = null
    private var replyingTo: MessageId? = null
    private var compassSelection: CompassSelection = CompassSelection.MeetPoint
    private var createError: String? = null

    // Join flow A: the link opened (kept until "Ask to join"), why asking failed, and the
    // request "Ben asked to join" is showing.
    private var inviteLink: String? = null
    private var askError: String? = null
    private var currentJoinRequest: InviteId? = null

    // Section 4: the phone's facts, the drafts on Quiet hours and Zone times, the safety code
    // shown, and "This server needs a code".
    private var phoneDevice = PhoneFacts.device(context)
    private var phoneWake = PhoneFacts.wake(context)
    private var quietDraft: QuietHoursDraft? = null
    private var zoneEditing: ZoneId? = null
    private var zoneDraft: ZoneTimes? = null
    private var safetyMember: uniffi.pingers.MemberId? = null
    private var safetyCode: String? = null
    private var codeAsked = false
    private val _registrationCode = MutableStateFlow("")
    override val registrationCode: StateFlow<String> = _registrationCode.asStateFlow()
    private var codeAnswer = CodeAnswer.NOT_TRIED

    private var partyDraft = uniffi.pingers.PartyDraft(
        name = "",
        colour = uniffi.pingers.PartyColour.BLUE,
        serverChoice = ServerChoice.BUILT_IN,
        builtIn = host(BuildConfig.SERVER_HOST),
        // The instance from the build (CoreHost.instance); checked when New party opens.
        server = uniffi.pingers.ServiceDraft(host(BuildConfig.SERVER_HOST), uniffi.pingers.Reachability.Checking),
        serverKeys = uniffi.pingers.ServerKeysDraft.NotLookedUp,
        relay = uniffi.pingers.ServiceDraft(host(BuildConfig.OHTTP_RELAY), uniffi.pingers.Reachability.Checking),
        pushRelay = uniffi.pingers.PushRelayChoice.Official,
        pushRelayOperator = null,
        postQuantum = false,
    )

    /** Another server as New party last looked it up, for "Trust this server's keys". */
    private var lookedUp: uniffi.pingers.ServerLook? = null
    private var lookUp: Job? = null

    private var editingSpot: SpotId? = null
    private var spotDraft = uniffi.pingers.SpotDraft("", uniffi.pingers.ShapeKind.CIRCLE, 150.0, null)
    private var area = AreaEditor()
    private var drawFrame = frameAt(null, MapPoint(195f, 300f))

    private var meetPointMode = MeetPointMode.Pin
    private var meetPointLabel = ""
    private var meetPointPin: Coordinate? = null
    private var meetPointFrame = frameAt(null)

    // ------------------------------------------------------------ Flows, empty until the core says otherwise

    private val noOne = Person(MemberId(-1), "", "", Tint.Stone)
    private val _home = MutableStateFlow(HomeState(SharingSummary(Sharing.Off, 0, false, 0), null, emptyList(), null))
    override val home: StateFlow<HomeState> = _home.asStateFlow()
    private val _newParty = MutableStateFlow(NewPartyState("", PartyColour.Blue, "", null, canCreate = false))
    override val newParty: StateFlow<NewPartyState> = _newParty.asStateFlow()
    private val _advanced = MutableStateFlow(
        AdvancedState(null, "", Reachability.Checking, "", Reachability.Checking, PushRelayChoice.Official, false),
    )
    override val advanced: StateFlow<AdvancedState> = _advanced.asStateFlow()
    private val _invite = MutableStateFlow(InviteState("", null, null, emptyList()))
    override val invite: StateFlow<InviteState> = _invite.asStateFlow()
    private val _openInvite = MutableStateFlow(OpenInviteState("", "", "", emptyList(), null, "", "", "", ""))
    override val openInvite: StateFlow<OpenInviteState> = _openInvite.asStateFlow()
    private val _askedToJoin = MutableStateFlow<AskedToJoinState?>(null)
    override val askedToJoin: StateFlow<AskedToJoinState?> = _askedToJoin.asStateFlow()
    private val _letIn = MutableStateFlow<LetInState?>(null)
    override val letIn: StateFlow<LetInState?> = _letIn.asStateFlow()
    private val _joinRequest = MutableStateFlow<JoinRequestState?>(null)
    override val joinRequest: StateFlow<JoinRequestState?> = _joinRequest.asStateFlow()
    private val _party = MutableStateFlow(
        PartyState(PartyId(""), "", emptyList(), emptyList(), cardFrame(emptyList()), "", PrivacyLevel.Full, null),
    )
    override val party: StateFlow<PartyState> = _party.asStateFlow()
    private val _spotEditor = MutableStateFlow(SpotEditorState("", "", ShapeKind.Circle, 150, 100, 1000, null, canSave = false))
    override val spotEditor: StateFlow<SpotEditorState> = _spotEditor.asStateFlow()
    private val _drawArea = MutableStateFlow(drawState())
    override val drawArea: StateFlow<DrawAreaState> = _drawArea.asStateFlow()
    private val _exclusionZones = MutableStateFlow(ExclusionZonesState(emptyList(), false, null, frameAt(null)))
    override val exclusionZones: StateFlow<ExclusionZonesState> = _exclusionZones.asStateFlow()
    private val _privacy = MutableStateFlow(PrivacyState(null, false, false, false, "", 0, Visibility.AskEachTime, false))
    override val privacy: StateFlow<PrivacyState> = _privacy.asStateFlow()
    private val _serverView = MutableStateFlow(ServerViewState("", false, PrivacyLevel.Full, emptyList(), emptyList()))
    override val serverView: StateFlow<ServerViewState> = _serverView.asStateFlow()
    private val _meetup = MutableStateFlow(MeetupState(MeetupId(""), "", "", 0, null, emptyList(), null, 0, emptyList(), frameAt(null)))
    override val meetup: StateFlow<MeetupState> = _meetup.asStateFlow()
    private val _chat = MutableStateFlow(ChatState("", emptyList(), emptyList(), null))
    override val chat: StateFlow<ChatState> = _chat.asStateFlow()
    private val _meetPoint = MutableStateFlow(
        MeetPointState(MeetPointMode.Pin, "", Coordinate(0.0, 0.0), null, emptyList(), emptyList(), false, frameAt(null)),
    )
    override val meetPoint: StateFlow<MeetPointState> = _meetPoint.asStateFlow()
    private val _compass = MutableStateFlow(CompassState(emptyList(), CompassSelection.MeetPoint, "", null, null, emptyList()))
    override val compass: StateFlow<CompassState> = _compass.asStateFlow()
    private val _locationRequest = MutableStateFlow(
        LocationRequestState(RequestId(""), MeetupId(""), noOne, "", "", ShareScope.ThisMeetup, askBack = true, alwaysAllow = false),
    )
    override val locationRequest: StateFlow<LocationRequestState> = _locationRequest.asStateFlow()
    private val _meetupEnded = MutableStateFlow(MeetupEndedState("", 0, "", "", 0, 0))
    override val meetupEnded: StateFlow<MeetupEndedState> = _meetupEnded.asStateFlow()

    private val _deviceSettings = MutableStateFlow<DeviceSettingsView?>(null)
    override val deviceSettings: StateFlow<DeviceSettingsView?> = _deviceSettings.asStateFlow()
    private val _yourName = MutableStateFlow<YourNameView?>(null)
    override val yourName: StateFlow<YourNameView?> = _yourName.asStateFlow()
    private val _quietHours = MutableStateFlow<QuietHoursEditorView?>(null)
    override val quietHours: StateFlow<QuietHoursEditorView?> = _quietHours.asStateFlow()
    private val _zoneTimes = MutableStateFlow<ZoneTimesEditorView?>(null)
    override val zoneTimes: StateFlow<ZoneTimesEditorView?> = _zoneTimes.asStateFlow()
    private val _wakeups = MutableStateFlow<WakeupsView?>(null)
    override val wakeups: StateFlow<WakeupsView?> = _wakeups.asStateFlow()
    private val _homeWakeups = MutableStateFlow<HomeWakeupsView>(HomeWakeupsView.Nothing)
    override val homeWakeups: StateFlow<HomeWakeupsView> = _homeWakeups.asStateFlow()
    private val _notice = MutableStateFlow<NoticeView?>(null)
    override val notice: StateFlow<NoticeView?> = _notice.asStateFlow()
    private val _privacyText = MutableStateFlow<PrivacyText?>(null)
    override val privacyText: StateFlow<PrivacyText?> = _privacyText.asStateFlow()
    private val _partySettings = MutableStateFlow<PartySettingsView?>(null)
    override val partySettings: StateFlow<PartySettingsView?> = _partySettings.asStateFlow()
    private val _safetyCodes = MutableStateFlow<SafetyCodesView?>(null)
    override val safetyCodes: StateFlow<SafetyCodesView?> = _safetyCodes.asStateFlow()
    private val _leaveParty = MutableStateFlow<LeavePartyView?>(null)
    override val leaveParty: StateFlow<LeavePartyView?> = _leaveParty.asStateFlow()
    private val _pendingJoin = MutableStateFlow<uniffi.pingers.PendingJoinView?>(null)
    override val pendingJoin: StateFlow<uniffi.pingers.PendingJoinView?> = _pendingJoin.asStateFlow()
    private val _partyCode = MutableStateFlow<PartyCodeView?>(null)
    override val partyCode: StateFlow<PartyCodeView?> = _partyCode.asStateFlow()

    init {
        scope.launch {
            // The process's one core, which receivers and workers share (CoreHost).
            CoreHost.listen(sink)
            core = CoreHost.core()
            openError = CoreHost.openError
            refresh()
        }
    }

    // ------------------------------------------------------------ Reading the core

    private fun refreshLater(then: () -> Unit = {}) {
        scope.launch {
            refresh()
            then()
        }
    }

    /** Takes a fresh snapshot and republishes every flow. */
    private fun refresh() {
        val next = core?.snapshot()
        if (next != null) {
            snapshot?.destroy()
            snapshot = next
        }
        publish()
    }

    /** One screen's read; a party or meetup that has gone keeps the last state. */
    private inline fun <T> read(what: String, block: (Snapshot) -> T): T? {
        val s = snapshot ?: return null
        return try {
            block(s)
        } catch (e: CoreException) {
            Log.i(TAG, "$what: ${e.forLog()}")
            null
        }
    }

    private fun publish() {
        read("home") { it.home().toUi() }?.let { h ->
            _home.value = if (h.alert?.meetupId == dismissedAlert) h.copy(alert = null) else h
        }
        publishNewParty()
        read("zones") { it.exclusionZones().toUi() }?.let { _exclusionZones.value = it }
        read("privacy") { it.privacy().toUi() }?.let { _privacy.value = it }
        publishJoin()
        read("meetup ended") { it.meetupEnded(localClock())?.toUi() }?.let { _meetupEnded.value = it }
        publishParty()
        publishSpotEditor()
        publishMeetup()
        publishLocationRequest()
        publishDevice()
        publishPartySettings()
    }

    // ------------------------------------------------------------ Section 4

    private fun nowSeconds() = (System.currentTimeMillis() / 1000).toULong()

    /** Settings, Your name, Quiet hours, Zone times, Wake-ups, Home's line and notice, Privacy's words. */
    private fun publishDevice() {
        if (snapshot == null) return
        val now = nowSeconds()
        read("settings") { it.deviceSettings(phoneDevice) }?.let { _deviceSettings.value = it }
        read("your name") { it.yourName() }?.let { _yourName.value = it }
        read("wake-ups") { it.wakeups(phoneWake, localClock()) }?.let { _wakeups.value = it }
        _homeWakeups.value = read("home wake-ups") { it.homeWakeups(phoneWake, now) } ?: HomeWakeupsView.Nothing
        _notice.value = read("notice") { it.topNotice(PhoneFacts.raised(context), now) }
        read("privacy text") { it.privacyText(phoneDevice.pushService) }?.let { _privacyText.value = it }
        _pendingJoin.value = read("pending join") { it.pendingJoin() }
        publishQuietHours()
        publishZoneTimes()
    }

    private fun publishQuietHours() {
        val draft = quietDraft
        _quietHours.value = if (draft == null) {
            read("quiet hours") { it.quietHoursStart() }
        } else {
            try {
                uniffi.pingers.quietHoursEditor(draft.on, draft.from, draft.to)
            } catch (e: CoreException) {
                Log.i(TAG, "quiet hours: ${e.forLog()}")
                _quietHours.value
            }
        }
    }

    private fun publishZoneTimes() {
        val zone = zoneEditing ?: return
        read("zone times") { it.zoneTimesEditor(zone.toFfi(), zoneDraft) }?.let { _zoneTimes.value = it }
    }

    /** The Party Party settings is about: the one opened, else the first (as publishParty). */
    private fun settingsParty(): PartyId? = currentParty ?: _home.value.parties.firstOrNull()?.id

    /** Party settings, Safety codes, the Leave party sheet and "This server needs a code". */
    private fun publishPartySettings() {
        val party = settingsParty()
        if (party != null) {
            val ffi = party.toFfi()
            read("party settings") { it.partySettings(ffi, PhoneLog.relayCheckedAt(context)?.toULong(), nowSeconds()) }
                ?.let { _partySettings.value = it }
            read("safety codes") { it.safetyCodes(ffi, safetyMember, safetyCode) }?.let { _safetyCodes.value = it }
            read("leave party") { it.leaveParty(ffi) }?.let { _leaveParty.value = it }
        }
        publishPartyCode()
    }

    private fun publishPartyCode() {
        _partyCode.value = if (codeAsked) {
            read("party code") { it.partyCode(partyDraft, _registrationCode.value, codeAnswer) } ?: _partyCode.value
        } else {
            null
        }
    }

    override fun setupStep(step: SetupStep): SetupView = uniffi.pingers.setupView(step)

    override fun refreshPhone() {
        phoneDevice = PhoneFacts.device(context)
        phoneWake = PhoneFacts.wake(context)
        publishDevice()
    }

    override fun saveDisplayName(name: String, onSaved: () -> Unit) =
        call("your name") { core ->
            core.setDisplayName(name.trim())
            refresh()
            onSaved()
        }

    override fun openQuietHours() {
        quietDraft = null
        publishQuietHours()
        _quietHours.value?.let { quietDraft = QuietHoursDraft(it.on, it.from, it.to) }
    }

    override fun setQuietHoursDraft(on: Boolean, from: TimeOfDay, to: TimeOfDay) {
        quietDraft = QuietHoursDraft(on, from, to)
        publishQuietHours()
    }

    override fun saveQuietHours(onSaved: () -> Unit) {
        val draft = quietDraft ?: return onSaved()
        call("quiet hours") { core ->
            core.setQuietHours(draft)
            quietDraft = null
            refresh()
            onSaved()
        }
    }

    override fun openZoneTimes(zone: ZoneId) {
        zoneEditing = zone
        zoneDraft = null
        _zoneTimes.value = null
        publishZoneTimes()
    }

    override fun setZoneTimesDraft(times: ZoneTimes) {
        zoneDraft = times
        publishZoneTimes()
    }

    override fun saveZoneTimes(onSaved: () -> Unit) {
        val view = _zoneTimes.value ?: return
        call("zone times") { core ->
            core.setZoneTimes(view.zone, view.times)
            refresh()
            onSaved()
        }
    }

    override fun deleteZone(onDeleted: () -> Unit) {
        val zone = zoneEditing ?: return
        call("delete zone") { core ->
            core.deleteZone(zone.toFfi())
            zoneEditing = null
            refresh()
            onDeleted()
        }
    }

    override fun openPartySettings(party: PartyId) {
        currentParty = party
        safetyMember = null
        safetyCode = null
        publishParty()
        publishPartySettings()
    }

    override fun checkRelayAgain() = checkReachability()

    override fun openSafetyCodes() {
        val party = settingsParty() ?: return
        val first = read("first to check") { it.firstToCheck(party.toFfi()) } ?: return publishPartySettings()
        showSafetyCode(first.toUi())
    }

    override fun showSafetyCode(member: MemberId) {
        val party = settingsParty() ?: return
        call("safety code") { core ->
            val code = core.safetyCode(party.toFfi(), member.toFfi())
            safetyMember = member.toFfi()
            safetyCode = code
        }
    }

    override fun markChecked() {
        val party = settingsParty() ?: return
        val member = safetyMember ?: return
        call("mark checked") { it.markChecked(party.toFfi(), member) }
    }

    override fun leaveParty(onLeft: () -> Unit) {
        // The Party the sheet named, never a guess.
        val party = _leaveParty.value?.party ?: return
        call("leave party") { core ->
            core.leaveParty(party)
            currentParty = null
            refresh()
            onLeft()
        }
    }

    private fun publishNewParty() {
        val view = read("new party") { it.newParty(partyDraft) }
        _newParty.value = view?.toUi(createError) ?: NewPartyState(
            partyDraft.name, partyDraft.colour.toUi(), partyDraft.server.host, null, canCreate = false, error = createError,
            serverChoice = partyDraft.serverChoice, text = _newParty.value.text,
        )
        read("advanced") { it.advanced(partyDraft).toUi() }?.let { _advanced.value = it }
            ?: _advanced.update {
                it.copy(
                    server = partyDraft.server.host, serverCheck = partyDraft.server.check.toUi(),
                    relay = partyDraft.relay.host, relayCheck = partyDraft.relay.check.toUi(),
                    postQuantum = partyDraft.postQuantum,
                )
            }
    }

    /** Join flow A: Open invite, Asked to join, You're in, and the request the inviter opened. */
    private fun publishJoin() {
        if (snapshot == null) return
        publishOpenInvite()
        _askedToJoin.value = read("asked to join") { it.askedToJoin()?.toUi() }
        _letIn.value = read("let in") { it.letIn()?.toUi() }
        val party = invitingParty()
        val request = currentJoinRequest
        _joinRequest.value = if (party != null && request != null) {
            read("join request") { it.joinRequest(party.toFfi(), request.toFfi(), localClock()).toUi() }
        } else {
            null
        }
    }

    private fun publishOpenInvite() {
        read("open invite") { it.openInvite().toUi(askError) }?.let { _openInvite.value = it }
    }

    private fun publishParty() {
        val id = currentParty ?: _home.value.parties.firstOrNull()?.id ?: return
        val ffi = id.toFfi()
        read("party") { it.party(ffi).toUi() }?.let { _party.value = it }
        read("invite") { it.invite(ffi, localClock()).toUi() }?.let { _invite.value = it }
        read("server sees") { it.serverSees(ffi).toUi() }?.let { _serverView.value = it }
    }

    private fun publishSpotEditor() {
        val party = currentParty ?: return
        read("spot editor") { it.spotEditor(party.toFfi(), spotDraft).toUi() }?.let { _spotEditor.value = it.copy(centre = drawFrame.origin) }
        _drawArea.value = drawState()
    }

    private fun drawState(): DrawAreaState {
        val name = spotDraft.name.ifBlank { res.getString(R.string.spot_unnamed) }
        return area.view(name).toUi(drawFrame)
    }

    private fun meetupId(): MeetupId? = currentMeetup ?: read("meetups") { it.meetups().lastOrNull()?.toUi() }

    private fun publishMeetup() {
        val id = meetupId() ?: return
        val ffi = id.toFfi()
        val view = read("meetup") { it.meetup(ffi).toUi() } ?: return
        _meetup.value = view
        read("chat") { it.chat(ffi, replyingTo?.toFfi(), localClock()).toUi() }?.let { _chat.value = it }
        val compass = read("compass") { it.compass(ffi, compassSelection.toFfi()).toUi() }
            ?: view.members.firstOrNull()?.let { m ->
                // No meet point yet: point at the first member instead.
                compassSelection = CompassSelection.Member(m.person.id)
                read("compass") { it.compass(ffi, compassSelection.toFfi()).toUi() }
            }
        compass?.let { _compass.value = it }
        publishMeetPoint(view)
    }

    private fun publishMeetPoint(meetup: MeetupState = _meetup.value) {
        val id = meetupId() ?: return
        val pin = meetPointPin ?: (meetup.meetPoint?.at ?: meetup.me ?: meetup.mapFrame.origin).also {
            meetPointPin = it
            meetPointFrame = frameAt(it)
        }
        val draft = uniffi.pingers.MeetPointDraft(meetPointMode.toFfi(), meetPointLabel, pin.toFfi())
        read("meet point") { it.meetPoint(id.toFfi(), draft).toUi(pin, meetPointFrame) }?.let { _meetPoint.value = it }
    }

    private fun publishLocationRequest() {
        val (meetup, request) = currentRequest
            ?: read("requests") { s -> s.locationRequests().firstOrNull()?.let { (meetupId() ?: return@let null) to it.toUi() } }
            ?: return
        read("location request") { it.locationRequest(request.toFfi(), answer?.answer()).toUi(meetup) }
            ?.let { _locationRequest.value = it }
    }

    // ------------------------------------------------------------ Calling the core

    /** Runs one core call; a failure is logged and handed to [onError]. */
    private fun call(what: String, onError: (String) -> Unit = {}, block: suspend (Core) -> Unit) {
        val c = core ?: return onError(openError ?: res.getString(R.string.core_unavailable))
        scope.launch {
            try {
                block(c)
            } catch (e: CoreException) {
                Log.w(TAG, "$what: ${e.forLog()}")
                onError(e.message ?: res.getString(R.string.core_unavailable))
            }
            refresh()
        }
    }


    // ------------------------------------------------------------ Parties and invites

    override fun openParty(partyId: PartyId) {
        currentParty = partyId
        publishParty()
    }

    override fun setNewPartyName(name: String) {
        partyDraft = partyDraft.copy(name = name)
        createError = null
        publishNewParty()
    }

    override fun setNewPartyColour(colour: PartyColour) {
        partyDraft = partyDraft.copy(colour = colour.toFfi())
        publishNewParty()
    }

    override fun setServerChoice(choice: ServerChoice) {
        val builtIn = choice == ServerChoice.BUILT_IN
        lookedUp = null
        partyDraft = partyDraft.copy(
            serverChoice = choice,
            // Another server is looked up once its address is typed (setServer).
            server = uniffi.pingers.ServiceDraft(
                if (builtIn) partyDraft.builtIn else "",
                if (builtIn) uniffi.pingers.Reachability.Checking else uniffi.pingers.Reachability.Unreachable,
            ),
            serverKeys = uniffi.pingers.ServerKeysDraft.NotLookedUp,
        )
        createError = null
        publishNewParty()
        if (builtIn) checkServices()
    }

    override fun setRegistrationCode(code: String) {
        _registrationCode.value = code
        publishPartyCode()
    }

    override fun createParty(onResult: (CreateOutcome) -> Unit) {
        val draft = partyDraft
        val code = _registrationCode.value.takeIf { codeAsked && it.isNotBlank() }
        val failed = { why: String ->
            createError = why
            publishNewParty()
            onResult(CreateOutcome.Failed)
        }
        call("create party", onError = failed) { core ->
            val created = uniffi.pingers.NewPartyDraft(
                name = draft.name,
                myName = myName(),
                colour = draft.colour,
                postQuantum = draft.postQuantum,
                otherServer = draft.server.host.takeIf { draft.serverChoice == ServerChoice.ANOTHER },
                registrationCode = code,
            )
            val id = try {
                core.createParty(created).toUi()
            } catch (e: CoreException.CodeNeeded) {
                // "This server needs a code": nothing was created; ask for one.
                codeAsked = true
                codeAnswer = CodeAnswer.NOT_TRIED
                publishPartyCode()
                return@call onResult(CreateOutcome.CodeNeeded)
            } catch (e: CoreException.CodeRefused) {
                codeAnswer = CodeAnswer.REFUSED
                publishPartyCode()
                return@call onResult(CreateOutcome.CodeRefused)
            }
            currentParty = id
            codeAsked = false
            _registrationCode.value = ""
            codeAnswer = CodeAnswer.NOT_TRIED
            createError = null
            publishPartyCode()
            onResult(CreateOutcome.Created)
            core.createInvite(id.toFfi())
        }
    }

    override fun openNewParty() = checkServices()

    override fun openAdvanced() = checkServices()

    /**
     * The core's reachability check (the gateway key fetched through the relay and checked
     * against its pin), then who the server's parameters say runs it and the push relay. The
     * relay declares no operator of its own, so it is counted as the server's: the cautious
     * reading, which the privacy summary then flags as one operator running both.
     */
    private fun checkServices() {
        publishNewParty()
        // Another server was checked when it was looked up (setServer).
        if (partyDraft.serverChoice == ServerChoice.ANOTHER) return
        call("reachability", onError = { markServices(null) }) { core ->
            val check = core.checkReachability()
            PhoneLog.recordRelayCheck(context)
            refresh()
            val operator = snapshot?.serverOperator()
            markServices(operator.takeIf { check.status == uniffi.pingers.RelayStatus.REACHABLE })
        }
    }

    /** A build URL as New party shows it: the host, without the scheme or a trailing slash. */
    private fun host(url: String) = url.removePrefix("https://").trimEnd('/')

    /** Server and relay reachable under [operator], or both unreachable when it is null. */
    private fun markServices(operator: String?) {
        val check = operator?.let { uniffi.pingers.Reachability.Reachable(it) }
            ?: uniffi.pingers.Reachability.Unreachable
        partyDraft = partyDraft.copy(
            server = partyDraft.server.copy(check = check),
            relay = partyDraft.relay.copy(check = check),
            pushRelayOperator = snapshot?.pushRelayOperator() ?: partyDraft.pushRelayOperator,
        )
        publishNewParty()
    }

    override fun setServer(url: String) {
        partyDraft = partyDraft.copy(
            server = partyDraft.server.copy(host = url, check = uniffi.pingers.Reachability.Checking),
            serverKeys = uniffi.pingers.ServerKeysDraft.NotLookedUp,
        )
        lookedUp = null
        createError = null
        publishNewParty()
        if (partyDraft.serverChoice != ServerChoice.ANOTHER) return
        // Looked up once typing pauses: who runs it, its relays, and the keys it serves.
        // Nothing is recorded until the user trusts the keys.
        lookUp?.cancel()
        lookUp = scope.launch {
            delay(LOOK_UP_PAUSE_MS)
            val c = core ?: return@launch
            val look = try {
                c.probeServer(url)
            } catch (e: CoreException) {
                Log.w(TAG, "look up server: ${e.forLog()}")
                if (partyDraft.server.host == url) {
                    partyDraft = partyDraft.copy(server = partyDraft.server.copy(check = uniffi.pingers.Reachability.Unreachable))
                    createError = e.message
                    publishNewParty()
                }
                return@launch
            }
            if (partyDraft.server.host != url) return@launch
            lookedUp = look
            showLook(look, trusted = look.known)
        }
    }

    /** What looking up another server found, on New party and Advanced. */
    private fun showLook(look: uniffi.pingers.ServerLook, trusted: Boolean) {
        // The relay publishes no operator of its own: counted as the server's, the cautious reading.
        val operator = look.operator?.let { uniffi.pingers.Reachability.Reachable(it) }
            ?: uniffi.pingers.Reachability.Unreachable
        partyDraft = partyDraft.copy(
            server = partyDraft.server.copy(host = host(look.server), check = operator),
            relay = uniffi.pingers.ServiceDraft(look.relay, operator),
            pushRelayOperator = look.pushRelayOperator ?: partyDraft.pushRelayOperator,
            serverKeys = if (trusted) uniffi.pingers.ServerKeysDraft.Trusted
            else uniffi.pingers.ServerKeysDraft.ToTrust(look.gatewayKey),
        )
        publishNewParty()
    }

    override fun trustServerKeys() {
        val look = lookedUp ?: return
        call("trust server", onError = { why -> createError = why; publishNewParty() }) { core ->
            core.trustServer(look.server)
            showLook(look, trusted = true)
        }
    }

    override fun setRelay(url: String) {
        partyDraft = partyDraft.copy(relay = partyDraft.relay.copy(host = url))
        publishNewParty()
    }

    override fun setPushRelay(choice: PushRelayChoice) {
        partyDraft = partyDraft.copy(
            pushRelay = when (choice) {
                PushRelayChoice.Official -> uniffi.pingers.PushRelayChoice.Official
                PushRelayChoice.Custom -> uniffi.pingers.PushRelayChoice.Custom("")
            },
        )
        publishNewParty()
    }

    override fun setPostQuantum(on: Boolean) {
        partyDraft = partyDraft.copy(postQuantum = on)
        publishNewParty()
    }

    /** The Party Invite people is showing (publishParty's choice). */
    private fun invitingParty(): PartyId? = currentParty ?: _home.value.parties.firstOrNull()?.id

    override fun cancelInvite(invitationId: InviteId) {
        val party = invitingParty() ?: return
        call("cancel invite") { it.cancelInvite(party.toFfi(), invitationId.toFfi()) }
    }

    // ------------------------------------------------------------ The first-phone setup link

    /**
     * The setup link step. It waits for the core to open, since a scanned link may start the
     * app; a core that does not open closes the step (Home then says why).
     */
    private val setupFlow = SetupLinkFlow(
        scope,
        check = { link -> setupCall("check setup link") { it.checkSetupLink(link) } },
        use = { link -> setupCall("use setup link") { it.useSetupLink(link) } },
        view = { uniffi.pingers.setupLinkView(it) },
    )

    override val setupLink: StateFlow<SetupLinkUi> = setupFlow.ui

    private suspend fun setupCall(what: String, block: suspend (Core) -> SetupLinkState): SetupLinkState? {
        val c = CoreHost.core() ?: return null
        return try {
            block(c).also { refresh() }
        } catch (e: CoreException) {
            Log.w(TAG, "$what: ${e.forLog()}")
            null
        }
    }

    override fun showSetupLink(link: String) = setupFlow.show(link)

    override fun actOnSetupLink() = setupFlow.act()

    override fun closeSetupLink() = setupFlow.close()

    // ------------------------------------------------------------ Join flow A, the joiner

    override fun showInvite(link: String) {
        inviteLink = link
        askError = null
        publishOpenInvite()
    }

    override fun askToJoin(name: String, onAsked: () -> Unit) {
        val link = inviteLink ?: return
        askError = null
        // No name set yet: what was typed on Open invite becomes Your name, then the join uses it.
        val needsName = _openInvite.value.joinAs == null
        call("ask to join", onError = { askError = it; publishOpenInvite() }) { core ->
            if (needsName) core.setDisplayName(name.trim())
            core.openInvite(link, if (needsName) name.trim() else myName())
            inviteLink = null
            // Asked to join reads its code from the snapshot, so take it before going there.
            refresh()
            onAsked()
        }
    }

    override fun withdrawJoin() = call("withdraw join") { it.withdrawJoin() }

    override fun checkJoins() = call("check joins") { it.checkJoins() }

    override fun openJoinedParty() {
        val party = _letIn.value?.party ?: return
        currentParty = party
        publishParty()
        call("dismiss join") { it.dismissJoin() }
    }

    override fun leaveJoinedParty() {
        val party = _letIn.value?.party ?: return
        if (currentParty == party) currentParty = null
        call("leave party") { it.leaveParty(party.toFfi()) }
        call("dismiss join") { it.dismissJoin() }
    }

    // ------------------------------------------------------------ Join flow A, the inviter

    override fun openJoinRequest(requestId: InviteId) {
        currentJoinRequest = requestId
        publishJoin()
    }

    override fun letIn() = decide("let in") { core, party, request -> core.letIn(party, request) }

    override fun turnAway() = decide("turn away") { core, party, request -> core.turnAway(party, request) }

    private fun decide(what: String, block: suspend (Core, uniffi.pingers.PartyId, uniffi.pingers.InviteId) -> Unit) {
        val party = invitingParty() ?: return
        val request = currentJoinRequest ?: return
        currentJoinRequest = null
        call(what) { block(it, party.toFfi(), request.toFfi()) }
    }

    /** The name set on this phone (Your name), or the default until one is. */
    private fun myName(): String =
        read("your name") { it.yourName().name } ?: res.getString(R.string.core_my_name)

    // ------------------------------------------------------------ Spots

    override fun editSpot(spotId: SpotId?) {
        editingSpot = spotId
        val row = _party.value.spots.firstOrNull { it.id == spotId }
        val fence = row?.fence
        spotDraft = uniffi.pingers.SpotDraft(
            name = row?.name.orEmpty(),
            shape = if (fence is Fence.Polygon) uniffi.pingers.ShapeKind.AREA else uniffi.pingers.ShapeKind.CIRCLE,
            radiusM = (fence as? Fence.Circle)?.radiusM ?: 150.0,
            area = (fence as? Fence.Polygon)?.vertices?.map { it.toFfi() },
        )
        area.destroy()
        area = (fence as? Fence.Polygon)?.let {
            try {
                AreaEditor.editing(it.vertices.map { v -> v.toFfi() })
            } catch (_: CoreException) {
                null
            }
        } ?: AreaEditor()
        val centre = when (fence) {
            is Fence.Circle -> fence.center
            is Fence.Polygon -> Coordinate(fence.vertices.map { it.latitude }.average(), fence.vertices.map { it.longitude }.average())
            null -> _party.value.spots.firstOrNull()?.fence?.let { (it as? Fence.Circle)?.center } ?: _meetup.value.me
        }
        drawFrame = frameAt(centre, MapPoint(195f, 300f))
        publishSpotEditor()
    }

    override fun setSpotName(name: String) {
        spotDraft = spotDraft.copy(name = name)
        publishSpotEditor()
    }

    override fun setSpotShape(shape: ShapeKind) {
        spotDraft = spotDraft.copy(shape = shape.toFfi())
        publishSpotEditor()
    }

    override fun moveSpot(to: Coordinate) {
        // A circle is saved at the frame's origin; a drawn area keeps its own coordinates.
        drawFrame = frameAt(to, MapPoint(195f, 300f))
        publishSpotEditor()
    }

    override fun setSpotRadius(radiusM: Int) {
        spotDraft = spotDraft.copy(radiusM = radiusM.toDouble())
        publishSpotEditor()
    }

    override fun saveSpot() {
        val party = currentParty ?: return
        val spot = editingSpot
        val draft = spotDraft
        // A circle stays where the editor's map is centred; an area is what was drawn.
        val fence = draft.area?.takeIf { draft.shape == uniffi.pingers.ShapeKind.AREA }
            ?.let { uniffi.pingers.Fence.Polygon(it) }
            ?: uniffi.pingers.Fence.Circle(drawFrame.origin.toFfi(), draft.radiusM)
        val save = uniffi.pingers.SpotToSave(spot?.toFfi(), draft.name, fence)
        call("save spot") { it.saveSpot(party.toFfi(), save) }
    }

    private fun draw(change: (AreaEditor) -> Unit) {
        try {
            change(area)
        } catch (e: CoreException) {
            Log.i(TAG, "draw: ${e.forLog()}")
        }
        _drawArea.value = drawState()
    }

    override fun drawTapMap(at: Coordinate) = draw { it.tapMap(at.toFfi()) }
    override fun drawTapPoint(index: Int) = draw { it.tapPoint(index.toUInt()) }
    override fun drawDragPoint(index: Int, to: Coordinate) = draw { it.drag(index.toUInt(), to.toFfi()) }
    override fun drawDragEnd() = draw { it.dragEnd() }
    override fun drawInsertMidpoint(afterIndex: Int) = draw { it.insertMidpoint(afterIndex.toUInt()) }
    override fun drawDeleteSelected() = draw { it.deleteSelected() }
    override fun drawUndo() = draw { it.undo() }
    override fun drawClear() = draw { it.clear() }

    override fun useDrawnArea() {
        val vertices = try {
            area.polygon()
        } catch (e: CoreException) {
            Log.i(TAG, "use area: ${e.forLog()}")
            return
        }
        spotDraft = spotDraft.copy(shape = uniffi.pingers.ShapeKind.AREA, area = vertices)
        publishSpotEditor()
    }

    // ------------------------------------------------------------ Privacy (local only)

    private fun change(change: SettingChange) = call("setting") { it.changeSetting(change) }

    override fun setHideTrail(on: Boolean) = change(SettingChange.HideTrail(on))
    override fun setSharing(on: Boolean) = change(SettingChange.LocationSharing(on))
    override fun setPauseInFocus(on: Boolean) = change(SettingChange.PauseInFocus(on))
    override fun setQuietHours(on: Boolean) = change(SettingChange.QuietHours(on))
    override fun setNoWakePushes(on: Boolean) = call("wake pushes") { it.setWakePushes(!on) }

    // The answer lands in the snapshot's relay status, which What your server sees shows as a
    // note; until pingers-sync merges the core answers Unavailable and the call only logs it.
    // Party settings dates it from when it returned.
    override fun checkReachability() = call("reachability") {
        it.checkReachability()
        PhoneLog.recordRelayCheck(context)
    }

    override fun setVisibility(visibility: Visibility) = change(
        SettingChange.Visibility(
            when (visibility) {
                Visibility.AskEachTime -> uniffi.pingers.Visibility.ASK_EACH_TIME
                Visibility.Everyone -> uniffi.pingers.Visibility.EVERYONE
                Visibility.Nobody -> uniffi.pingers.Visibility.NOBODY
            },
        ),
    )

    // ------------------------------------------------------------ Meetup

    override fun hideMeHere() {
        val id = _home.value.alert?.meetupId ?: meetupId() ?: return
        dismissAlert()
        call("leave meetup") { it.leaveMeetup(id.toFfi()) }
    }

    override fun dismissAlert() {
        dismissedAlert = _home.value.alert?.meetupId
        _home.update { it.copy(alert = null) }
    }

    override fun askLocation(memberId: MemberId) {
        val id = meetupId() ?: return
        call("ask location") { it.askLocation(id.toFfi(), memberId.toFfi()) }
    }

    override fun replyInThread(messageId: MessageId?) {
        replyingTo = messageId
        publishMeetup()
    }

    override fun sendChat(body: String) {
        val id = meetupId() ?: return
        val parent = replyingTo
        call("send chat") { it.sendChat(id.toFfi(), body.trim(), parent?.toFfi()) }
    }

    override fun setMeetPointMode(mode: MeetPointMode) {
        meetPointMode = mode
        publishMeetPoint()
    }

    override fun setMeetPointLabel(label: String) {
        meetPointLabel = label
        publishMeetPoint()
    }

    override fun moveMeetPoint(to: Coordinate) {
        meetPointPin = to
        publishMeetPoint()
    }

    override fun shareMeetPoint() {
        val id = meetupId() ?: return
        val label = meetPointLabel.ifBlank { null }
        val choice = when (meetPointMode) {
            MeetPointMode.AtMe -> uniffi.pingers.MeetPointChoice.AtMe
            // A pin, or the midpoint pingers-screens computed for the screen.
            MeetPointMode.Pin, MeetPointMode.Midpoint ->
                uniffi.pingers.MeetPointChoice.Fixed((_meetPoint.value.point ?: return).toFfi(), label)
        }
        call("share meet point") { it.setMeetPoint(id.toFfi(), choice) }
    }

    override fun selectCompassTarget(target: CompassSelection) {
        val chip = _compass.value.targets.firstOrNull { it.selection == target }
        if (chip is CompassTarget.Member && chip.locked) return askLocation(chip.id)
        compassSelection = target
        publishMeetup()
    }

    override fun setShareScope(scope: ShareScope) = updateAnswer { it.copy(scope = scope) }
    override fun setAskBack(on: Boolean) = updateAnswer { it.copy(askBack = on) }
    override fun setAlwaysAllow(on: Boolean) = updateAnswer { it.copy(alwaysAllow = on) }

    private fun updateAnswer(change: (LocationRequestState) -> LocationRequestState) {
        answer = change(answer ?: _locationRequest.value)
        publishLocationRequest()
    }

    override fun answerLocationRequest(allow: Boolean) {
        val state = _locationRequest.value
        val answer = if (allow) (this.answer ?: state).answer() else null
        call("answer request") { it.answerLocationRequest(state.meetupId.toFfi(), state.id.toFfi(), answer) }
        currentRequest = null
        this.answer = null
    }

    // The Home debug menu's events come from the core now; these have nothing to fake.
    override fun simulateCoPresence() = Unit
    override fun simulateLocationRequest() = Unit
    override fun simulateMeetupEnded() = Unit

    private companion object {
        const val TAG = "CoreRepository"
        /** How long typing pauses before another server is looked up. */
        const val LOOK_UP_PAUSE_MS = 800L
    }
}
