package org.pingers.app.data

import uniffi.pingers.LocalClock
import java.util.TimeZone

/*
 * pingers-ffi's records (package uniffi.pingers, generated from crates/pingers-ffi) onto the
 * UI's state, one function per view model. Conversion only: every decision was made in Rust
 * (pingers-screens computes the values, pingers-words writes the sentences). The only thing
 * added here is where each map is centred (MapFrame), which belongs to the placeholder map.
 */

// ---------------------------------------------------------------- Ids and common values

internal fun uniffi.pingers.PartyId.toUi() = PartyId(hex)
internal fun uniffi.pingers.SpotId.toUi() = SpotId(hex)
internal fun uniffi.pingers.MeetupId.toUi() = MeetupId(hex)
internal fun uniffi.pingers.InviteId.toUi() = InviteId(hex)
internal fun uniffi.pingers.MessageId.toUi() = MessageId(hex)
internal fun uniffi.pingers.LocationRequestId.toUi() = RequestId(hex)
internal fun uniffi.pingers.MemberId.toUi() = MemberId(leaf.toLong())
internal fun uniffi.pingers.ZoneId.toUi() = ZoneId(row.toLong())

internal fun PartyId.toFfi() = uniffi.pingers.PartyId(hex)
internal fun InviteId.toFfi() = uniffi.pingers.InviteId(hex)
internal fun SpotId.toFfi() = uniffi.pingers.SpotId(hex)
internal fun MeetupId.toFfi() = uniffi.pingers.MeetupId(hex)
internal fun MessageId.toFfi() = uniffi.pingers.MessageId(hex)
internal fun RequestId.toFfi() = uniffi.pingers.LocationRequestId(hex)
internal fun MemberId.toFfi() = uniffi.pingers.MemberId(leaf.toUInt())
internal fun ZoneId.toFfi() = uniffi.pingers.ZoneId(row.toUInt())

internal fun uniffi.pingers.Coordinate.toUi() = Coordinate(latitude, longitude)
internal fun Coordinate.toFfi() = uniffi.pingers.Coordinate(latitude, longitude)

internal fun uniffi.pingers.Fence.toUi(): Fence = when (this) {
    is uniffi.pingers.Fence.Circle -> Fence.Circle(center.toUi(), radiusM)
    is uniffi.pingers.Fence.Polygon -> Fence.Polygon(vertices.map { it.toUi() })
}

internal fun uniffi.pingers.Tint.toUi(): Tint = when (this) {
    uniffi.pingers.Tint.PEACH -> Tint.Peach
    uniffi.pingers.Tint.SKY -> Tint.Sky
    uniffi.pingers.Tint.SAGE -> Tint.Sage
    uniffi.pingers.Tint.BUTTER -> Tint.Butter
    uniffi.pingers.Tint.LILAC -> Tint.Lilac
    uniffi.pingers.Tint.MINT -> Tint.Mint
    uniffi.pingers.Tint.STONE -> Tint.Stone
}

internal fun uniffi.pingers.PartyColour.toUi(): PartyColour = when (this) {
    uniffi.pingers.PartyColour.BLUE -> PartyColour.Blue
    uniffi.pingers.PartyColour.ORANGE -> PartyColour.Orange
    uniffi.pingers.PartyColour.INK -> PartyColour.Ink
    uniffi.pingers.PartyColour.TEAL -> PartyColour.Teal
    uniffi.pingers.PartyColour.PLUM -> PartyColour.Plum
}

internal fun PartyColour.toFfi(): uniffi.pingers.PartyColour = when (this) {
    PartyColour.Blue -> uniffi.pingers.PartyColour.BLUE
    PartyColour.Orange -> uniffi.pingers.PartyColour.ORANGE
    PartyColour.Ink -> uniffi.pingers.PartyColour.INK
    PartyColour.Teal -> uniffi.pingers.PartyColour.TEAL
    PartyColour.Plum -> uniffi.pingers.PartyColour.PLUM
}

internal fun uniffi.pingers.PrivacyLevel.toUi(): PrivacyLevel = when (this) {
    uniffi.pingers.PrivacyLevel.FULL -> PrivacyLevel.Full
    uniffi.pingers.PrivacyLevel.REDUCED -> PrivacyLevel.Reduced
}

