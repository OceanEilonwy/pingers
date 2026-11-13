package org.pingers.app.data

import kotlinx.coroutines.flow.StateFlow

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
    val acceptInvite: StateFlow<AcceptInviteState>
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

    // Parties and invites
    fun openParty(partyId: String)
    fun setNewPartyName(name: String)
    fun setNewPartyColour(colour: PartyColour)
    fun createParty()
    /** Advanced was opened: check the server and relay are reachable. */
    fun openAdvanced()
    fun setServer(url: String)
    fun setRelay(url: String)
    fun setPushRelay(choice: PushRelayChoice)
    fun setPostQuantum(on: Boolean)
    fun verifyInvite(invitationId: String)
    fun cancelInvite(invitationId: String)
    fun acceptInvite()
    fun declineInvite()

    // Spots
    fun editSpot(spotId: String?)
    fun setSpotName(name: String)
    fun setSpotShape(shape: ShapeKind)
    fun setSpotRadius(radiusM: Int)
    fun saveSpot()

    // Draw area, the point-by-point polygon editor (map units)
    fun drawTapMap(at: MapPoint)
    fun drawTapPoint(index: Int)
    fun drawDragPoint(index: Int, to: MapPoint)
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

    // Meetup
    fun hideMeHere()
    fun dismissAlert()
    fun askLocation(memberId: String)
    fun replyInThread(messageId: String?)
    fun sendChat(body: String)
    fun setMeetPointMode(mode: MeetPointMode)
    fun setMeetPointLabel(label: String)
    fun moveMeetPoint(by: MapPoint)
    fun shareMeetPoint()
    fun selectCompassTarget(targetId: String)
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
