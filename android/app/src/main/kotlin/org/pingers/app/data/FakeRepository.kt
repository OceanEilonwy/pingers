package org.pingers.app.data

import android.content.res.Resources
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.pingers.app.R

/**
 * The canvas's example data (Climbing crew meeting at Riverside Park), with just
 * enough behaviour for every control to respond. Nothing leaves the phone and
 * nothing is stored. To be replaced by a repository over the Rust core.
 */
class FakeRepository(private val res: Resources, private val scope: CoroutineScope) : PingersRepository {

    private val me = Person("me", "You", "You", Tint.Sage, isMe = true)
    private val ana = Person("ana", "Ana", "AN", Tint.Peach)
    private val ben = Person("ben", "Ben", "BE", Tint.Butter)
    private val chloe = Person("chloe", "Chloé", "CH", Tint.Sky)
    private val dev = Person("dev", "Dev", "DE", Tint.Lilac)
    private val eli = Person("eli", "Eli", "EL", Tint.Mint)

    private val server = "pingers.flat7.org"

    private val parties = listOf(
        PartyRow("cc", "Climbing crew", "CC", PartyColour.Blue, members = 6, spots = 3, live = true),
        PartyRow("uf", "Uni flat", "UF", PartyColour.Orange, members = 4, spots = 2, live = false),
        PartyRow("5s", "Saturday 5-a-side", "5S", PartyColour.Ink, members = 11, spots = 1, live = false),
    )

    private val partyDetails = mapOf(
        "cc" to PartyState(
            id = "cc",
            name = "Climbing crew",
            members = listOf(me, ana, ben, chloe, dev, eli),
            spots = listOf(
                SpotRow("riverside", "Riverside Park", SpotShape.Circle(300), inMeetup = true),
                SpotRow("wall", "The Wall climbing gym", SpotShape.Area(0.4f), inMeetup = false),
                SpotRow("anas", "Ana's place", SpotShape.Circle(60), inMeetup = false),
            ),
            mapSpots = listOf(
                MapCircle(MapPoint(245f, 150f), 46f),
                MapCircle(MapPoint(70f, 55f), 30f),
                MapCircle(MapPoint(210f, 40f), 14f, small = true),
            ),
            server = server,
            privacy = PrivacyLevel.Full,
        ),
        "uf" to PartyState(
            id = "uf",
            name = "Uni flat",
            members = listOf(me, dev, eli, ana),
            spots = listOf(
                SpotRow("flat", "Flat 7", SpotShape.Circle(80), inMeetup = false),
                SpotRow("library", "Main library", SpotShape.Circle(120), inMeetup = false),
            ),
            mapSpots = listOf(MapCircle(MapPoint(120f, 130f), 26f), MapCircle(MapPoint(260f, 60f), 34f)),
            server = server,
            privacy = PrivacyLevel.Full,
        ),
        "5s" to PartyState(
            id = "5s",
            name = "Saturday 5-a-side",
            members = listOf(me, ben, dev, eli, ana, chloe),
            spots = listOf(SpotRow("marshes", "Hackney Marshes", SpotShape.Circle(400), inMeetup = false)),
            mapSpots = listOf(MapCircle(MapPoint(200f, 150f), 52f)),
            server = server,
            privacy = PrivacyLevel.Full,
        ),
    )

    private val _home = MutableStateFlow(
        HomeState(
            sharing = SharingSummary(on = true, partyCount = 3, pausesInFocus = true, exclusionZones = 2),
            liveMeetup = LiveMeetupCard("m1", "Climbing crew", "Riverside Park", listOf(ben, chloe, me)),
            parties = parties,
            alert = null,
        ),
    )
    override val home: StateFlow<HomeState> = _home.asStateFlow()

    private val _newParty = MutableStateFlow(NewPartyState("Climbing crew", PartyColour.Blue, server, PrivacyLevel.Full))
    override val newParty: StateFlow<NewPartyState> = _newParty.asStateFlow()