internal fun uniffi.pingers.Direction.toUi(): Direction = when (this) {
    uniffi.pingers.Direction.N -> Direction.N
    uniffi.pingers.Direction.NE -> Direction.NE
    uniffi.pingers.Direction.E -> Direction.E
    uniffi.pingers.Direction.SE -> Direction.SE
    uniffi.pingers.Direction.S -> Direction.S
    uniffi.pingers.Direction.SW -> Direction.SW
    uniffi.pingers.Direction.W -> Direction.W
    uniffi.pingers.Direction.NW -> Direction.NW
}

internal fun uniffi.pingers.Person.toUi() = Person(id.toUi(), name, initials, tint.toUi(), isMe)

/** The device's clock for screens that print times. */
internal fun localClock(nowMillis: Long = System.currentTimeMillis()) = LocalClock(
    (nowMillis / 1000).toULong(),
    TimeZone.getDefault().getOffset(nowMillis) / 1000,
)

internal fun Double.metres(): Int = Math.round(this).toInt()

// ---------------------------------------------------------------- Map frames

/** Where the 390-wide scenes put the thing they centre on. */
private val SceneCentre = MapPoint(195f, 256f)

/**
 * Where a frame with nothing of its own to centre on starts: CoreRepository sets it to
 * [org.pingers.app.maps.startingPoint] (where the phone last was, else the downloaded map).
 */
internal object FrameStart {
    @Volatile var centre: () -> Coordinate? = { null }
}

/**
 * A frame centred on [centre]; when nothing has a position yet, an unplaced frame at
 * [FrameStart] (0,0 only when the phone knows nowhere and has no map).
 */
internal fun frameAt(centre: Coordinate?, at: MapPoint = SceneCentre, metresPerUnit: Float = 2f) =
    if (centre != null) {
        MapFrame(centre, at, metresPerUnit)
    } else {
        MapFrame(FrameStart.centre() ?: Coordinate(0.0, 0.0), at, metresPerUnit, placed = false)
    }

/** The Party card's frame: the Spots' middle at the card's centre, zoomed out to fit them. */
internal fun cardFrame(spots: List<Fence>): MapFrame {
    val points = spots.flatMap {
        when (it) {
            is Fence.Circle -> listOf(it.center)
            is Fence.Polygon -> it.vertices
        }
    }
    if (points.isEmpty()) return frameAt(null, MapPoint(175f, 115f))
    val centre = Coordinate(points.map { it.latitude }.average(), points.map { it.longitude }.average())
    val probe = MapFrame(centre, MapPoint(0f, 0f), 1f)
    val extent = spots.maxOf {
        when (it) {
            is Fence.Circle -> probe.toMap(it.center).let { p -> maxOf(kotlin.math.abs(p.x), kotlin.math.abs(p.y)) } + it.radiusM.toFloat()
            is Fence.Polygon -> it.vertices.maxOf { v -> probe.toMap(v).let { p -> maxOf(kotlin.math.abs(p.x), kotlin.math.abs(p.y)) } }
        }
    }
    // Fit the farthest edge inside the card's 115-unit half height, never closer than the canvas's 2 m a unit.
    return MapFrame(centre, MapPoint(175f, 115f), maxOf(2f, extent / 100f))
}

// ---------------------------------------------------------------- Home

internal fun uniffi.pingers.HomeView.toUi() = HomeState(
    sharing = SharingSummary(
        sharing = when (val s = sharing.sharing) {
            uniffi.pingers.Sharing.On -> Sharing.On
            uniffi.pingers.Sharing.Off -> Sharing.Off
            is uniffi.pingers.Sharing.Paused -> Sharing.Paused(s.because)
        },
        partyCount = sharing.partyCount.toInt(),
        pausesInFocus = sharing.pausesInFocus,
        exclusionZones = sharing.exclusionZones.toInt(),
    ),
    liveMeetup = liveMeetup?.let { LiveMeetupCard(it.meetup.toUi(), it.partyName, it.spotName, it.people.map { p -> p.toUi() }) },
    parties = parties.map {
        PartyRow(it.id.toUi(), it.name, it.initials, it.colour.toUi(), it.members.toInt(), it.spots.toInt(), it.live)
    },
    alert = alert?.toUi(),
)

internal fun uniffi.pingers.CoPresenceAlert.toUi() = CoPresenceAlert(meetup.toUi(), partyName, others, spotName)

// ---------------------------------------------------------------- Parties

internal fun uniffi.pingers.DraftPrivacy.level(): PrivacyLevel? = when (this) {
    uniffi.pingers.DraftPrivacy.Checking -> null
    is uniffi.pingers.DraftPrivacy.Known -> level.toUi()
}

