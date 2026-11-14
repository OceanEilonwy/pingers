package org.pingers.app.data

import kotlinx.coroutines.flow.StateFlow
import uniffi.pingers.DeviceSettingsView
import uniffi.pingers.HomeWakeupsView
import uniffi.pingers.LeavePartyView
import uniffi.pingers.NoticeView
import uniffi.pingers.PartyCodeView
import uniffi.pingers.PartySettingsView
import uniffi.pingers.PrivacyText
import uniffi.pingers.QuietHoursEditorView
import uniffi.pingers.SafetyCodesView
import uniffi.pingers.ServerChoice
import uniffi.pingers.SetupStep
import uniffi.pingers.SetupView
import uniffi.pingers.TimeOfDay
import uniffi.pingers.WakeupsView
import uniffi.pingers.YourNameView
import uniffi.pingers.ZoneTimes
import uniffi.pingers.ZoneTimesEditorView

/**
 * Everything the screens read and every action they take. The composables see
 * only this interface; FakeRepository backs it today, and the Rust core
 * (pingers-screens through pingers-ffi) will back it later. Each flow is one
 * screen's state; each method is one user action, mapped later to a Core call.
 */
interface PingersRepository {
    val home: StateFlow<HomeState>
    val newParty: StateFlow<NewPartyState>
    val advanced: StateFlow<AdvancedState>
    val invite: StateFlow<InviteState>
    val openInvite: StateFlow<OpenInviteState>
    /** Null unless this phone asked to join a Party and waits to be let in. */
    val askedToJoin: StateFlow<AskedToJoinState?>
    /** Null unless this phone was let in and You're in has not been dismissed. */
    val letIn: StateFlow<LetInState?>
    /** The request [openJoinRequest] chose; null once it was decided or withdrawn. */
    val joinRequest: StateFlow<JoinRequestState?>
    val party: StateFlow<PartyState>
    val spotEditor: StateFlow<SpotEditorState>
    val drawArea: StateFlow<DrawAreaState>
    val exclusionZones: StateFlow<ExclusionZonesState>
    val privacy: StateFlow<PrivacyState>
    val serverView: StateFlow<ServerViewState>
    val meetup: StateFlow<MeetupState>
    val chat: StateFlow<ChatState>
    val meetPoint: StateFlow<MeetPointState>
    val compass: StateFlow<CompassState>
    val locationRequest: StateFlow<LocationRequestState>
    val meetupEnded: StateFlow<MeetupEndedState>

    // The canvas's section 4, as pingers-ffi's records (every word from pingers-words). Null
    // until the core has answered, or while nothing is open on that screen.
    /** Settings, the Home cog. */
    val deviceSettings: StateFlow<DeviceSettingsView?>
    val yourName: StateFlow<YourNameView?>
    /** Quiet hours with the times picked so far. */
    val quietHours: StateFlow<QuietHoursEditorView?>
    /** The zone [openZoneTimes] chose, with the times picked so far. */
    val zoneTimes: StateFlow<ZoneTimesEditorView?>
    /** Wake-ups; its layout follows [WakeupsView.kind]. */
    val wakeups: StateFlow<WakeupsView?>
    /** What Home says about wake-ups: nothing, the grey line, or the orange banner. */
    val homeWakeups: StateFlow<HomeWakeupsView>
    /** The one notice at the top of Home, most serious first. */
    val notice: StateFlow<NoticeView?>
    /** Privacy's words. */
    val privacyText: StateFlow<PrivacyText?>
    /** Party settings for the Party [openPartySettings] chose. */
    val partySettings: StateFlow<PartySettingsView?>
    val safetyCodes: StateFlow<SafetyCodesView?>
    val leaveParty: StateFlow<LeavePartyView?>
    /** "This server needs a code", once Create party was asked for one. */
    val partyCode: StateFlow<PartyCodeView?>
    /** The registration code typed so far on it. */
    val registrationCode: StateFlow<String>
    /** Home's "Waiting to be let in" row, while this phone has asked to join and waits. */
    val pendingJoin: StateFlow<uniffi.pingers.PendingJoinView?>

    /** A first-run step's words. */
    fun setupStep(step: SetupStep): SetupView

    /** The app came to the front: read the phone's facts again (push app, battery, notifications). */
    fun refreshPhone()

    // Settings
    /** Your name's Save; [onSaved] runs once the core took it. */
    fun saveDisplayName(name: String, onSaved: () -> Unit)
    /** Quiet hours opened: start from the stored times. */
    fun openQuietHours()
    fun setQuietHoursDraft(on: Boolean, from: TimeOfDay, to: TimeOfDay)
    fun saveQuietHours(onSaved: () -> Unit)
    /** A zone row in Exclusion zones was tapped: Zone times starts from its stored times. */
    fun openZoneTimes(zone: ZoneId)
    fun setZoneTimesDraft(times: ZoneTimes)
    fun saveZoneTimes(onSaved: () -> Unit)
    fun deleteZone(onDeleted: () -> Unit)