    private val _advanced = MutableStateFlow(
        AdvancedState(
            privacy = PrivacyLevel.Full,
            server = server,
            serverCheck = Reachability.Checking,
            relay = "relay.quietpath.net",
            relayCheck = Reachability.Checking,
            pushRelay = PushRelayChoice.Official,
            postQuantum = false,
        ),
    )
    override val advanced: StateFlow<AdvancedState> = _advanced.asStateFlow()

    private val _invite = MutableStateFlow(
        InviteState(
            partyName = "Climbing crew",
            link = "https://pingers.flat7.org/join#k7Qx-Climbing-crew",
            expiresInHours = 24,
            invitations = listOf(
                Invitation.Opened("i1", ben, "4821"),
                Invitation.Pending("i2", sharedHoursAgo = 2, expiresInHours = 22),
                Invitation.Joined("i3", chloe),
            ),
        ),
    )
    override val invite: StateFlow<InviteState> = _invite.asStateFlow()

    private val _acceptInvite = MutableStateFlow(
        AcceptInviteState(
            inviter = "Ana",
            partyName = "Climbing crew",
            partyInitials = "CC",
            colour = PartyColour.Blue,
            preview = listOf(ana, chloe, dev),
            moreMembers = 2,
            members = 5,
            spots = 3,
            safetyCode = "4821 0937 5512",
        ),
    )
    override val acceptInvite: StateFlow<AcceptInviteState> = _acceptInvite.asStateFlow()

    private val _party = MutableStateFlow(partyDetails.getValue("cc"))
    override val party: StateFlow<PartyState> = _party.asStateFlow()

    private val _spotEditor = MutableStateFlow(
        SpotEditorState("Climbing crew", "Riverside Park", ShapeKind.Circle, 300, 40, 1000, areaHectares = null),
    )
    override val spotEditor: StateFlow<SpotEditorState> = _spotEditor.asStateFlow()

    private var editor = PolygonEditor.Example
    private val _drawArea = MutableStateFlow(editor.state("Riverside Park"))
    override val drawArea: StateFlow<DrawAreaState> = _drawArea.asStateFlow()

    private val _exclusionZones = MutableStateFlow(
        ExclusionZonesState(
            zones = listOf(
                ZoneRow("home", "Home", ZoneShape.Circle(150), schedule = null, labelAt = MapPoint(50f, 52f)),
                ZoneRow("work", "Work", ZoneShape.Area, schedule = "weekdays 08:00–18:00", labelAt = MapPoint(245f, 50f)),
            ),
            hideTrail = true,
            me = MapPoint(160f, 160f),
        ),
    )
    override val exclusionZones: StateFlow<ExclusionZonesState> = _exclusionZones.asStateFlow()

    private val _privacy = MutableStateFlow(
        PrivacyState(
            pausedBecause = "Sleep Focus is on",
            sharing = true,
            pauseInFocus = true,
            quietHours = false,
            quietHoursRange = "23:00–07:00",
            exclusionZones = 2,
            visibility = Visibility.AskEachTime,
            noWakePushes = false,
        ),
    )
    override val privacy: StateFlow<PrivacyState> = _privacy.asStateFlow()

    // pingers-words generates these lists from the live configuration; here they are fixed.
    private val _serverView = MutableStateFlow(
        ServerViewState(
            server = server,
            selfHosted = true,
            privacy = PrivacyLevel.Full,
            canSee = res.getStringArray(R.array.fake_server_can_see).toList(),
            neverSees = res.getStringArray(R.array.fake_server_never_sees).toList(),
        ),
    )
    override val serverView: StateFlow<ServerViewState> = _serverView.asStateFlow()

    private val _meetup = MutableStateFlow(
        MeetupState(
            id = "m1",
            partyName = "Climbing crew",
            spotName = "Riverside Park",
            hereCount = 3,
            meetPoint = MeetPoint("Meet at the bandstand", "Ben", 85, MapPoint(218f, 196f)),
            members = listOf(
                MeetupMember(ben, PeerLocation.Shared(120, Direction.NE, MapPoint(268f, 248f))),
                MeetupMember(chloe, PeerLocation.Private),
            ),
            me = MapPoint(149f, 291f),
            chatCount = 4,
            sharedWith = listOf("Ben"),
        ),
    )
    override val meetup: StateFlow<MeetupState> = _meetup.asStateFlow()

