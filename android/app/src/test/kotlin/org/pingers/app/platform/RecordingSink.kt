package org.pingers.app.platform

import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/** A [CoreSink] that records every call, for tests. */
class RecordingSink(private val reply: CoreNotification? = null) : CoreSink {
    val calls = LinkedBlockingQueue<Any>()

    data class Geofenced(val ids: List<String>, val transition: GeofenceTransition, val location: Fix?)
    data class Pushed(val platform: PushPlatform, val payload: String, val downgraded: Boolean)
    data class Woken(val reason: WakeReason)
    data class Focused(val focus: FocusState)
    data class Fixed(val fix: Fix?)
    data class Tokens(val tokens: PushTokens?)

    override fun onFix(fix: Fix?) {
        calls.add(Fixed(fix))
    }

    override fun onGeofence(regionIds: List<String>, transition: GeofenceTransition, location: Fix?) {
        calls.add(Geofenced(regionIds, transition, location))
    }

    override fun onPush(platform: PushPlatform, payload: ByteArray, downgraded: Boolean): CoreNotification? {
        calls.add(Pushed(platform, String(payload), downgraded))
        return reply
    }

    override fun onPushTokensChanged(tokens: PushTokens?) {
        calls.add(Tokens(tokens))
    }

    override fun onFocusChanged(focus: FocusState) {
        calls.add(Focused(focus))
    }

    override fun onWake(reason: WakeReason) {
        calls.add(Woken(reason))
    }

    /** The next call of type [T] within [seconds], skipping others; null on timeout. */
    inline fun <reified T> await(seconds: Long = 5): T? {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(seconds)
        while (true) {
            val left = deadline - System.nanoTime()
            if (left <= 0) return null
            val call = calls.poll(left, TimeUnit.NANOSECONDS) ?: return null
            if (call is T) return call
        }
    }
}
