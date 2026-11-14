package org.pingers.app.data

import android.content.Context
import android.os.PowerManager
import androidx.core.app.NotificationManagerCompat
import org.pingers.app.maps.Maps
import org.pingers.app.platform.AndroidPlatform
import org.pingers.app.platform.PhoneLog
import org.pingers.app.platform.Push
import uniffi.pingers.BatteryUse
import uniffi.pingers.DevicePlatform
import uniffi.pingers.Notice
import uniffi.pingers.PushServiceInfo
import uniffi.pingers.RaisedNotice
import uniffi.pingers.WakePlatform

/**
 * The phone's own facts, as the records the core's Settings, Wake-ups and Home views take
 * (pingers-ffi device_views.rs): which push service can wake Pingers, "Keep Pingers running",
 * battery optimisation, whether notifications are allowed, the maps downloaded, and what
 * platform/PhoneLog.kt remembers (the last wake-up, delays this week, the notices raised).
 * Read again whenever the app comes back to the front: any of them may change in Android's
 * settings meanwhile.
 */
object PhoneFacts {
    /** What can wake Pingers: the push app registered, a choice to make, Google's service, or nothing. */
    fun pushService(context: Context): PushServiceInfo {
        val distributors = Push.distributors(context)
        val chosen = Push.distributor(context)
        return when {
            chosen != null -> PushServiceInfo.PushApp(Push.label(context, chosen))
            distributors.isNotEmpty() -> PushServiceInfo.NotChosen
            AndroidPlatform.get(context).fcmAvailable -> PushServiceInfo.Google
            else -> PushServiceInfo.Nothing
        }
    }

    /** What the Settings hub needs: the push service and the maps on this phone. */
    fun device(context: Context): DevicePlatform {
        val installed = Maps.get(context).installed.value
        return DevicePlatform(
            pushService = pushService(context),
            mapAreas = installed.size.toUInt(),
            mapBytes = installed.sumOf { it.pack.size + it.index.size }.coerceAtLeast(0).toULong(),
        )
    }

    /** What the Wake-ups screens and Home need. */
    fun wake(context: Context, nowMillis: Long = System.currentTimeMillis()): WakePlatform {
        val power = context.getSystemService(PowerManager::class.java)
        val unrestricted = power?.isIgnoringBatteryOptimizations(context.packageName) == true
        val last = PhoneLog.lastWake(context)
        return WakePlatform(
            pushService = pushService(context),
            keepRunning = AndroidPlatform.get(context).modes.keepRunning,
            battery = if (unrestricted) BatteryUse.UNRESTRICTED else BatteryUse.OPTIMISED,
            notificationsAllowed = NotificationManagerCompat.from(context).areNotificationsEnabled(),
            lastWakeAt = last?.atSeconds?.toULong(),
            lastWakeOnTime = last?.onTime ?: true,
            delayedThisWeek = PhoneLog.delayedThisWeek(context, nowMillis).toUInt(),
            downgradedAt = PhoneLog.noticeAt(context, kindOf(Notice.PushDowngraded))?.toULong(),
        )
    }

    /** The notices the core raised, the latest time of each kind, for `Snapshot.topNotice`. */
    fun raised(context: Context): List<RaisedNotice> = PhoneLog.notices(context).mapNotNull { logged ->
        notice(logged.kind, logged.detail)?.let { RaisedNotice(it, logged.atSeconds.toULong()) }
    }

    /** Keeps a notice the core raised (Events.notice), with the time. */
    fun record(context: Context, notice: Notice) {
        val detail = (notice as? Notice.UnansweredKnock)?.let { "${it.party.hex}/${it.spot.hex}" }
        PhoneLog.recordNotice(context, kindOf(notice), detail)
    }

    /** A notice's kind, as PhoneLog keeps it. */
    private fun kindOf(notice: Notice): String = notice::class.simpleName ?: "Other"

    private fun notice(kind: String, detail: String?): Notice? = when (kind) {
        kindOf(Notice.ClockSkew) -> Notice.ClockSkew
        kindOf(Notice.RelayKeyMismatch) -> Notice.RelayKeyMismatch
        kindOf(Notice.BoardEquivocation) -> Notice.BoardEquivocation
        kindOf(Notice.DndUnavailable) -> Notice.DndUnavailable
        kindOf(Notice.BoardTooLarge) -> Notice.BoardTooLarge
        kindOf(Notice.PushDowngraded) -> Notice.PushDowngraded
        kindOf(Notice.PrivacyPassOnly) -> Notice.PrivacyPassOnly
        kindOf(Notice.SameOperator) -> Notice.SameOperator
        "UnansweredKnock" -> detail?.split('/')?.takeIf { it.size == 2 }?.let {
            Notice.UnansweredKnock(uniffi.pingers.PartyId(it[0]), uniffi.pingers.SpotId(it[1]))
        }
        else -> null
    }
}
