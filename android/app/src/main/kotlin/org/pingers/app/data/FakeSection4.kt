package org.pingers.app.data

import uniffi.pingers.AdvancedText
import uniffi.pingers.DeviceSettingsView
import uniffi.pingers.HomeWakeupsView
import uniffi.pingers.LeavePartyView
import uniffi.pingers.NewPartyText
import uniffi.pingers.NoticeLink
import uniffi.pingers.NoticeSeverity
import uniffi.pingers.NoticeView
import uniffi.pingers.PartyChip
import uniffi.pingers.PartyCodeView
import uniffi.pingers.PartySettingsView
import uniffi.pingers.PrivacyText
import uniffi.pingers.SafetyCodesView
import uniffi.pingers.SafetyRowView
import uniffi.pingers.ServiceRowView
import uniffi.pingers.TimeOfDay
import uniffi.pingers.TitledRow
import uniffi.pingers.VisibilityChoice
import uniffi.pingers.WakeupsKind
import uniffi.pingers.WakeupsView
import uniffi.pingers.YourNameView
import uniffi.pingers.ZoneDays
import uniffi.pingers.ZoneTimes
import uniffi.pingers.ZoneTimesEditorView

/**
 * The canvas's section-4 example (Ben's phone, design canvas "4 · Settings and setup screens"),
 * as the pingers-ffi records the real app gets, with every word copied from the boards. Which
 * state each screen shows comes from the debug launch extra `--es variant ...`:
 *
 * - `shared`: Party settings in reduced privacy (PartySettingsShared).
 * - `refused`: "This server needs a code" after the code was refused (PartyCodeRefused).
 * - `error`: Quiet hours with equal times (QuietHoursError).
 * - `off`, `nothing`, `delayed`: Wake-ups off by choice, nothing can wake Pingers, Android
 *   holds back wake-ups; Home shows the grey line or the orange banner to match.
 * - `notice=clock|relay|hiding|dnd|knock|busy`: the notice at the top of Home.
 * - `noname`: Open invite before a name is set, with the name field.
 */
class FakeSection4(private val variants: Set<String>) {
    private fun has(name: String) = name in variants

    /** The Wake-ups state the variants pick. */
    val wakeKind: WakeupsKind = when {
        has("off") -> WakeupsKind.OFF
        has("nothing") -> WakeupsKind.NOTHING_CAN_WAKE
        has("delayed") -> WakeupsKind.DELAYED
        else -> WakeupsKind.WORKING
    }

    private fun wakeupsSummary(kind: WakeupsKind) = when (kind) {
        WakeupsKind.OFF -> "Off"
        WakeupsKind.WORKING -> "On · through ntfy"
        WakeupsKind.NOTHING_CAN_WAKE -> "On · nothing can wake Pingers"
        WakeupsKind.DELAYED -> "On · through Google"
    }

    fun deviceSettings(kind: WakeupsKind = wakeKind) = DeviceSettingsView(
        title = "Settings",
        subtitle = "This phone, for every Party",
        youAvatar = "BE",
        youTitle = "Ben",
        youSubtitle = "Your name in every Party",
        whenHeading = "When not to share",
        quietHours = "Quiet hours",
        quietHoursSummary = "23:00–07:00 every day",
        zones = "Exclusion zones",
        zonesSummary = "Home · Work, weekdays 08:00–18:00",
        phoneHeading = "This phone",
        wakeups = "Wake-ups",
        wakeupsSummary = wakeupsSummary(kind),
        maps = "Downloaded maps",
        mapsSummary = "2 areas · 246 MB",
        securityLink = "How Pingers keeps you private",
        footnote = "Each Party's server, relays, invites and safety codes are in that Party's settings.",
    )

    val yourName = YourNameView(
        name = "Ben",
        title = "Your name",
        back = "Back to Settings",
        avatar = "BE",
        appearsAs = "How you appear to members",
        label = "Name",
        placeholder = "First name or nickname",
        help = "Up to 80 characters. Members see it, encrypted end to end. Your server never does.",
        partiesHeading = "Changes in every Party you're in",
        parties = listOf(
            PartyChip("CC", "Climbing crew", uniffi.pingers.PartyColour.BLUE),
            PartyChip("UF", "Uni flat", uniffi.pingers.PartyColour.ORANGE),
            PartyChip("5S", "Saturday 5-a-side", uniffi.pingers.PartyColour.INK),
        ),
        note = "Members see the new name the next time their phone checks in. Until then they see the name you had before.",
        save = "Save",
    )

