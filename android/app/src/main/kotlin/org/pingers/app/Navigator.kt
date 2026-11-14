package org.pingers.app

import androidx.compose.runtime.mutableStateListOf

/** Every screen of the design canvas. */
enum class Route(val dark: Boolean = false) {
    Home,
    NewParty,
    Advanced,
    Invite,
    /** "Ben asked to join", from an opened invite on Invite people. */
    JoinRequest,
    /** Join flow A on the joiner's phone: a link opened, then the code, then You're in. */
    OpenInvite,
    AskedToJoin,
    LetIn,
    Party,
    SpotEditor,
    DrawArea,
    ExclusionZones,
    Privacy,
    ServerView,
    Security,
    Alert(dark = true),
    Meetup,
    Chat,
    MeetPoint,
    Compass(dark = true),
    LocationRequest,
    MeetupEnded,
    /** Downloaded maps and the areas on offer (the region picker, canvas DownloadedMaps). */
    MapRegions,

    // Section 4 of the canvas.
    /** First run (SetupWelcome, SetupBackground and the notifications step). */
    Setup,
    /** Settings for this phone, the Home cog. */
    Settings,
    YourName,
    QuietHours,
    /** When an exclusion zone applies, from a zone row in Exclusion zones. */
    ZoneTimes,
    /** Wake-ups: WakeupsOn, Wakeups or WakeupsDelayed, as the core's view says. */
    Wakeups,
    PartySettings,
    SafetyCodes,
    /** The Leave party sheet over Party settings. */
    LeaveParty,
    /** "This server needs a code" (PartyCode, PartyCodeRefused). */
    PartyCode,
}

/**
 * A plain back stack, as in monokulo-fob: no navigation library. [go] returns to a
 * screen already on the stack (the canvas's "Done" and "Back" links) or pushes it.
 */
class Navigator(start: Route = Route.Home) {
    val stack = mutableStateListOf(start)

    val current: Route get() = stack.last()

    fun go(route: Route) {
        val i = stack.lastIndexOf(route)
        if (i >= 0) {
            while (stack.size > i + 1) stack.removeAt(stack.lastIndex)
        } else {
            stack.add(route)
        }
    }

    /** Replaces the current screen, so Back skips it (Create party then Invite). */
    fun replace(route: Route) {
        stack[stack.lastIndex] = route
        val i = stack.indexOf(route)
        if (i < stack.lastIndex) go(route)
    }

    /** Clears the stack down to [route], as a tab or a notification does. */
    fun reset(route: Route) {
        stack.clear()
        if (route != Route.Home) stack.add(Route.Home)
        stack.add(route)
    }

    fun back(): Boolean {
        if (stack.size <= 1) return false
        stack.removeAt(stack.lastIndex)
        return true
    }
}