    private val firstMessage = ChatMessage(
        "c1", ben, "Who's up for coffee after? I'm by the bandstand",
        replies = listOf(ChatMessage("c2", chloe, "Yes! 5 min"), ChatMessage("c3", me, "On my way")),
    )
    private var nextMessage = 10
    private val _chat = MutableStateFlow(
        ChatState(
            spotName = "Riverside Park",
            participants = listOf("Ben", "Chloé"),
            items = listOf(
                ChatItem.Joined("Chloé", "14:40"),
                ChatItem.Message(firstMessage),
                ChatItem.MeetPointSet(ben, "bandstand"),
                ChatItem.Message(ChatMessage("c4", me, "I can see you on the compass, 2 min")),
            ),
            replyingTo = firstMessage,
        ),
    )
    override val chat: StateFlow<ChatState> = _chat.asStateFlow()

    private val _meetPoint = MutableStateFlow(
        MeetPointState(
            mode = MeetPointMode.Pin,
            label = "Bandstand",
            pin = MapPoint(195f, 256f),
            distances = listOf(MeetPointDistance("You", 60), MeetPointDistance("Ben", 70), MeetPointDistance("Chloé", null)),
            members = listOf(ben to MapPoint(285f, 267f), chloe to MapPoint(103f, 237f)),
        ),
    )
    override val meetPoint: StateFlow<MeetPointState> = _meetPoint.asStateFlow()

    private val compassTargets = listOf(
        CompassTarget.Place("mp", "Bandstand"),
        CompassTarget.Member("ben", "Ben", locked = false),
        CompassTarget.Member("chloe", "Chloé", locked = true),
    )
    private val _compass = MutableStateFlow(compassFor("mp"))
    override val compass: StateFlow<CompassState> = _compass.asStateFlow()

    private val _locationRequest = MutableStateFlow(
        LocationRequestState(chloe, "Riverside Park", "Climbing crew", ShareScope.ThisMeetup, askBack = true, alwaysAllow = false),
    )
    override val locationRequest: StateFlow<LocationRequestState> = _locationRequest.asStateFlow()

    private val _meetupEnded = MutableStateFlow(MeetupEndedState("Riverside Park", 94, "14:31", "16:05", 4, 23))
    override val meetupEnded: StateFlow<MeetupEndedState> = _meetupEnded.asStateFlow()

    // ------------------------------------------------------------ Parties and invites

    override fun openParty(partyId: String) {
        partyDetails[partyId]?.let { p ->
            _party.value = p
            _invite.update { it.copy(partyName = p.name) }
            _spotEditor.update { it.copy(partyName = p.name) }
        }
    }

    override fun setNewPartyName(name: String) = _newParty.update { it.copy(name = name) }

    override fun setNewPartyColour(colour: PartyColour) = _newParty.update { it.copy(colour = colour) }

    override fun createParty() {
        val name = _newParty.value.name.ifBlank { "New party" }
        _party.update { it.copy(name = name) }
        _invite.update { it.copy(partyName = name) }
    }

    private val checks = mutableMapOf<String, Job>()

    private fun check(key: String, ms: Long, set: (AdvancedState, Reachability) -> AdvancedState) {
        checks[key]?.cancel()
        _advanced.update { set(it, Reachability.Checking) }
        checks[key] = scope.launch {
            delay(ms)
            _advanced.update { set(it, Reachability.Reachable) }
        }
    }

    private fun checkServer(ms: Long) = check("server", ms) { s, r -> s.copy(serverCheck = r) }
    private fun checkRelay(ms: Long) = check("relay", ms) { s, r -> s.copy(relayCheck = r) }