    /** Quiet hours as the canvas shows them: 23:00 to 07:00, or both at 22:00 for `error`. */
    val quietFrom: TimeOfDay = if (has("error")) time(22, 0) else time(23, 0)
    val quietTo: TimeOfDay = if (has("error")) time(22, 0) else time(7, 0)

    fun zoneTimes(times: ZoneTimes = ZoneTimes.Window(ZoneDays.WEEKDAYS, time(8, 0), time(18, 0))): ZoneTimesEditorView {
        val window = times as? ZoneTimes.Window
        return ZoneTimesEditorView(
            zone = uniffi.pingers.ZoneId(2u),
            times = times,
            title = "Work",
            subtitle = "Exclusion zone · drawn area · all parties",
            whenHeading = "When it applies",
            whenLabel = "When this zone applies",
            always = "All the time",
            certainTimes = "Only at certain times",
            isAlways = window == null,
            daysLabel = "Days",
            days = listOf("Every day", "Weekdays", "Weekends"),
            from = "From",
            fromTime = window?.let { clock(it.from) },
            fromAction = window?.let { "From ${clock(it.from)}. Change" },
            until = "Until",
            untilTime = window?.let { clock(it.to) },
            untilAction = window?.let { "Until ${clock(it.to)}. Change" },
            summaryLead = "Monday to Friday, 08:00–18:00",
            summaryRest = ", Pingers sends nothing from here. At other times this place is like anywhere else: " +
                "if it's inside a Spot, your Party can notice you.",
            error = null,
            footnote = "The zone and its times stay on this phone. Uses this phone's time zone.",
            save = "Save",
            canSave = true,
            delete = "Delete zone",
        )
    }

