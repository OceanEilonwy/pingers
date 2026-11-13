package org.pingers.app.data

/*
 * What each screen shows: one plain data class per screen, no behaviour. They are
 * shaped after what the Rust crate pingers-screens will publish, so that the FFI
 * work can replace FakeRepository with a mapping from the generated UniFFI records
 * without touching the composables.
 *
 * Positions on the placeholder map are in map units (the canvas's 390-wide
 * artboard, 1 unit = 2 m). When MapLibre arrives they become coordinates.
 */

/** Avatar background tints from the canvas, so the palette stays in the theme. */
enum class Tint { Peach, Sky, Sage, Butter, Lilac, Mint, Stone }

/** The five party colours offered on New party. */
enum class PartyColour { Blue, Orange, Ink, Teal, Plum }

data class Person(val id: String, val name: String, val initials: String, val tint: Tint, val isMe: Boolean = false)

data class MapPoint(val x: Float, val y: Float)

/** How private a party's setup is, from who runs the server and the two relays. */
enum class PrivacyLevel { Full, Reduced }

// ---------------------------------------------------------------- Home

data class HomeState(
    val sharing: SharingSummary,
    val liveMeetup: LiveMeetupCard?,
    val parties: List<PartyRow>,
    /** A co-presence alert that arrived while the app was open (the in-app state of the notification). */
    val alert: CoPresenceAlert?,
)

data class SharingSummary(val on: Boolean, val partyCount: Int, val pausesInFocus: Boolean, val exclusionZones: Int)

data class LiveMeetupCard(val meetupId: String, val partyName: String, val spotName: String, val people: List<Person>)

data class PartyRow(
    val id: String,
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
    val privacy: PrivacyLevel,
)

enum class Reachability { Checking, Reachable, Unreachable }

enum class PushRelayChoice { Official, Custom }

data class AdvancedState(
    val privacy: PrivacyLevel,
    val server: String,
    val serverCheck: Reachability,
    val relay: String,
    val relayCheck: Reachability,
    val pushRelay: PushRelayChoice,
    val postQuantum: Boolean,
)

sealed interface Invitation {
    val id: String

    data class Opened(override val id: String, val person: Person, val code: String) : Invitation
    data class Pending(override val id: String, val sharedHoursAgo: Int, val expiresInHours: Int) : Invitation
    data class Joined(override val id: String, val person: Person) : Invitation
}

data class InviteState(
    val partyName: String,
    val link: String,
    val expiresInHours: Int,
    val invitations: List<Invitation>,
)

data class AcceptInviteState(
    val inviter: String,
    val partyName: String,
    val partyInitials: String,
    val colour: PartyColour,
    val preview: List<Person>,
    val moreMembers: Int,
    val members: Int,
    val spots: Int,
    val safetyCode: String,
)

sealed interface SpotShape {
    data class Circle(val radiusM: Int) : SpotShape
    data class Area(val hectares: Float) : SpotShape
}

data class SpotRow(val id: String, val name: String, val shape: SpotShape, val inMeetup: Boolean)

data class MapCircle(val center: MapPoint, val radius: Float, val small: Boolean = false)

data class PartyState(
    val id: String,
    val name: String,
    val members: List<Person>,
    val spots: List<SpotRow>,
    /** The party's Spots on the card map, in its 350x230 units. */
    val mapSpots: List<MapCircle>,
    val server: String,
    val privacy: PrivacyLevel,
)

// ---------------------------------------------------------------- Spots and privacy

enum class ShapeKind { Circle, Area }

data class SpotEditorState(
    val partyName: String,
    val name: String,
    val shape: ShapeKind,
    val radiusM: Int,
    val minRadiusM: Int,
    val maxRadiusM: Int,
    val areaHectares: Float?,
)

/** What the draw-area editor tells the user to do next. */
sealed interface DrawHint {
    data object FirstPoint : DrawHint
    data object KeepTapping : DrawHint
    data object CloseShape : DrawHint
    data class Selected(val number: Int) : DrawHint
    data object Adjust : DrawHint
}

data class DrawAreaState(
    val spotName: String,
    val points: List<MapPoint>,
    /** Where a "+" sits on each edge of a closed shape, to insert a point after that index. */
    val midpoints: List<MapPoint>,
    val closed: Boolean,
    val selected: Int?,
    /** The first point is shown larger and orange while it can close the shape. */
    val closable: Boolean,
    val canUndo: Boolean,
    val canDelete: Boolean,
    val areaM2: Float?,
    val hint: DrawHint,
)

sealed interface ZoneShape {
    data class Circle(val radiusM: Int) : ZoneShape
    data object Area : ZoneShape
}

data class ZoneRow(
    val id: String,
    val name: String,
    val shape: ZoneShape,
    /** When the zone applies, as the core words it; null means always, for all parties. */
    val schedule: String?,
    val labelAt: MapPoint,
)

data class ExclusionZonesState(val zones: List<ZoneRow>, val hideTrail: Boolean, val me: MapPoint)

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

data class CoPresenceAlert(val meetupId: String, val partyName: String, val others: List<String>, val spotName: String)

data class MeetPoint(val label: String, val setBy: String, val distanceM: Int, val at: MapPoint)

/** What a member's exact location looks like to me. */
sealed interface PeerLocation {
    data class Shared(val distanceM: Int, val direction: Direction, val at: MapPoint) : PeerLocation
    data object Private : PeerLocation
    data object Asked : PeerLocation
}

enum class Direction { N, NE, E, SE, S, SW, W, NW }

data class MeetupMember(val person: Person, val location: PeerLocation)

data class MeetupState(
    val id: String,
    val partyName: String,
    val spotName: String,
    val hereCount: Int,
    val meetPoint: MeetPoint?,
    val members: List<MeetupMember>,
    val me: MapPoint,
    val chatCount: Int,
    /** Whom my exact spot is shared with. */
    val sharedWith: List<String>,
)

data class ChatMessage(
    val id: String,
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

enum class MeetPointMode { Pin, AtPerson, Midpoint }

data class MeetPointDistance(val name: String, val distanceM: Int?)

data class MeetPointState(
    val mode: MeetPointMode,
    val label: String,
    val pin: MapPoint,
    val distances: List<MeetPointDistance>,
    val members: List<Pair<Person, MapPoint>>,
)

sealed interface CompassTarget {
    val id: String

    data class Place(override val id: String, val label: String) : CompassTarget
    data class Member(override val id: String, val name: String, val locked: Boolean) : CompassTarget
}

data class CompassState(
    val targets: List<CompassTarget>,
    val selected: String,
    val targetLabel: String,
    /** From me to the target, degrees clockwise from true north. */
    val bearingDeg: Float,
    val direction: Direction,
    val distanceM: Int,
    val walkMinutes: Int,
    val gpsAccuracyM: Int,
    /** Who sees that I'm heading over. */
    val watchers: List<String>,
)

enum class ShareScope { ThisMeetup, FifteenMinutes }

data class LocationRequestState(
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
