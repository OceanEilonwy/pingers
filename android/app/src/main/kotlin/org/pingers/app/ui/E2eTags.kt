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
    // looks for these first.
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
    /** Invite people: "Verify" on an opened invite, which opens InviteApprove. */
    const val INVITE_VERIFY = "invite_verify"
    /** JoinA1: "Change" on "You'll join as Ben", which opens Your name. */
    const val JOIN_CHANGE_NAME = "join_change_name"

    // Section 4 of the canvas: first run, Settings, Wake-ups, notices, Party settings.
    /** First run: the step's main button ("Allow location", "Continue"). */
    const val SETUP_CONTINUE = "setup_continue"
    /** First run: "Not now". */
    const val SETUP_NOT_NOW = "setup_not_now"
    /** Home: the cog, which opens Settings. */
    const val SETTINGS_OPEN = "settings_open"
    /** Home: the notice's link ("Open date and time settings", "Party settings"). */
    const val NOTICE_ACTION = "notice_action"
    /** Home: the orange wake-ups fault banner; tapping it (Fix) opens Wake-ups. */
    const val WAKEUPS_FIX = "wakeups_fix"
    /** Home: the grey "Wake-ups off" line. */
    const val WAKEUPS_OFF_LINE = "wakeups_off_line"
    /** Home: "Waiting to be let in", which reopens Asked to join. */
    const val HOME_PENDING_JOIN = "home_pending_join"
    /** Settings: the Your name card. */
    const val SETTINGS_YOUR_NAME = "settings_your_name"
    const val SETTINGS_QUIET_HOURS = "settings_quiet_hours"
    const val SETTINGS_WAKEUPS = "settings_wakeups"
    const val YOUR_NAME_FIELD = "your_name_field"
    const val YOUR_NAME_SAVE = "your_name_save"
    const val QUIET_HOURS_SWITCH = "quiet_hours_switch"
    const val QUIET_HOURS_SAVE = "quiet_hours_save"
    /** Exclusion zones: a zone row, which opens Zone times. */
    const val ZONE_ROW = "zone_row"
    const val ZONE_TIMES_SAVE = "zone_times_save"
    const val ZONE_DELETE = "zone_delete"
    /** Wake-ups: the "No wake-up pushes" switch. */
    const val WAKEUPS_NO_PUSHES = "wakeups_no_pushes"
    const val WAKEUPS_KEEP_RUNNING = "wakeups_keep_running"
    /** Wake-ups: "Get ntfy". */
    const val WAKEUPS_GET_PUSH_APP = "wakeups_get_push_app"
    /** Wake-ups: battery use's "Change". */
    const val WAKEUPS_BATTERY = "wakeups_battery"
    const val WAKEUPS_NOTIFICATIONS = "wakeups_notifications"
    /** Privacy: the Wake-ups link under Delivery. */
    const val PRIVACY_WAKEUPS = "privacy_wakeups"
    /** Downloaded maps: a row's Update, Delete or Download. */
    const val MAPS_ACTION = "maps_action"
    /** Party: the row at the bottom, which opens Party settings. */
    const val PARTY_SETTINGS_OPEN = "party_settings_open"
    const val PARTY_SETTINGS_INVITE = "party_settings_invite"
    /** Party settings: "Check the privacy relay again". */
    const val PARTY_CHECK_RELAY = "party_check_relay"
    /** Party settings (reduced privacy): "Wake-ups settings". */
    const val PARTY_WAKEUPS_LINK = "party_wakeups_link"
    /** Party settings: "Leave party", which opens the sheet. */
    const val PARTY_LEAVE = "party_leave"
    /** The Leave party sheet: "Leave party". */
    const val LEAVE_PARTY_CONFIRM = "leave_party_confirm"
    const val SAFETY_CODES_OPEN = "safety_codes_open"
    /** Safety codes: the twelve groups of digits. */
    const val SAFETY_CODE = "safety_code"
    /** Safety codes: "Codes match · Mark … as checked". */
    const val SAFETY_MARK_CHECKED = "safety_mark_checked"
    /** New party: "The app's server" and "Another server", and the address field. */
    const val NEW_PARTY_BUILT_IN = "new_party_built_in"
    const val NEW_PARTY_ANOTHER = "new_party_another"
    const val NEW_PARTY_SERVER = "new_party_server"
    /** New party: "Trust this server's keys" for another server. */
    const val NEW_PARTY_TRUST_KEYS = "new_party_trust_keys"
    /** "This server needs a code": the code field and "Create party" / "Try again". */
    const val PARTY_CODE_FIELD = "party_code_field"
    const val PARTY_CODE_CREATE = "party_code_create"
}