internal fun uniffi.pingers.Reachability.toUi(): Reachability = when (this) {
    uniffi.pingers.Reachability.Checking -> Reachability.Checking
    is uniffi.pingers.Reachability.Reachable -> Reachability.Reachable
    uniffi.pingers.Reachability.Unreachable -> Reachability.Unreachable
}

internal fun uniffi.pingers.NewPartyView.toUi(error: String?) =
    NewPartyState(name, colour.toUi(), server, privacy.level(), canCreate, error, serverChoice, keysLine, keysTrust, text)

internal fun uniffi.pingers.AdvancedView.toUi() = AdvancedState(
    privacy = privacy.level(),
    server = server.host,
    serverCheck = server.check.toUi(),
    relay = relay.host,
    relayCheck = relay.check.toUi(),
    pushRelay = when (pushRelay) {
        uniffi.pingers.PushRelayChoice.Official -> PushRelayChoice.Official
        is uniffi.pingers.PushRelayChoice.Custom -> PushRelayChoice.Custom
    },
    postQuantum = postQuantum,
    serverChoice = serverChoice,
    builtIn = builtIn,
    text = text,
)

internal fun uniffi.pingers.PartyView.toUi(): PartyState {
    val fences = spots.map { it.fence.toUi() }
    return PartyState(
        id = id.toUi(),
        name = name,
        members = members.map { it.toUi() },
        spots = spots.zip(fences).map { (row, fence) ->
            SpotRow(
                row.id.toUi(), row.name,
                when (val s = row.shape) {
                    is uniffi.pingers.SpotShape.Circle -> SpotShape.Circle(s.radiusM.metres())
                    is uniffi.pingers.SpotShape.Area -> SpotShape.Area((s.squareM / 10_000).toFloat())
                },
                fence, row.inMeetup,
            )
        },
        mapFrame = cardFrame(fences),
        server = server,
        privacy = privacy.toUi(),
        meetup = meetup?.toUi(),
        settingsRow = settingsRow,
        settingsDetail = settingsDetail,
    )
}

internal fun uniffi.pingers.InviteView.toUi() = InviteState(
    partyName = partyName,
    link = link,
    expiresInHours = expiresInHours?.toInt(),
    invitations = invitations.map {
        when (it) {
            is uniffi.pingers.Invitation.Pending ->
                Invitation.Pending(it.id.toUi(), it.sharedHoursAgo.toInt(), it.expiresInHours.toInt())
            is uniffi.pingers.Invitation.Opened -> Invitation.Opened(
                it.id.toUi(),
                Person(MemberId(-1), it.name, it.name.take(2).uppercase(), Tint.Stone),
                it.code,
            )
            is uniffi.pingers.Invitation.Joined -> Invitation.Joined(it.id.toUi(), it.person.toUi())
        }
    },
)

internal fun uniffi.pingers.OpenInviteView.toUi(error: String?) = OpenInviteState(
    title, subtitle, whatJoiningMeans, points, joinAs, nameLabel, nameHelp, ask, notNow, error,
)

internal fun uniffi.pingers.AskedToJoinView.toUi() = AskedToJoinState(
    title, subtitle, readCode, code, warning, leaveNote, done, withdraw,
)

internal fun uniffi.pingers.LetInView.toUi() = LetInState(
    party.toUi(), colour.toUi(), partyInitials, letInBy, partyName, preview.map { it.toUi() }, more,
    counts, checked, open, leave, notificationTitle, notificationBody,
)

internal fun uniffi.pingers.JoinRequestView.toUi() = JoinRequestState(
    id.toUi(), avatar, title, subtitle, ask, code, warning, whenHeading, whenBody, letIn, turnAway,
)

// ---------------------------------------------------------------- Spots and privacy

internal fun ShapeKind.toFfi() = when (this) {
    ShapeKind.Circle -> uniffi.pingers.ShapeKind.CIRCLE
    ShapeKind.Area -> uniffi.pingers.ShapeKind.AREA
}

internal fun uniffi.pingers.SpotEditorView.toUi() = SpotEditorState(
    partyName = partyName,
    name = name,
    shape = when (shape) {
        uniffi.pingers.ShapeKind.CIRCLE -> ShapeKind.Circle
        uniffi.pingers.ShapeKind.AREA -> ShapeKind.Area
    },
    radiusM = radiusM.metres(),
    minRadiusM = minRadiusM.metres(),
    maxRadiusM = maxRadiusM.metres(),
    areaHectares = areaSquareM?.let { (it / 10_000).toFloat() },
    canSave = canSave,
)