    fun wakeups(kind: WakeupsKind = wakeKind): WakeupsView {
        val delivery = kind == WakeupsKind.OFF || kind == WakeupsKind.WORKING
        return WakeupsView(
            kind = kind,
            title = "Wake-ups",
            intro = if (delivery) {
                "When a friend arrives at a Spot you're in, their phone asks for yours to be woken, so the Meetup starts in " +
                    "seconds. These settings are for this phone and every Party on it."
            } else {
                null
            },
            statusTitle = when (kind) {
                WakeupsKind.WORKING -> "Working"
                WakeupsKind.OFF -> "Off"
                else -> null
            },
            statusDetail = when (kind) {
                WakeupsKind.WORKING -> "Last wake-up arrived on time, yesterday 18:42"
                WakeupsKind.OFF -> "Meetups can take up to an hour to start."
                else -> null
            },
            faultTitle = when (kind) {
                WakeupsKind.NOTHING_CAN_WAKE -> "Nothing can wake Pingers"
                WakeupsKind.DELAYED -> "Android is holding back wake-ups"
                else -> null
            },
            faultBody = when (kind) {
                WakeupsKind.NOTHING_CAN_WAKE ->
                    "This phone has no push app and no Google push service. Pingers only notices friends when it wakes by " +
                        "itself, which can take up to an hour, and a short visit can be missed."
                WakeupsKind.DELAYED ->
                    "This week Android delayed 6 urgent wake-ups until the phone was next awake anyway. Meetups can start " +
                        "late, or be missed if a visit is short."
                else -> null
            },
            fixHeading = "How to fix it".takeIf { kind == WakeupsKind.NOTHING_CAN_WAKE },
            installBefore = "Install a push app, such as ".takeIf { kind == WakeupsKind.NOTHING_CAN_WAKE },
            installApp = "ntfy".takeIf { kind == WakeupsKind.NOTHING_CAN_WAKE },
            installAfter = ". It learns that a push arrived, never what it says.".takeIf { kind == WakeupsKind.NOTHING_CAN_WAKE },
            installAction = "Get ntfy".takeIf { kind == WakeupsKind.NOTHING_CAN_WAKE },
            comeBack = "Come back here. Pingers picks it up, or asks which one to use if you have more than one."
                .takeIf { kind == WakeupsKind.NOTHING_CAN_WAKE },
            tryHeading = "Try these".takeIf { kind == WakeupsKind.DELAYED },
            battery = TitledRow("Battery use: Unrestricted", "Now “Optimised”. Android limits apps it thinks you don't use.")
                .takeIf { kind == WakeupsKind.DELAYED },
            batteryAction = "Change".takeIf { kind == WakeupsKind.DELAYED },
            notifications = TitledRow("Notifications", "Allowed").takeIf { kind == WakeupsKind.DELAYED },
            notificationsAllowed = true,
            settingsHeading = if (delivery) "Delivery" else "On this phone",
            noWakePushes = when (kind) {
                WakeupsKind.NOTHING_CAN_WAKE ->
                    TitledRow("No wake-up pushes", "Choose this if you'd rather nobody, not even a push app, sees when you're woken.")
                WakeupsKind.DELAYED ->
                    TitledRow("No wake-up pushes", "Nobody, not even Google, sees when you're woken. Meetups can take up to an hour to start.")
                else -> TitledRow(
                    "No wake-up pushes",
                    "Nobody, not even Apple or Google, sees when you're woken. Meetups can take up to an hour to start.",
                )
            },
            noWakePushesOn = kind == WakeupsKind.OFF,
            pushService = TitledRow(
                "Push service",
                when (kind) {
                    WakeupsKind.NOTHING_CAN_WAKE -> "None installed"
                    WakeupsKind.DELAYED -> "Google (Firebase). Install a push app such as ntfy to use it instead."
                    else -> "ntfy"
                },
            ),
            pushServiceChoosable = kind == WakeupsKind.WORKING,
            keepRunning = TitledRow(
                "Keep Pingers running",
                if (delivery) {
                    "Shows a lasting notification so Android doesn't put Pingers to sleep. Friends nearby are noticed on time."
                } else {
                    "Shows a lasting notification so Android doesn't put Pingers to sleep. Uses more battery."
                },
            ),
            keepRunningOn = false,
            footnote = if (delivery) {
                "The push app learns that a push arrived, never what it says. Each Party's push relay sees when your phone " +
                    "is woken, never why or by whom; Party settings shows who runs it."
            } else {
                null
            },
        )
    }

    fun homeWakeups(kind: WakeupsKind = wakeKind): HomeWakeupsView = when (kind) {
        WakeupsKind.OFF -> HomeWakeupsView.OffByChoice("Wake-ups off · Meetups can take up to an hour to start")
        WakeupsKind.NOTHING_CAN_WAKE -> HomeWakeupsView.Fault(
            kind, "Friends nearby may be noticed late", "Nothing can wake Pingers on this phone yet.", "Fix",
        )
        WakeupsKind.DELAYED -> HomeWakeupsView.Fault(kind, "Friends nearby may be noticed late", "Android is holding back wake-ups.", "Fix")
        WakeupsKind.WORKING -> HomeWakeupsView.Nothing
    }

    /** The notice `notice=<kind>` picks (canvas Notices), or none. */
    val notice: NoticeView? = when (variants.firstOrNull { it.startsWith("notice=") }?.removePrefix("notice=")) {
        "clock" -> NoticeView(
            NoticeSeverity.STOPPED, "Your phone's clock is wrong",
            "Pingers has stopped checking in until the time is right. Turn on automatic date and time.",
            "Open date and time settings", NoticeLink.DateTime,
        )
        "relay" -> NoticeView(
            NoticeSeverity.STOPPED, "Climbing crew's privacy relay key changed",
            "The privacy relay's key doesn't match the one your party agreed on. Pingers has stopped anonymous traffic until it does.",
            "Party settings", NoticeLink.Party(uniffi.pingers.PartyId("cc")),
        )
        "hiding" -> NoticeView(
            NoticeSeverity.WARNING, "Your server may be hiding people",
            "Phones in your last Meetup were shown different check-ins. That can stop friends finding each other. " +
                "It doesn't tell the server where you are.",
            null, null,
        )
        "dnd" -> NoticeView(
            NoticeSeverity.WARNING, "Pingers can't see Do Not Disturb",
            "“Pause during Focus” won't work on this phone. Turn off Location sharing yourself when you want a break.",
            "Privacy settings", NoticeLink.Privacy,
        )
        "knock" -> NoticeView(
            NoticeSeverity.INFO, "Someone from Climbing crew seemed to be here",
            "At The Wall climbing gym, but their phone didn't answer. It may be asleep, or they've left.",
            null, null,
        )
        "busy" -> NoticeView(
            NoticeSeverity.SLOW, "Your server is very busy",
            "Checking for friends is taking longer than usual, so some Meetups may start late.",
            null, null,
        )
        else -> null
    }

