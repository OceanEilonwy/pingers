package org.pingers.app.data

import kotlin.math.PI
import kotlin.math.cos
import uniffi.pingers.AdvancedText
import uniffi.pingers.JoinAs
import uniffi.pingers.NewPartyText
import uniffi.pingers.ServerChoice

/*
 * What each screen shows: one plain data class per screen, no behaviour. They follow the
 * view models of the Rust crate pingers-screens field for field (typed ids, coordinates,
 * enums, the can* and problem fields), so CoreRepository maps the generated UniFFI records
 * onto them one to one and FakeRepository fills them with the canvas's example.
 *
 * The screens of the canvas's section 4 (Settings, Wake-ups, notices, Party settings and the
 * rest) take pingers-ffi's records as they are instead: they are words and flags only, written
 * by pingers-words, so a copy here would add nothing.
 *
 * Positions are WGS84 coordinates. The placeholder map still draws in its own units (the
 * canvas's 390-wide artboard); each map screen carries a MapFrame that places coordinates
 * on it. When MapLibre arrives the frame becomes its camera.
 */

// ---------------------------------------------------------------- Ids

/** A Party (pingers-proto PartyId, as hex). */
@JvmInline value class PartyId(val hex: String)

/** A Spot of a Party. */
@JvmInline value class SpotId(val hex: String)

/** A Meetup (its MLS group). */
@JvmInline value class MeetupId(val hex: String)

/** An invite this device made. */
@JvmInline value class InviteId(val hex: String)

/** A chat message. */
@JvmInline value class MessageId(val hex: String)

/** A request to see someone's exact location. */
@JvmInline value class RequestId(val hex: String)

/** A member of a Party or Meetup: their leaf in its group. */
@JvmInline value class MemberId(val leaf: Long)

/** An exclusion zone, stored on this device only. */
@JvmInline value class ZoneId(val row: Long)

// ---------------------------------------------------------------- Common

/** Avatar background tints from the canvas, so the palette stays in the theme. */
enum class Tint { Peach, Sky, Sage, Butter, Lilac, Mint, Stone }

/** The five party colours offered on New party. */
enum class PartyColour { Blue, Orange, Ink, Teal, Plum }

data class Person(val id: MemberId, val name: String, val initials: String, val tint: Tint, val isMe: Boolean = false)

/** A WGS84 position in degrees. */
data class Coordinate(val latitude: Double, val longitude: Double)

/** A point on the placeholder map, in its units. */
data class MapPoint(val x: Float, val y: Float)

/**
 * Places coordinates on the placeholder map: [origin] sits at [originAt], and one map unit is
 * [metresPerUnit] metres (the canvas's scale is 2). Equirectangular, which is exact enough at
 * the scale of a park.
 */
data class MapFrame(val origin: Coordinate, val originAt: MapPoint, val metresPerUnit: Float = 2f) {
    private val metresPerDegreeLon get() = METRES_PER_DEGREE * cos(origin.latitude * PI / 180)

    fun toMap(c: Coordinate): MapPoint = MapPoint(
        originAt.x + ((c.longitude - origin.longitude) * metresPerDegreeLon / metresPerUnit).toFloat(),
        originAt.y - ((c.latitude - origin.latitude) * METRES_PER_DEGREE / metresPerUnit).toFloat(),
    )

    fun toCoordinate(p: MapPoint): Coordinate = Coordinate(
        origin.latitude - (p.y - originAt.y) * metresPerUnit / METRES_PER_DEGREE,
        origin.longitude + (p.x - originAt.x) * metresPerUnit / metresPerDegreeLon,
    )

    /** A length in metres, in map units. */
    fun units(metres: Double): Float = (metres / metresPerUnit).toFloat()

    private companion object {
        const val METRES_PER_DEGREE = 111_320.0
    }
}

/** A Spot's or zone's shape. */
sealed interface Fence {
    data class Circle(val center: Coordinate, val radiusM: Double) : Fence
    data class Polygon(val vertices: List<Coordinate>) : Fence
}

/** How private a party's setup is, from who runs the server and the two relays. */
enum class PrivacyLevel { Full, Reduced }

enum class Direction { N, NE, E, SE, S, SW, W, NW }

// ---------------------------------------------------------------- Home

