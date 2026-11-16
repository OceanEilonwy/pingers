package org.pingers.app.platform

import android.app.ForegroundServiceStartNotAllowedException
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import androidx.core.content.edit

/**
 * The foreground service of type `location`, with its persistent notification. It runs while
 * the device is in a Meetup (design F.3: "Meetup, backgrounded") or while the user has turned
 * on "Keep Pingers running". It holds the process so continuous updates and the live stream
 * survive; it does no work of its own.
 */
class LocationService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val meetup = intent?.getBooleanExtra(EXTRA_MEETUP, false) ?: false
        try {
            ServiceCompat.startForeground(this, Notifications.RUNNING_ID, Notifications.running(this, meetup), ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION)
            running = true
        } catch (e: SecurityException) {
            // Location permission was revoked: a location service can't be foreground without it.
            Log.w(TAG, "foreground refused: ${e.forLog()}")
            stopSelf()
        } catch (e: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException (Android 12+) is an IllegalStateException.
            Log.w(TAG, "foreground refused: ${e.forLog()}")
            stopSelf()
        }
        // A stop asked for while this start was on its way: Android requires startForeground
        // first, so the stop happens here.
        val stop = synchronized(lock) {
            pendingStarts = (pendingStarts - 1).coerceAtLeast(0)
            stopWanted && pendingStarts == 0
        }
        if (stop) stopSelf()
        // Not restarted after a kill: the core sets the mode again at its next wake.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        super.onDestroy()
    }

    companion object {
        private const val TAG = "PingersService"
        private const val EXTRA_MEETUP = "meetup"

        private val lock = Any()
        private var pendingStarts = 0
        private var stopWanted = false

        /** Whether the service is in the foreground now (this process). */
        @Volatile
        var running: Boolean = false
            private set

        /**
         * Starts or updates the service. From the background Android 12+ may refuse (a Meetup
         * started by a normal-priority push, say); then this returns false and the core keeps
         * working on wakes alone.
         */
        fun start(context: Context, meetup: Boolean): Boolean = try {
            synchronized(lock) {
                ContextCompat.startForegroundService(context, Intent(context, LocationService::class.java).putExtra(EXTRA_MEETUP, meetup))
                pendingStarts++
                stopWanted = false
            }
            true
        } catch (e: IllegalStateException) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && e is ForegroundServiceStartNotAllowedException) {
                Log.w(TAG, "foreground service not allowed from the background")
            } else {
                Log.w(TAG, "service not started: ${e.forLog()}")
            }
            false
        } catch (e: SecurityException) {
            Log.w(TAG, "service not started: ${e.forLog()}")
            false
        }

        /**
         * Stops the service. A service started with startForegroundService must reach
         * startForeground before it stops, or Android kills the app; so while a start is still
         * on its way the stop is left to [onStartCommand].
         */
        fun stop(context: Context) {
            val now = synchronized(lock) {
                if (pendingStarts > 0) {
                    stopWanted = true
                    false
                } else {
                    true
                }
            }
            if (now) context.stopService(Intent(context, LocationService::class.java))
        }
    }
}

/**
 * Applies the location mode the core asks for, together with the user's "Keep Pingers
 * running" setting: the foreground service runs in the Meetup modes or when kept running, and
 * continuous updates run in the Meetup modes only.
 */
class LocationModes(
    private val context: Context,
    private val fixes: FixSource,
) {
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    @Volatile
    var mode: LocationMode = LocationMode.IDLE
        private set

    /** "Keep Pingers running" (Settings): a persistent notification that keeps the app awake outside Meetups. */
    var keepRunning: Boolean
        get() = prefs.getBoolean(KEY_KEEP_RUNNING, false)
        set(value) {
            prefs.edit { putBoolean(KEY_KEEP_RUNNING, value) }
            apply()
        }

    @Synchronized
    fun set(mode: LocationMode) {
        this.mode = mode
        apply()
    }

    @Synchronized
    private fun apply() {
        val meetup = mode == LocationMode.MEETUP_BACKGROUND || mode == LocationMode.MEETUP_FOREGROUND
        if (meetup || keepRunning) LocationService.start(context, meetup) else LocationService.stop(context)
        fixes.follow(mode)
    }

    companion object {
        const val PREFS = "pingers.platform"
        private const val KEY_KEEP_RUNNING = "keep-running"
    }
}