    /** Home's "Waiting to be let in" row (canvas HomePendingJoin), for `pending`. */
    val pendingJoin: uniffi.pingers.PendingJoinView? = if (has("pending")) {
        uniffi.pingers.PendingJoinView(
            title = "Waiting to be let in",
            detail = "Asked to join a Party · Code ",
            codeStart = "4821",
            more = " …",
            label = "Waiting to be let in. Asked to join a Party. Code starts 4821.",
        )
    } else {
        null
    }

    fun privacyText(kind: WakeupsKind = wakeKind) = PrivacyText(
        title = "Privacy",
        paused = "Paused · Sleep Focus is on",
        pausedDetail = "Nobody can tell you've paused. Resumes when Focus ends.",
        whenHeading = "When to share",
        sharing = "Location sharing",
        pauseInFocus = "Pause during Focus",
        pauseInFocusHelp = "Do Not Disturb, Sleep, any Focus",
        whoHeading = "Who sees my exact spot in meetups",
        visibility = listOf(
            VisibilityChoice(uniffi.pingers.Visibility.ASK_EACH_TIME, "Ask me each time", "Others see you're here, not where"),
            VisibilityChoice(uniffi.pingers.Visibility.EVERYONE, "Everyone in the meetup", null),
            VisibilityChoice(uniffi.pingers.Visibility.NOBODY, "Nobody", null),
        ),
        deliveryHeading = "Delivery",
        wakeups = "Wake-ups",
        wakeupsSummary = wakeupsSummary(kind),
        securityLink = "How Pingers keeps you private",
    )

    /** Party settings for Climbing crew, in reduced privacy for `shared`. */
    val partySettings: PartySettingsView = if (has("shared")) {
        PartySettingsView(
            party = uniffi.pingers.PartyId("cc"),
            privacy = uniffi.pingers.PrivacyLevel.REDUCED,
            title = "Party settings",
            subtitle = "Climbing crew · only this Party",
            back = "Back to Climbing crew",
            invite = "Invite people",
            members = "6 members",
            operatorsHeading = "Who runs this Party's server",
            privacyTitle = "Reduced privacy",
            privacyDetail = "The server's operator also runs the push relay, so it could see which phone each alert wakes.",
            consequenceBefore = "For this Party, Flat 7 could work out ",
            consequenceBold = "who met whom",
            consequenceAfter = ". It still can't see where anyone is, which Spot, or what's said. Turning off wake-ups stops this, " +
                "for every Party on this phone.",
            wakeupsLink = "Wake-ups settings",
            server = ServiceRowView("Server", "pingers.flat7.org", "Flat 7", shared = true),
            relay = ServiceRowView("Privacy relay", "relay.quietpath.net · hides your internet address", "Quietpath", shared = false),
            relayCheck = "Your server answered through it · 2 min ago",
            relayCheckOk = true,
            pushRelay = ServiceRowView("Push relay", "push.flat7.org · same operator as the server", "Flat 7", shared = true),
            checkAgain = "Check the privacy relay again",
            operatorsNote = OPERATORS_NOTE,
            serverSees = "What the server sees",
            safetyCodes = "Safety codes",
            safetySummary = "Checked with 2 of 5 members",
            security = "Security details",
            securitySummary = "End-to-end encrypted · post-quantum off",
            leave = "Leave party",
        )
    } else {
        PartySettingsView(
            party = uniffi.pingers.PartyId("cc"),
            privacy = uniffi.pingers.PrivacyLevel.FULL,
            title = "Party settings",
            subtitle = "Climbing crew · only this Party",
            back = "Back to Climbing crew",
            invite = "Invite people",
            members = "6 members",
            operatorsHeading = "Who runs this Party's server",
            privacyTitle = "Full privacy",
            privacyDetail = "Server, privacy relay and push relay are run by three different operators.",
            consequenceBefore = null,
            consequenceBold = null,
            consequenceAfter = null,
            wakeupsLink = null,
            server = ServiceRowView("Server", "pingers.flat7.org", "Flat 7", shared = false),
            relay = ServiceRowView("Privacy relay", "relay.quietpath.net · hides your internet address", "Quietpath", shared = false),
            relayCheck = "Your server answered through it · 2 min ago",
            relayCheckOk = true,
            pushRelay = ServiceRowView("Push relay", "push.pingers.org · sees when, never why", "Pingers", shared = false),
            checkAgain = "Check the privacy relay again",
            operatorsNote = OPERATORS_NOTE,
            serverSees = "What the server sees",
            safetyCodes = "Safety codes",
            safetySummary = "Checked with 2 of 5 members",
            security = "Security details",
            securitySummary = "End-to-end encrypted · post-quantum off",
            leave = "Leave party",
        )
    }