    // Party settings
    fun openPartySettings(party: PartyId)
    /** "Check the privacy relay again". */
    fun checkRelayAgain()
    /** Safety codes opened: the first member not yet checked, with their code. */
    fun openSafetyCodes()
    fun showSafetyCode(member: MemberId)
    /** "Codes match · Mark … as checked", for the member shown. */
    fun markChecked()
    /** The Leave party sheet's "Leave party"; [onLeft] runs once the core took it. */
    fun leaveParty(onLeft: () -> Unit)

    // Parties and invites
    fun openParty(partyId: PartyId)
    fun setNewPartyName(name: String)
    fun setNewPartyColour(colour: PartyColour)
    /** "The app's server" or "Another server" on New party and Advanced. */
    fun setServerChoice(choice: ServerChoice)
    /** The code typed on "This server needs a code". */
    fun setRegistrationCode(code: String)
    /** Create party (with the code typed, if one was asked for); [onResult] says where to go. */
    fun createParty(onResult: (CreateOutcome) -> Unit)
    /** New party was opened: check the server and relay are reachable, so Create can enable. */
    fun openNewParty()
    /** Advanced was opened: check the server and relay are reachable. */
    fun openAdvanced()
    fun setServer(url: String)
    fun setRelay(url: String)
    fun setPushRelay(choice: PushRelayChoice)
    fun setPostQuantum(on: Boolean)
    fun cancelInvite(invitationId: InviteId)

    // Join flow A, the joiner's side
    /** A join link was opened: Open invite shows, and nothing reaches the core until [askToJoin]. */
    fun showInvite(link: String)
    /**
     * Open invite's "Ask to join"; [onAsked] runs once the core took it. [name] is what was typed
     * in the name field, shown only while no name is set: it is saved as Your name first.
     */
    fun askToJoin(name: String, onAsked: () -> Unit)
    fun withdrawJoin()
    /** You're in's "Open <party>": dismisses You're in and opens the Party. */
    fun openJoinedParty()
    /** You're in's "Leave Party": leaves the Party just joined and dismisses You're in. */
    fun leaveJoinedParty()

    // Join flow A, the inviter's side
    /** An opened invite was tapped on Invite people: "Ben asked to join" shows that request. */
    fun openJoinRequest(requestId: InviteId)
    fun letIn()
    fun turnAway()

    // Spots
    fun editSpot(spotId: SpotId?)
    fun setSpotName(name: String)
    fun setSpotShape(shape: ShapeKind)
    fun setSpotRadius(radiusM: Int)
    /** Moves the Spot (and the editor's map) to [to]: a dragged map or a place found by search. */
    fun moveSpot(to: Coordinate)
    fun saveSpot()

    // Draw area, the point-by-point polygon editor
    fun drawTapMap(at: Coordinate)
    fun drawTapPoint(index: Int)
    fun drawDragPoint(index: Int, to: Coordinate)
    fun drawDragEnd()
    fun drawInsertMidpoint(afterIndex: Int)
    fun drawDeleteSelected()
    fun drawUndo()
    fun drawClear()
    fun useDrawnArea()

    // Privacy controls (local only)
    fun setHideTrail(on: Boolean)
    fun setSharing(on: Boolean)
    fun setPauseInFocus(on: Boolean)
    fun setQuietHours(on: Boolean)
    fun setVisibility(visibility: Visibility)
    fun setNoWakePushes(on: Boolean)
    /** What your server sees was opened: check this phone reaches its server through the relay. */
    fun checkReachability()

    // Meetup
    fun hideMeHere()
    fun dismissAlert()
    fun askLocation(memberId: MemberId)
    fun replyInThread(messageId: MessageId?)
    fun sendChat(body: String)
    fun setMeetPointMode(mode: MeetPointMode)
    fun setMeetPointLabel(label: String)
    fun moveMeetPoint(to: Coordinate)
    fun shareMeetPoint()
    fun selectCompassTarget(target: CompassSelection)
    fun setShareScope(scope: ShareScope)
    fun setAskBack(on: Boolean)
    fun setAlwaysAllow(on: Boolean)
    fun answerLocationRequest(allow: Boolean)

    // Events the core will raise (Events.meetup_started, location_request, meetup_ended,
    // a join link opened). The debug menu on Home fires them by hand.
    fun simulateCoPresence()
    fun simulateLocationRequest()
    fun simulateMeetupEnded()
}