data class HomeState(
    val sharing: SharingSummary,
    val liveMeetup: LiveMeetupCard?,
    val parties: List<PartyRow>,
    /** A co-presence alert that arrived while the app was open (the in-app state of the notification). */
    val alert: CoPresenceAlert?,
)

/** Whether this device shares its presence now. */
sealed interface Sharing {
    data object On : Sharing
    data object Off : Sharing

    /** Switched on but suppressed; [because] is worded by the core ("Sleep Focus is on"). */
    data class Paused(val because: String) : Sharing
}

data class SharingSummary(val sharing: Sharing, val partyCount: Int, val pausesInFocus: Boolean, val exclusionZones: Int) {
    val on: Boolean get() = sharing != Sharing.Off
}

data class LiveMeetupCard(val meetupId: MeetupId, val partyName: String, val spotName: String, val people: List<Person>)

data class PartyRow(
    val id: PartyId,
    val name: String,
    val initials: String,
    val colour: PartyColour,
    val members: Int,
    val spots: Int,
    val live: Boolean,
)

// ---------------------------------------------------------------- Parties and invites

data class NewPartyState(
    val name: String,
    val colour: PartyColour,
    val server: String,
    /** Null while the server or relay hasn't said who runs it. */
    val privacy: PrivacyLevel?,
    val canCreate: Boolean,
    /** Why the last Create failed, worded by the core. */
    val error: String? = null,
    /** The app's own server, or another one typed in. */
    val serverChoice: ServerChoice = ServerChoice.BUILT_IN,
    /** Every word on the screen (pingers-words); null until the core has answered. */
    val text: NewPartyText? = null,
)

enum class Reachability { Checking, Reachable, Unreachable }

enum class PushRelayChoice { Official, Custom }

data class AdvancedState(
    /** Null while checking. */
    val privacy: PrivacyLevel?,
    val server: String,
    val serverCheck: Reachability,
    val relay: String,
    val relayCheck: Reachability,
    val pushRelay: PushRelayChoice,
    val postQuantum: Boolean,
    val serverChoice: ServerChoice = ServerChoice.BUILT_IN,
    /** The app's own server, as the build names it. */
    val builtIn: String = "",
    /** Every word on the screen (pingers-words); null until the core has answered. */
    val text: AdvancedText? = null,
)

/** How Create party ended, for New party and "This server needs a code" to route. */
enum class CreateOutcome {
    /** Made: on to Invite people. */
    Created,

    /** The chosen server asks for a registration code: "This server needs a code". */
    CodeNeeded,

    /** The code typed was refused: stay, with the refusal shown. */
    CodeRefused,

    /** Anything else; the screen shows why. */
    Failed,
}

sealed interface Invitation {
    val id: InviteId

    data class Opened(override val id: InviteId, val person: Person, val code: String) : Invitation
    data class Pending(override val id: InviteId, val sharedHoursAgo: Int, val expiresInHours: Int) : Invitation
    data class Joined(override val id: InviteId, val person: Person) : Invitation
}

data class InviteState(
    val partyName: String,
    /** Null until a link has been made. */
    val link: String?,
    val expiresInHours: Int?,
    val invitations: List<Invitation>,
)

/** Open invite (canvas JoinA1): nothing about the Party yet, only the name to go by. */
data class OpenInviteState(
    val title: String,
    val subtitle: String,
    val whatJoiningMeans: String,
    val points: List<String>,
    /** "You'll join as Ben · Change", once a name is set; the name field shows only without it. */
    val joinAs: JoinAs?,
    /** "Your name": what is typed here becomes Your name. */
    val nameLabel: String,
    val nameHelp: String,
    val ask: String,
    val notNow: String,
    /** Why the last "Ask to join" failed, if it did. */
    val error: String? = null,
)

/** Asked to join (canvas JoinA2): the code to read to the inviter. */
data class AskedToJoinState(
    val title: String,
    val subtitle: String,
    val readCode: String,
    val code: String,
    val warning: String,
    val leaveNote: String,
    val done: String,
    val withdraw: String,
)

