package org.pingers.app.ui

/**
 * Test tags for the two-emulator scenario (`cargo xtask e2e-emulators`, xtask/src/scenario.rs).
 * MainActivity exposes them as view resource ids (`testTagsAsResourceId`), so UI Automator on
 * the host finds them in `uiautomator dump`. They name what a control does, not how it looks,
 * so a redesign keeps them. The scenario falls back to the visible text where a tag is missing,
 * which is how it finds the flow-A screens until they carry the tags listed below.
 */
object E2eTags {
    const val HOME_NEW_PARTY = "home_new_party"
    const val PARTY_ROW = "party_row"
    const val NEW_PARTY_CREATE = "new_party_create"
    const val PARTY_INVITE = "party_invite"
    const val PARTY_ADD_SPOT = "party_add_spot"
    const val INVITE_SHARE = "invite_share"
    const val ACCEPT_JOIN = "accept_join"
    const val SPOT_SAVE = "spot_save"
    const val ALERT_OPEN = "alert_open"
    const val MEETUP_CHAT = "meetup_chat"
    const val MEETUP_COMPASS = "meetup_compass"
    const val MEETUP_ASK_LOCATION = "meetup_ask_location"
    const val CHAT_SEND = "chat_send"
    const val COMPASS_DIAL = "compass_dial"
    const val LOCATION_ALLOW = "location_allow"
    const val ENDED_DONE = "ended_done"

    // Flow A (ask to join, design canvas JoinA1 to JoinA3 and InviteApprove). The scenario
    // looks for these first; the screens that carry them are not built yet.
    /** JoinA1: the "Your name in this Party" field. */
    const val JOIN_NAME = "join_name"
    /** JoinA1: "Ask to join". */
    const val JOIN_ASK = "join_ask"
    /** JoinA2: the safety code the joiner reads out. */
    const val JOIN_CODE = "join_code"
    /** JoinA3: "Open <party>". */
    const val JOINED_OPEN = "joined_open"
    /** InviteApprove: the code the inviter compares. */
    const val APPROVE_CODE = "approve_code"
    /** InviteApprove: "Codes match · Let <name> in". */
    const val APPROVE_LET_IN = "approve_let_in"
}