    private data class Member(val leaf: UInt, val avatar: String, val tint: uniffi.pingers.Tint, val name: String, val her: String)

    private val members = listOf(
        Member(1u, "AN", uniffi.pingers.Tint.PEACH, "Ana", "her"),
        Member(2u, "BE", uniffi.pingers.Tint.BUTTER, "Ben", "him"),
        Member(3u, "CH", uniffi.pingers.Tint.SKY, "Chloé", "her"),
        Member(4u, "DE", uniffi.pingers.Tint.LILAC, "Dev", "them"),
        Member(5u, "EL", uniffi.pingers.Tint.MINT, "Eli", "them"),
    )

    /** Ana's code, as on the canvas; the others' are made up from it. */
    private val anasCode = listOf("48210", "93755", "12064", "38817", "70459", "21386", "66102", "94573", "15028", "83941", "57260", "30694")

    /** Safety codes showing [shown]'s code; Ben and Chloé are checked, and whoever [checked] adds. */
    fun safetyCodes(shown: UInt = 1u, checked: Set<UInt> = setOf(2u, 3u)): SafetyCodesView {
        val who = members.first { it.leaf == shown }
        val isChecked = shown in checked
        return SafetyCodesView(
            shown = uniffi.pingers.MemberId(shown),
            title = "Safety codes",
            subtitle = "Climbing crew",
            codeHeading = "Your code with ${who.name}",
            codeGroups = anasCode.map { group -> group.map { d -> '0' + (d - '0' + shown.toInt() - 1) % 10 }.joinToString("") },
            compare = "Compare with ${who.name} in person or on a call. If every digit matches, nobody is passing messages " +
                "between you while pretending to be ${who.her}.",
            mark = "Codes match · Mark ${who.name} as checked".takeUnless { isChecked },
            checkedNote = "${who.name} is checked".takeIf { isChecked },
            membersHeading = "Members",
            members = members.map {
                SafetyRowView(
                    member = uniffi.pingers.MemberId(it.leaf),
                    avatar = it.avatar,
                    tint = it.tint,
                    name = it.name,
                    state = if (it.leaf in checked) "Checked" else "Not checked",
                    checked = it.leaf in checked,
                    selected = it.leaf == shown,
                )
            },
            footnote = "Checking is optional. A code changes only if that member sets up Pingers again from scratch.",
        )
    }

    val leaveParty = LeavePartyView(
        party = uniffi.pingers.PartyId("cc"),
        title = "Leave Climbing crew?",
        points = listOf(
            "Members stop noticing you at its 3 Spots, and you stop noticing them.",
            "Members see that you left. They get a new key, so you can't read anything sent after you go.",
            "To come back, someone in the Party has to invite you again.",
        ),
        meetupWarning = "You're in a Meetup at Riverside Park with this Party. Leaving ends it for you too.",
        leave = "Leave party",
        cancel = "Cancel",
    )