internal fun uniffi.pingers.ShapeProblem.toUi() = when (this) {
    uniffi.pingers.ShapeProblem.CROSSING -> ShapeProblem.Crossing
    uniffi.pingers.ShapeProblem.NO_AREA -> ShapeProblem.NoArea
    uniffi.pingers.ShapeProblem.TOO_LARGE -> ShapeProblem.TooLarge
}

internal fun uniffi.pingers.DrawAreaView.toUi(frame: MapFrame) = DrawAreaState(
    spotName = spotName,
    points = points.map { it.toUi() },
    midpoints = midpoints.map { it.toUi() },
    closed = closed,
    selected = selected?.toInt(),
    closable = closable,
    canUndo = canUndo,
    canDelete = canDelete,
    areaM2 = areaSquareM?.toFloat(),
    problem = problem?.toUi(),
    canUse = canUse,
    hint = when (val h = hint) {
        uniffi.pingers.DrawHint.FirstPoint -> DrawHint.FirstPoint
        uniffi.pingers.DrawHint.KeepTapping -> DrawHint.KeepTapping
        uniffi.pingers.DrawHint.CloseShape -> DrawHint.CloseShape
        is uniffi.pingers.DrawHint.Fix -> DrawHint.Fix(h.problem.toUi())
        is uniffi.pingers.DrawHint.Selected -> DrawHint.Selected(h.index.toInt() + 1)
        uniffi.pingers.DrawHint.Adjust -> DrawHint.Adjust
    },
    mapFrame = frame,
)

internal fun uniffi.pingers.ExclusionZonesView.toUi() = ExclusionZonesState(
    zones = zones.map {
        ZoneRow(
            it.id.toUi(), it.name,
            when (val s = it.shape) {
                is uniffi.pingers.ZoneShape.Circle -> ZoneShape.Circle(s.radiusM.metres())
                is uniffi.pingers.ZoneShape.Area -> ZoneShape.Area(s.points.map { p -> p.toUi() })
            },
            it.detail, it.labelAt.toUi(),
        )
    },
    hideTrail = hideTrail,
    me = me?.toUi(),
    mapFrame = frameAt(me?.toUi(), MapPoint(175f, 125f)),
)

internal fun uniffi.pingers.PrivacyView.toUi() = PrivacyState(
    pausedBecause = pausedBecause,
    sharing = sharing,
    pauseInFocus = pauseInFocus,
    quietHours = quietHours,
    quietHoursRange = quietHoursRange,
    exclusionZones = exclusionZones.toInt(),
    visibility = when (visibility) {
        uniffi.pingers.Visibility.ASK_EACH_TIME -> Visibility.AskEachTime
        uniffi.pingers.Visibility.EVERYONE -> Visibility.Everyone
        uniffi.pingers.Visibility.NOBODY -> Visibility.Nobody
    },
    noWakePushes = noWakePushes,
    pushDelivery = pushDelivery,
)

internal fun uniffi.pingers.ServerSeesView.toUi() = ServerViewState(server, selfHosted, privacy.toUi(), canSee, neverSees)

// ---------------------------------------------------------------- Meetup

internal fun uniffi.pingers.MeetupView.toUi(): MeetupState {
    val firstShared = members.firstNotNullOfOrNull { (it.location as? uniffi.pingers.PeerLocation.Shared)?.at }
    val centre = (me ?: meetPoint?.at ?: firstShared)?.toUi()
    return MeetupState(
        id = id.toUi(),
        partyName = partyName,
        spotName = spotName,
        hereCount = hereCount.toInt(),
        meetPoint = meetPoint?.let { MeetPoint(it.title, it.setBy.name, it.distanceM?.metres(), it.at?.toUi()) },
        members = members.map {
            MeetupMember(
                it.person.toUi(),
                when (val l = it.location) {
                    is uniffi.pingers.PeerLocation.Shared ->
                        PeerLocation.Shared(l.distanceM?.metres(), l.direction?.toUi(), l.at.toUi(), l.updated)
                    uniffi.pingers.PeerLocation.Private -> PeerLocation.Private
                    uniffi.pingers.PeerLocation.Asked -> PeerLocation.Asked
                },
            )
        },
        me = me?.toUi(),
        chatCount = chatCount.toInt(),
        sharedWith = sharedWith,
        // The canvas puts me low and left of centre.
        mapFrame = frameAt(centre, MapPoint(149f, 291f)),
    )
}

private fun uniffi.pingers.ChatMessage.toUi(): ChatMessage =
    ChatMessage(id.toUi(), author.toUi(), body, replies.map { it.toUi() })