/** You're in (canvas JoinA3), once the inviter let this phone in. */
data class LetInState(
    val party: PartyId,
    val colour: PartyColour,
    val partyInitials: String,
    val letInBy: String,
    val partyName: String,
    val preview: List<Person>,
    /** "+2", when there are more members than avatars. */
    val more: String?,
    val counts: String,
    val checked: String,
    val open: String,
    val leave: String,
    val notificationTitle: String,
    val notificationBody: String,
)

/** "Ben asked to join" (canvas InviteApprove), on the inviter's phone. */
data class JoinRequestState(
    val id: InviteId,
    val avatar: String,
    val title: String,
    val subtitle: String,
    val ask: String,
    val code: String,
    val warning: String,
    val whenHeading: String,
    val whenBody: String,
    val letIn: String,
    val turnAway: String,
)

sealed interface SpotShape {
    data class Circle(val radiusM: Int) : SpotShape
    data class Area(val hectares: Float) : SpotShape
}

data class SpotRow(val id: SpotId, val name: String, val shape: SpotShape, val fence: Fence, val inMeetup: Boolean)

data class PartyState(
    val id: PartyId,
    val name: String,
    val members: List<Person>,
    val spots: List<SpotRow>,
    /** Places the Spots on the card map (350x230 units). */
    val mapFrame: MapFrame,
    val server: String,
    val privacy: PrivacyLevel,
    val meetup: MeetupId?,
    /** The bottom row: "pingers.flat7.org · Full privacy", in the core's words. */
    val settingsRow: String = "",
    /** "Party settings: server, invites, leave". */
    val settingsDetail: String = "",
)

// ---------------------------------------------------------------- Spots and privacy

enum class ShapeKind { Circle, Area }

data class SpotEditorState(
    val partyName: String,
    val name: String,
    val shape: ShapeKind,
    val radiusM: Int,
    /** 100 m: OS geofences are unreliable below it (design E.2). */
    val minRadiusM: Int,
    val maxRadiusM: Int,
    val areaHectares: Float?,
    val canSave: Boolean,
    /** Where a circle Spot is centred, and where the editor's map looks. */
    val centre: Coordinate = Coordinate(0.0, 0.0),
)

/** What is wrong with a drawn area. */
enum class ShapeProblem { Crossing, NoArea, TooLarge }

/** What the draw-area editor tells the user to do next. */
sealed interface DrawHint {
    data object FirstPoint : DrawHint
    data object KeepTapping : DrawHint
    data object CloseShape : DrawHint
    data class Fix(val problem: ShapeProblem) : DrawHint
    data class Selected(val number: Int) : DrawHint
    data object Adjust : DrawHint
}

data class DrawAreaState(
    val spotName: String,
    val points: List<Coordinate>,
    /** Where a "+" sits on each edge of a closed shape, to insert a point after that index. */
    val midpoints: List<Coordinate>,
    val closed: Boolean,
    val selected: Int?,
    /** The first point is shown larger and orange while it can close the shape. */
    val closable: Boolean,
    val canUndo: Boolean,
    val canDelete: Boolean,
    val areaM2: Float?,
    val problem: ShapeProblem?,
    val canUse: Boolean,
    val hint: DrawHint,
    val mapFrame: MapFrame,
)

sealed interface ZoneShape {
    data class Circle(val radiusM: Int) : ZoneShape
    data object Area : ZoneShape
}

data class ZoneRow(
    val id: ZoneId,
    val name: String,
    val shape: ZoneShape,
    /** When and for whom the zone applies, as the core words it; null means always, for all parties. */
    val schedule: String?,
    val labelAt: Coordinate,
)

data class ExclusionZonesState(val zones: List<ZoneRow>, val hideTrail: Boolean, val me: Coordinate?, val mapFrame: MapFrame)

enum class Visibility { AskEachTime, Everyone, Nobody }

data class PrivacyState(
    /** Why sharing is paused right now, worded by the core ("Sleep Focus is on"); null when not paused. */
    val pausedBecause: String?,
    val sharing: Boolean,
    val pauseInFocus: Boolean,
    val quietHours: Boolean,
    val quietHoursRange: String,
    val exclusionZones: Int,
    val visibility: Visibility,
    val noWakePushes: Boolean,
)

/** "What your server sees": generated by pingers-words from the actual configuration. */
data class ServerViewState(
    val server: String,
    val selfHosted: Boolean,
    val privacy: PrivacyLevel,
    val canSee: List<String>,
    val neverSees: List<String>,
)

