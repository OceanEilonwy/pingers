package org.pingers.app.data

import android.content.res.Resources
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.pingers.app.R
import uniffi.pingers.AreaEditor
import uniffi.pingers.Core
import uniffi.pingers.CoreException
import uniffi.pingers.Events
import uniffi.pingers.Platform
import uniffi.pingers.SettingChange
import uniffi.pingers.Snapshot

/**
 * The repository over the Rust core (pingers-ffi). It opens [Core] with the shell's
 * [Platform] and an [Events] sink, maps each pingers-screens view model to its UiState (see
 * CoreMapping.kt), and turns each action into one Core call. Drafts the user is typing (New
 * party, the Spot editor, the meet point, an answer) stay here and go to Rust with each read,
 * so pingers-screens decides what they allow.
 *
 * Every event from the core takes a fresh [Snapshot] and republishes the flows. Calls run on
 * [scope] (the activity's, so the main thread); the core's suspend calls run on its own tokio
 * runtime.
 */
class CoreRepository(
    private val res: Resources,
    private val scope: CoroutineScope,
    dbPath: String,
    platform: Platform,
) : PingersRepository {

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
        override fun notice(notice: uniffi.pingers.Notice) {
            Log.i(TAG, "notice: ${notice::class.simpleName}")
            refreshLater()
        }
        override fun privacyStatusChanged() = refreshLater()
    }

    private val core: Core? = try {
        Core.open(dbPath, platform, sink)
    } catch (e: CoreException) {
        Log.e(TAG, "the core did not open: ${e.message}")
        null
    }

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
    private var registered: String? = null

    private var partyDraft = uniffi.pingers.PartyDraft(
        name = "",
        colour = uniffi.pingers.PartyColour.BLUE,
        server = uniffi.pingers.ServiceDraft("", uniffi.pingers.Reachability.Checking),
        relay = uniffi.pingers.ServiceDraft("", uniffi.pingers.Reachability.Checking),
        pushRelay = uniffi.pingers.PushRelayChoice.Official,
        pushRelayOperator = null,
        postQuantum = false,
    )

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
    private val _acceptInvite = MutableStateFlow(AcceptInviteState("", "", "", PartyColour.Blue, emptyList(), 0, 0, 0, ""))
    override val acceptInvite: StateFlow<AcceptInviteState> = _acceptInvite.asStateFlow()
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

    init {
        refresh()
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
        val next = try {
            core?.snapshot()
        } catch (e: CoreException) {
            Log.i(TAG, "no snapshot yet: ${e.message}")
            null
        }
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
            Log.i(TAG, "$what: ${e.message}")
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
        read("accept invite") { it.acceptInvite()?.toUi() }?.let { _acceptInvite.value = it }
        read("meetup ended") { it.meetupEnded(localClock())?.toUi() }?.let { _meetupEnded.value = it }
        publishParty()
        publishSpotEditor()
        publishMeetup()
        publishLocationRequest()
    }

    private fun publishNewParty() {
        val view = read("new party") { it.newParty(partyDraft) }
        _newParty.value = view?.toUi(createError) ?: NewPartyState(
            partyDraft.name, partyDraft.colour.toUi(), partyDraft.server.host, null, canCreate = false, error = createError,
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

    private fun publishParty() {
        val id = currentParty ?: _home.value.parties.firstOrNull()?.id ?: return
        val ffi = id.toFfi()
        read("party") { it.party(ffi).toUi() }?.let { _party.value = it }
        read("invite") { it.invite(ffi, localClock()).toUi() }?.let { _invite.value = it }
        read("server sees") { it.serverSees(ffi).toUi() }?.let { _serverView.value = it }
    }

    private fun publishSpotEditor() {
        val party = currentParty ?: return
        read("spot editor") { it.spotEditor(party.toFfi(), spotDraft).toUi() }?.let { _spotEditor.value = it }
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
        val c = core ?: return onError(res.getString(R.string.core_unavailable))
        scope.launch {
            try {
                block(c)
            } catch (e: CoreException) {
                Log.w(TAG, "$what: ${e.message}")
                onError(e.message ?: res.getString(R.string.core_unavailable))
            }
            refresh()
        }
    }

    private fun check(host: String, set: (uniffi.pingers.Reachability) -> Unit) {
        set(uniffi.pingers.Reachability.Checking)
        publishNewParty()
        call("check $host", onError = { set(uniffi.pingers.Reachability.Unreachable); publishNewParty() }) {
            val result = it.checkService(host)
            set(result)
            publishNewParty()
        }
    }

    private fun checkServer() = check(partyDraft.server.host) { partyDraft = partyDraft.copy(server = partyDraft.server.copy(check = it)) }
    private fun checkRelay() = check(partyDraft.relay.host) { partyDraft = partyDraft.copy(relay = partyDraft.relay.copy(check = it)) }

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

    override fun createParty() {
        val draft = partyDraft
        call("create party", onError = { createError = it; publishNewParty() }) { core ->
            val server = draft.server.host
            if (registered != server) {
                core.register(serverUrl(server), null)
                registered = server
            }
            val id = core.createParty(draft, res.getString(R.string.core_my_name)).toUi()
            currentParty = id
            core.createInvite(id.toFfi())
        }
    }

    override fun openAdvanced() {
        checkServer()
        checkRelay()
    }

    override fun setServer(url: String) {
        partyDraft = partyDraft.copy(server = partyDraft.server.copy(host = url))
        checkServer()
    }

    override fun setRelay(url: String) {
        partyDraft = partyDraft.copy(relay = partyDraft.relay.copy(host = url))
        checkRelay()
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

    // WAITING(pingers-client): verifying and cancelling an invite have no client call yet.
    override fun verifyInvite(invitationId: InviteId) {
        Log.i(TAG, "verify invite: not in the client API yet")
    }
    override fun cancelInvite(invitationId: InviteId) {
        Log.i(TAG, "cancel invite: not in the client API yet")
    }

    override fun acceptInvite() = call("accept invite") { core ->
        currentParty = core.acceptInvite().toUi()
    }

    override fun declineInvite() = Unit

    /** Opens an invite link: Accept invite then shows its preview. */
    fun openInvite(link: String) = call("open invite") { it.openInvite(link, res.getString(R.string.core_my_name)) }

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

    override fun setSpotRadius(radiusM: Int) {
        spotDraft = spotDraft.copy(radiusM = radiusM.toDouble())
        publishSpotEditor()
    }

    override fun saveSpot() {
        val party = currentParty ?: return
        val spot = editingSpot
        val draft = spotDraft
        call("save spot") { it.upsertSpot(party.toFfi(), spot?.toFfi(), draft) }
    }

    private fun draw(change: (AreaEditor) -> Unit) {
        try {
            change(area)
        } catch (e: CoreException) {
            Log.i(TAG, "draw: ${e.message}")
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
            Log.i(TAG, "use area: ${e.message}")
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
    override fun setNoWakePushes(on: Boolean) = change(SettingChange.WakePushes(!on))
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
        val pin = meetPointPin ?: return
        val draft = uniffi.pingers.MeetPointDraft(meetPointMode.toFfi(), meetPointLabel, pin.toFfi())
        call("share meet point") { it.setMeetPoint(id.toFfi(), draft) }
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

        /** A host as typed, as the URL the core registers with. */
        fun serverUrl(host: String) = if ("://" in host) host else "https://$host"
    }
}
