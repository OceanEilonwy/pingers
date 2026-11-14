package org.pingers.app.platform

import android.content.Context
import androidx.core.content.edit

/**
 * What this phone remembers for the Wake-ups screens, the notice at the top of Home and Party
 * settings' relay check (design canvas section 4). All of it stays on the phone: the last
 * wake-up push and whether it came at the priority it was sent with, the urgent pushes Android
 * held back this week, the notices the core raised (the latest time of each kind), and when the
 * privacy relay was last checked. Times are Unix seconds.
 *
 * The notices are kept by kind name, so this package needs no generated type; data/PhoneFacts.kt
 * turns them into the core's `RaisedNotice`.
 */
object PhoneLog {
    private const val PREFS = "pingers.phone-log"
    private const val KEY_LAST_WAKE = "wake.last"
    private const val KEY_LAST_WAKE_ON_TIME = "wake.last-on-time"
    private const val KEY_DELAYED = "wake.delayed"
    private const val KEY_RELAY_CHECKED = "relay.checked"
    private const val NOTICE_PREFIX = "notice."
    private const val WEEK_SECONDS = 7L * 24 * 60 * 60

    /** The last wake-up push: when it came, and whether at the priority it was sent with. */
    data class LastWake(val atSeconds: Long, val onTime: Boolean)

    /** A notice the core raised: its kind, what it was about (a Party and Spot), and when last. */
    data class Logged(val kind: String, val detail: String?, val atSeconds: Long)

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun seconds(nowMillis: Long) = nowMillis / 1000

    /**
     * A wake-up push arrived. [onTime] is false when FCM delivered it below the priority it was
     * sent with; such pushes are also counted for "delayed this week".
     */
    fun recordWake(context: Context, onTime: Boolean, nowMillis: Long = System.currentTimeMillis()) {
        val now = seconds(nowMillis)
        synchronized(this) {
            val prefs = prefs(context)
            val kept = recent(prefs.getString(KEY_DELAYED, null), now)
            val delayed = if (onTime) kept else kept + now
            prefs.edit {
                putLong(KEY_LAST_WAKE, now)
                putBoolean(KEY_LAST_WAKE_ON_TIME, onTime)
                putString(KEY_DELAYED, delayed.joinToString(","))
            }
        }
    }

    fun lastWake(context: Context): LastWake? {
        val prefs = prefs(context)
        if (!prefs.contains(KEY_LAST_WAKE)) return null
        return LastWake(prefs.getLong(KEY_LAST_WAKE, 0), prefs.getBoolean(KEY_LAST_WAKE_ON_TIME, true))
    }

    /** How many urgent wake-ups Android held back in the last seven days. */
    fun delayedThisWeek(context: Context, nowMillis: Long = System.currentTimeMillis()): Int =
        recent(prefs(context).getString(KEY_DELAYED, null), seconds(nowMillis)).size

    /** The timestamps in [stored] from the last seven days; older ones are dropped. */
    private fun recent(stored: String?, now: Long): List<Long> =
        stored.orEmpty().split(',').mapNotNull { it.toLongOrNull() }.filter { now - it < WEEK_SECONDS }

    /** The core raised a notice of [kind] (about [detail], if it names something). */
    fun recordNotice(context: Context, kind: String, detail: String?, nowMillis: Long = System.currentTimeMillis()) {
        prefs(context).edit { putString(NOTICE_PREFIX + kind, "${seconds(nowMillis)}|${detail.orEmpty()}") }
    }

    /** The notices raised so far, the latest of each kind. */
    fun notices(context: Context): List<Logged> = prefs(context).all.mapNotNull { (key, value) ->
        if (!key.startsWith(NOTICE_PREFIX) || value !is String) return@mapNotNull null
        val (at, detail) = value.split('|', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
        at.toLongOrNull()?.let { Logged(key.removePrefix(NOTICE_PREFIX), detail.ifEmpty { null }, it) }
    }

    /** When a notice of [kind] was last raised. */
    fun noticeAt(context: Context, kind: String): Long? = notices(context).firstOrNull { it.kind == kind }?.atSeconds

    /** The privacy relay check just answered. */
    fun recordRelayCheck(context: Context, nowMillis: Long = System.currentTimeMillis()) {
        prefs(context).edit { putLong(KEY_RELAY_CHECKED, seconds(nowMillis)) }
    }

    fun relayCheckedAt(context: Context): Long? =
        prefs(context).takeIf { it.contains(KEY_RELAY_CHECKED) }?.getLong(KEY_RELAY_CHECKED, 0)
}