// ---------------------------------------------------------------- Meetup

data class CoPresenceAlert(val meetupId: MeetupId, val partyName: String, val others: List<String>, val spotName: String)

/** The meet point card. [distanceM] and [at] are null until both positions are known. */
data class MeetPoint(val label: String, val setBy: String, val distanceM: Int?, val at: Coordinate?)

/** What a member's exact location looks like to me. */
sealed interface PeerLocation {
    /**
     * [distanceM] and [direction] are null until this device has a fix. [updated] is
     * "Updated 40 s ago" / "Updated just now", from pingers-screens and pingers-words.
     */
    data class Shared(val distanceM: Int?, val direction: Direction?, val at: Coordinate, val updated: String?) : PeerLocation
    data object Private : PeerLocation
    data object Asked : PeerLocation
}

data class MeetupMember(val person: Person, val location: PeerLocation)

data class MeetupState(
    val id: MeetupId,
    val partyName: String,
    val spotName: String,
    val hereCount: Int,
    val meetPoint: MeetPoint?,
    val members: List<MeetupMember>,
    /** Null until this device has a fix. */
    val me: Coordinate?,
    val chatCount: Int,
    /** Whom my exact spot is shared with. */
    val sharedWith: List<String>,
    val mapFrame: MapFrame,
)

data class ChatMessage(
    val id: MessageId,
    val author: Person,
    val body: String,
    val replies: List<ChatMessage> = emptyList(),
)

sealed interface ChatItem {
    data class Joined(val name: String, val time: String) : ChatItem
    data class Message(val message: ChatMessage) : ChatItem
    data class MeetPointSet(val author: Person, val label: String) : ChatItem
}

data class ChatState(
    val spotName: String,
    val participants: List<String>,
    val items: List<ChatItem>,
    /** The message whose thread the composer replies in; null for the main thread. */
    val replyingTo: ChatMessage?,
)

/** How the meet point is chosen: a dropped pin, "At me" (my live position), or the midpoint. */
enum class MeetPointMode { Pin, AtMe, Midpoint }

data class MeetPointDistance(val name: String, val distanceM: Int?)

data class MeetPointState(
    val mode: MeetPointMode,
    val label: String,
    /** Where the dropped pin is. */
    val pin: Coordinate,
    /** Where the meet point would be in this mode; null if not known (no fix yet). */
    val point: Coordinate?,
    val distances: List<MeetPointDistance>,
    val members: List<Pair<Person, Coordinate>>,
    val canShare: Boolean,
    val mapFrame: MapFrame,
)

/** What the compass points at. */
sealed interface CompassSelection {
    data object MeetPoint : CompassSelection
    data class Member(val id: MemberId) : CompassSelection
}

sealed interface CompassTarget {
    val selection: CompassSelection

    data class Place(val label: String) : CompassTarget {
        override val selection: CompassSelection get() = CompassSelection.MeetPoint
    }

    data class Member(val id: MemberId, val name: String, val locked: Boolean) : CompassTarget {
        override val selection: CompassSelection get() = CompassSelection.Member(id)
    }
}

/** Where the target is from here. */
data class CompassReading(
    /** From me to the target, degrees clockwise from true north. */
    val bearingDeg: Float,
    val direction: Direction,
    val distanceM: Int,
    val walkMinutes: Int,
    /** "Updated 40 s ago" / "Updated just now", under the distance. */
    val updated: String?,
)

data class CompassState(
    val targets: List<CompassTarget>,
    val selected: CompassSelection,
    val targetLabel: String,
    /** Null until both positions are known. */
    val reading: CompassReading?,
    val gpsAccuracyM: Int?,
    /** Who sees that I'm heading over. */
    val watchers: List<String>,
)

enum class ShareScope { ThisMeetup, FifteenMinutes }

data class LocationRequestState(
    val id: RequestId,
    val meetupId: MeetupId,
    val from: Person,
    val spotName: String,
    val partyName: String,
    val scope: ShareScope,
    val askBack: Boolean,
    val alwaysAllow: Boolean,
)

data class MeetupEndedState(
    val spotName: String,
    val durationMinutes: Int,
    val from: String,
    val to: String,
    val people: Int,
    val messages: Int,
)