    override fun openAdvanced() {
        checkServer(1200)
        checkRelay(1800)
    }

    override fun setServer(url: String) {
        _advanced.update { it.copy(server = url) }
        _newParty.update { it.copy(server = url) }
        checkServer(1200)
    }

    override fun setRelay(url: String) {
        _advanced.update { it.copy(relay = url) }
        checkRelay(1200)
    }

    override fun setPushRelay(choice: PushRelayChoice) = _advanced.update { it.copy(pushRelay = choice) }

    override fun setPostQuantum(on: Boolean) = _advanced.update { it.copy(postQuantum = on) }

    override fun verifyInvite(invitationId: String) = _invite.update { s ->
        s.copy(
            invitations = s.invitations.map {
                if (it is Invitation.Opened && it.id == invitationId) Invitation.Joined(it.id, it.person) else it
            },
        )
    }

    override fun cancelInvite(invitationId: String) = _invite.update { s ->
        s.copy(invitations = s.invitations.filterNot { it.id == invitationId })
    }

    override fun acceptInvite() = openParty("cc")

    override fun declineInvite() = Unit

    // ------------------------------------------------------------ Spots

    override fun editSpot(spotId: String?) {
        val spot = _party.value.spots.firstOrNull { it.id == spotId }
        _spotEditor.update {
            when (val shape = spot?.shape) {
                null -> it.copy(name = "", shape = ShapeKind.Circle, radiusM = 150, areaHectares = null)
                is SpotShape.Circle -> it.copy(name = spot.name, shape = ShapeKind.Circle, radiusM = shape.radiusM, areaHectares = null)
                is SpotShape.Area -> it.copy(name = spot.name, shape = ShapeKind.Area, areaHectares = shape.hectares)
            }
        }
        editor = PolygonEditor.Example
        publishDraw()
    }

    override fun setSpotName(name: String) {
        _spotEditor.update { it.copy(name = name) }
        publishDraw()
    }

    override fun setSpotShape(shape: ShapeKind) = _spotEditor.update { it.copy(shape = shape) }

    override fun setSpotRadius(radiusM: Int) = _spotEditor.update { it.copy(radiusM = radiusM) }

    override fun saveSpot() = Unit

    private fun publishDraw() {
        _drawArea.value = editor.state(_spotEditor.value.name.ifBlank { res.getString(R.string.spot_unnamed) })
    }

    private fun draw(change: (PolygonEditor) -> PolygonEditor) {
        editor = change(editor)
        publishDraw()
    }

    override fun drawTapMap(at: MapPoint) = draw { it.tapMap(at) }
    override fun drawTapPoint(index: Int) = draw { it.tapPoint(index) }
    override fun drawDragPoint(index: Int, to: MapPoint) = draw { it.drag(index, to) }
    override fun drawDragEnd() = draw { it.dragEnd() }
    override fun drawInsertMidpoint(afterIndex: Int) = draw { it.insertMidpoint(afterIndex) }
    override fun drawDeleteSelected() = draw { it.deleteSelected() }
    override fun drawUndo() = draw { it.undo() }
    override fun drawClear() = draw { it.clear() }

    override fun useDrawnArea() {
        val m2 = editor.areaM2() ?: return
        _spotEditor.update { it.copy(shape = ShapeKind.Area, areaHectares = m2 / 10_000f) }
    }

    // ------------------------------------------------------------ Privacy

    override fun setHideTrail(on: Boolean) = _exclusionZones.update { it.copy(hideTrail = on) }

    override fun setSharing(on: Boolean) {
        _privacy.update { it.copy(sharing = on) }
        _home.update { it.copy(sharing = it.sharing.copy(on = on)) }
    }

    override fun setPauseInFocus(on: Boolean) {
        _privacy.update { it.copy(pauseInFocus = on, pausedBecause = if (on) "Sleep Focus is on" else null) }
        _home.update { it.copy(sharing = it.sharing.copy(pausesInFocus = on)) }
    }

    override fun setQuietHours(on: Boolean) = _privacy.update { it.copy(quietHours = on) }