internal fun uniffi.pingers.ChatView.toUi() = ChatState(
    spotName = spotName,
    participants = participants.map { it.name },
    items = items.map {
        when (it) {
            is uniffi.pingers.ChatItem.Joined -> ChatItem.Joined(it.person.name, it.time)
            is uniffi.pingers.ChatItem.Message -> ChatItem.Message(it.message.toUi())
            is uniffi.pingers.ChatItem.MeetPointSet -> ChatItem.MeetPointSet(it.author.toUi(), it.label.orEmpty())
        }
    },
    replyingTo = replyingTo?.toUi(),
)

internal fun MeetPointMode.toFfi() = when (this) {
    MeetPointMode.Pin -> uniffi.pingers.MeetPointMode.PIN
    MeetPointMode.AtMe -> uniffi.pingers.MeetPointMode.AT_ME
    MeetPointMode.Midpoint -> uniffi.pingers.MeetPointMode.MIDPOINT
}

internal fun uniffi.pingers.MeetPointView.toUi(pin: Coordinate, frame: MapFrame) = MeetPointState(
    mode = when (mode) {
        uniffi.pingers.MeetPointMode.PIN -> MeetPointMode.Pin
        uniffi.pingers.MeetPointMode.AT_ME -> MeetPointMode.AtMe
        uniffi.pingers.MeetPointMode.MIDPOINT -> MeetPointMode.Midpoint
    },
    label = label,
    pin = pin,
    point = point?.toUi(),
    distances = distances.map { MeetPointDistance(it.person.name, it.distanceM?.metres()) },
    members = members.filterNot { it.person.isMe }.map { it.person.toUi() to it.at.toUi() },
    canShare = canShare,
    mapFrame = frame,
)

internal fun CompassSelection.toFfi(): uniffi.pingers.CompassSelection = when (this) {
    CompassSelection.MeetPoint -> uniffi.pingers.CompassSelection.MeetPoint
    is CompassSelection.Member -> uniffi.pingers.CompassSelection.Member(id.toFfi())
}

private fun uniffi.pingers.CompassTarget.toUi(): CompassTarget = when (this) {
    is uniffi.pingers.CompassTarget.MeetPoint -> CompassTarget.Place(label)
    is uniffi.pingers.CompassTarget.Member -> CompassTarget.Member(person.id.toUi(), person.name, locked)
}

private fun uniffi.pingers.CompassTarget.label(): String = when (this) {
    is uniffi.pingers.CompassTarget.MeetPoint -> label
    is uniffi.pingers.CompassTarget.Member -> person.name
}

internal fun uniffi.pingers.CompassView.toUi() = CompassState(
    targets = targets.map { it.toUi() },
    selected = when (val s = selected) {
        uniffi.pingers.CompassSelection.MeetPoint -> CompassSelection.MeetPoint
        is uniffi.pingers.CompassSelection.Member -> CompassSelection.Member(s.id.toUi())
    },
    targetLabel = target.label(),
    reading = reading?.let {
        CompassReading(it.bearingDeg.toFloat(), it.direction.toUi(), it.distanceM.metres(), it.walkMinutes.toInt(), it.updated)
    },
    gpsAccuracyM = gpsAccuracyM?.metres(),
    watchers = watchers,
)

internal fun LocationRequestState.answer() = uniffi.pingers.AnswerDraft(
    scope = when (scope) {
        ShareScope.ThisMeetup -> uniffi.pingers.ShareScope.THIS_MEETUP
        ShareScope.FifteenMinutes -> uniffi.pingers.ShareScope.FIFTEEN_MINUTES
    },
    askBack = askBack,
    alwaysAllow = alwaysAllow,
)

internal fun uniffi.pingers.LocationRequestView.toUi(meetup: MeetupId) = LocationRequestState(
    id = id.toUi(),
    meetupId = meetup,
    from = from.toUi(),
    spotName = spotName,
    partyName = partyName,
    scope = when (answer.scope) {
        uniffi.pingers.ShareScope.THIS_MEETUP -> ShareScope.ThisMeetup
        uniffi.pingers.ShareScope.FIFTEEN_MINUTES -> ShareScope.FifteenMinutes
    },
    askBack = answer.askBack,
    alwaysAllow = answer.alwaysAllow,
)

internal fun uniffi.pingers.MeetupEndedView.toUi() =
    MeetupEndedState(spotName, durationMinutes.toInt(), from, to, people.toInt(), messages.toInt())
