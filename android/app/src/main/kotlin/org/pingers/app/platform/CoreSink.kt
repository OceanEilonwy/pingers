package org.pingers.app.platform

import android.util.Log

/** A location fix, as the core's `on_location` entry point takes it. */
data class Fix(
    val latitude: Double,
    val longitude: Double,
    /** Radius of the 68 % confidence circle, in metres. */
    val accuracyMeters: Float,
    /** UTC time of the fix, in milliseconds since the epoch. */
    val timeMillis: Long,
)

/** An OS geofence transition (Geofencing API `GEOFENCE_TRANSITION_*`). */
enum class GeofenceTransition { ENTER, DWELL, EXIT }

/** Why the core is being woken. */
enum class WakeReason {
    /** The wake [Platform.scheduleWake] asked for. */
    SCHEDULED,

    /** The phone restarted; OS geofences were registered again. */
    BOOT,

    /** The app was updated; OS geofences were registered again. */
    APP_UPDATED,
}

/** What a notification is about: where tapping it leads. */
enum class NotificationKind { MEETUP_STARTED, JOIN_REQUEST, CHAT_MESSAGE, LOCATION_REQUEST }

/** A notification the core asks the shell to show after a wake (design B.7): the push arrived empty, the core wrote this. */
data class CoreNotification(
    /** Stable id: a later notification with the same id replaces this one (the next chat message of a Meetup). */
    val id: Int,
    val title: String,
    val body: String,
    val kind: NotificationKind,
)

/** The priority FCM sent a message with and the one it delivered it at (`RemoteMessage.getOriginalPriority()` and `getPriority()`). */
data class FcmPriority(val sentHigh: Boolean, val deliveredHigh: Boolean)

/**
 * The core's entry points the shell calls when the OS reports something (design E.2: "shell
 * calls core.on_*()"). data/CoreHost.kt implements it over pingers-ffi's `Core`; tests record
 * what reaches it.
 *
 * Calls come from receivers, services and workers on background threads; each may block for
 * as long as the core needs (a receiver's goAsync window, about ten seconds, or a worker's).
 */
interface CoreSink {
    /** A fix [Platform.requestFix] asked for, or a continuous update in a Meetup mode; null when none came in time. */
    fun onFix(fix: Fix?)

    /** OS geofences fired. [location] is the fix that triggered them, when Play services gives one. */
    fun onGeofence(regionIds: List<String>, transition: GeofenceTransition, location: Fix?)

    /**
     * A push arrived; [payload] is its body (`{"v":1,"h":handle}`). [priority]: for FCM, the priority it was sent and
     * delivered at, which the core counts (B.4.9). What the wake leads to is posted through the core's events.
     */
    fun onPush(platform: PushPlatform, payload: ByteArray, priority: FcmPriority?)

    /** The push registration changed (new endpoint or token, or none). */
    fun onPushTokensChanged(tokens: PushTokens?)

    /** The interruption filter changed (design B.10). The core also reads it fresh at every wake. */
    fun onFocusChanged(focus: FocusState)

    /** A background wake. */
    fun onWake(reason: WakeReason)
}

/**
 * Where receivers, services and workers find the core. The process may have been started by
 * the OS for one of them alone, so the app sets [sinkProvider] in `Application.onCreate`;
 * until then calls are logged and dropped.
 */
object PlatformHost {
    @Volatile
    var sinkProvider: () -> CoreSink = { LoggingSink }

    val sink: CoreSink get() = sinkProvider()
}

/** The sink before the core is wired up: logs each call and does nothing. */
object LoggingSink : CoreSink {
    private const val TAG = "PingersPlatform"

    override fun onFix(fix: Fix?) {
        Log.i(TAG, "onFix ${fix != null}")
    }

    override fun onGeofence(regionIds: List<String>, transition: GeofenceTransition, location: Fix?) {
        Log.i(TAG, "onGeofence $transition ${regionIds.size}")
    }

    override fun onPush(platform: PushPlatform, payload: ByteArray, priority: FcmPriority?) {
        Log.i(TAG, "onPush $platform ${payload.size} bytes priority=$priority")
    }

    override fun onPushTokensChanged(tokens: PushTokens?) {
        Log.i(TAG, "onPushTokensChanged $tokens")
    }

    override fun onFocusChanged(focus: FocusState) {
        Log.i(TAG, "onFocusChanged $focus")
    }

    override fun onWake(reason: WakeReason) {
        Log.i(TAG, "onWake $reason")
    }
}