    override fun setVisibility(visibility: Visibility) = _privacy.update { it.copy(visibility = visibility) }

    override fun setNoWakePushes(on: Boolean) = _privacy.update { it.copy(noWakePushes = on) }

    // ------------------------------------------------------------ Meetup

    override fun hideMeHere() = dismissAlert()

    override fun dismissAlert() = _home.update { it.copy(alert = null) }

    override fun askLocation(memberId: String) = _meetup.update { s ->
        s.copy(
            members = s.members.map {
                if (it.person.id == memberId && it.location is PeerLocation.Private) it.copy(location = PeerLocation.Asked) else it
            },
        )
    }

    override fun replyInThread(messageId: String?) = _chat.update { s ->
        val target = s.items.filterIsInstance<ChatItem.Message>().map { it.message }.firstOrNull { it.id == messageId }
        s.copy(replyingTo = target)
    }

    override fun sendChat(body: String) {
        val text = body.trim()
        if (text.isEmpty()) return
        val msg = ChatMessage("n${nextMessage++}", me, text)
        _chat.update { s ->
            val parent = s.replyingTo
            if (parent == null) {
                s.copy(items = s.items + ChatItem.Message(msg))
            } else {
                val items = s.items.map {
                    if (it is ChatItem.Message && it.message.id == parent.id) {
                        ChatItem.Message(it.message.copy(replies = it.message.replies + msg))
                    } else {
                        it
                    }
                }
                s.copy(items = items)
            }
        }
        _meetup.update { it.copy(chatCount = it.chatCount + 1) }
    }

    override fun setMeetPointMode(mode: MeetPointMode) = _meetPoint.update { it.copy(mode = mode) }

    override fun setMeetPointLabel(label: String) = _meetPoint.update { it.copy(label = label) }

    override fun moveMeetPoint(by: MapPoint) = _meetPoint.update {
        it.copy(pin = MapPoint(it.pin.x + by.x, it.pin.y + by.y))
    }

    override fun shareMeetPoint() {
        val label = _meetPoint.value.label.ifBlank { res.getString(R.string.meet_point_default) }
        _meetup.update {
            it.copy(meetPoint = MeetPoint(res.getString(R.string.meet_at, label), "You", 60, it.meetPoint?.at ?: MapPoint(218f, 196f)))
        }
    }

    private fun compassFor(id: String): CompassState = when (id) {
        "ben" -> CompassState(compassTargets, id, "Ben", 40f, Direction.NE, 120, 2, 6, listOf("Ben", "Chloé"))
        else -> CompassState(compassTargets, "mp", "Bandstand", 42f, Direction.NE, 85, 1, 6, listOf("Ben", "Chloé"))
    }

    override fun selectCompassTarget(targetId: String) {
        val target = compassTargets.firstOrNull { it.id == targetId } ?: return
        if (target is CompassTarget.Member && target.locked) {
            askLocation(target.id)
        } else {
            _compass.value = compassFor(targetId)
        }
    }

    override fun setShareScope(scope: ShareScope) = _locationRequest.update { it.copy(scope = scope) }

    override fun setAskBack(on: Boolean) = _locationRequest.update { it.copy(askBack = on) }

    override fun setAlwaysAllow(on: Boolean) = _locationRequest.update { it.copy(alwaysAllow = on) }

    override fun answerLocationRequest(allow: Boolean) {
        if (!allow) return
        val from = _locationRequest.value.from
        _meetup.update { s ->
            s.copy(sharedWith = (s.sharedWith + from.name).distinct())
        }
    }

    override fun simulateCoPresence() = _home.update {
        it.copy(alert = CoPresenceAlert("m1", "Climbing crew", listOf("Ben", "Chloé"), "Riverside Park"))
    }

    override fun simulateLocationRequest() = _locationRequest.update {
        it.copy(scope = ShareScope.ThisMeetup, askBack = true, alwaysAllow = false)
    }

    override fun simulateMeetupEnded() = Unit
}