    /** The code typed, as on the canvas. */
    val typedCode: String = if (has("refused")) "spring-otter-4417" else "spring-otter-4471"

    fun partyCode(refused: Boolean = has("refused")) = PartyCodeView(
        back = "Back to New party",
        title = "This server needs a code",
        body = "You chose pingers.flat7.org for Climbing crew. It only lets people with a code from whoever runs it create Parties there.",
        server = "pingers.flat7.org",
        operator = "Run by Flat 7",
        codeLabel = "Registration code",
        codeHelp = "This server sees the code once, when it sets up your account there. The account is a random key made on " +
            "this phone: no name, phone number or email.",
        refused = "The server didn't accept this code. Check it for typing mistakes, or ask Flat 7 for a new one.".takeIf { refused },
        nothingCreated = "No Party was created. You can try again, or go back and pick another server.".takeIf { refused },
        invitees = "People you invite won't need a code: their invite link carries it.",
        create = if (refused) "Try again" else "Create party",
        canCreate = true,
    )

    fun newPartyText(privacy: PrivacyLevel?) = NewPartyText(
        title = "New party",
        nameLabel = "Party name",
        colourLabel = "Colour",
        colourNames = listOf("Blue", "Orange", "Ink", "Teal", "Plum"),
        serverLabel = "Server",
        builtIn = "The app's server",
        builtInDetail = "$BUILT_IN · built in",
        another = "Another server",
        anotherDetail = "Enter its address, like pingers.example.org",
        anotherPlaceholder = "pingers.example.org",
        advanced = "Advanced",
        advancedDetail = "Relays and encryption · ${privacyWord(privacy)}",
        later = "You'll invite people and add Spots once the party is created.",
        create = "Create party",
    )

    fun advancedText(server: Reachability, relay: Reachability) = AdvancedText(
        title = "Advanced",
        privacy = privacyWord(PrivacyLevel.Full),
        privacyDetail = "Server, privacy relay and push relay are run by three different operators.",
        serverLabel = "Server",
        serverChoiceLabel = "Which server",
        serverChoices = listOf("The app's server", "Another server"),
        serverCheck = checkWord("Server", server),
        serverHelp = "The server that hosts this party. Everyone who joins uses it, and it can't be changed later.",
        relayLabel = "Privacy relay",
        relayCheck = checkWord("Privacy relay", relay),
        relayHelp = "Hides your IP address from the server. Suggested by the server; each member can choose their own.",
        pushLabel = "Push relay",
        pushOfficial = "Pingers (official app)",
        pushCustom = "Custom URL (UnifiedPush or own build)",
        pushHelp = "Sees when a phone is woken, never why or by whom.",
        postQuantum = "Post-quantum encryption",
        postQuantumHelp = "Protects against future quantum computers. Larger messages; every member needs a recent app.",
        done = "Done",
    )

    private fun privacyWord(privacy: PrivacyLevel?) = when (privacy) {
        PrivacyLevel.Full -> "Full privacy"
        PrivacyLevel.Reduced -> "Reduced privacy"
        null -> "Checking"
    }

    private fun checkWord(what: String, r: Reachability) = when (r) {
        Reachability.Checking -> "Checking ${what.lowercase()}"
        Reachability.Reachable -> "$what reachable"
        Reachability.Unreachable -> "$what not reachable"
    }

    companion object {
        /** The app's own server, as the canvas names it. */
        const val BUILT_IN = "server.pingers.org"

        /** Party detail's bottom row, under the server. */
        const val SETTINGS_DETAIL = "Party settings: server, invites, leave"

        private const val OPERATORS_NOTE =
            "Each service publishes who runs it. Pingers shows what they say; it can't prove they aren't secretly the same people."

        fun time(hour: Int, minute: Int) = TimeOfDay(hour.toUByte(), minute.toUByte())

        fun clock(t: TimeOfDay) = "%02d:%02d".format(t.hour.toInt(), t.minute.toInt())
    }
}
