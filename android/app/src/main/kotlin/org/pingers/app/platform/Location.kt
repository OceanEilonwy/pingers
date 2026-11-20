package org.pingers.app.platform

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationManager
import androidx.core.content.ContextCompat
import org.pingers.app.data.Coordinate

internal fun Location.toFix() = Fix(latitude, longitude, accuracy, time)

/** Whether the app may read location at all (fine, or coarse alone). */
internal fun hasLocationPermission(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

/**
 * Where the phone last was, as the platform's providers remember it: no new fix is taken and
 * nothing is sent. Null without permission or a remembered location.
 */
@SuppressLint("MissingPermission") // Checked first.
internal fun lastKnownCoordinate(context: Context): Coordinate? {
    if (!hasLocationPermission(context)) return null
    val manager = context.getSystemService(LocationManager::class.java) ?: return null
    return try {
        manager.getProviders(true).mapNotNull { manager.getLastKnownLocation(it) }
            .maxByOrNull { it.elapsedRealtimeNanos }
            ?.let { Coordinate(it.latitude, it.longitude) }
    } catch (_: SecurityException) {
        null
    } catch (_: IllegalArgumentException) {
        null
    }
}

/**
 * Where fixes come from: Fused Location in the `gms` flavour, the platform LocationManager in
 * `foss`. Every result goes to [CoreSink.onFix].
 */
interface FixSource {
    /** One fix within [timeoutMillis], or null on timeout or without permission. */
    fun request(quality: FixQuality, timeoutMillis: Long)

    /** Starts (or retunes) continuous updates for a Meetup mode; stops them for the others. */
    fun follow(mode: LocationMode)
}

/** What continuous updates a mode wants (design F.3), for either flavour's [FixSource]. */
data class ContinuousRequest(val highAccuracy: Boolean, val intervalMillis: Long, val minIntervalMillis: Long, val minDistanceMeters: Float) {
    companion object {
        /**
         * How old a cached fix may be to answer a request. The pipeline asks only when what it
         * has is too old (design E.2), and an entry is confirmed by a second fix taken at least
         * 30 s after the first: a cached fix up to a minute old could be the first one again,
         * and the entry would never be confirmed.
         */
        const val MAX_CACHED_AGE_MILLIS = 5_000L

        /**
         * Whether a cached fix taken at [fixElapsedNanos] (`Location.elapsedRealtimeNanos`) is
         * young enough at [nowElapsedNanos] (`SystemClock.elapsedRealtimeNanos()`). Measured on
         * the monotonic clock: a fix's wall-clock time comes from the provider and the phone's
         * clock can be set back, either of which could make an old fix look new. A fix from the
         * future (another boot) is not young.
         */
        fun isCachedFixYoung(fixElapsedNanos: Long, nowElapsedNanos: Long): Boolean {
            val age = nowElapsedNanos - fixElapsedNanos
            return age >= 0 && age < MAX_CACHED_AGE_MILLIS * 1_000_000L
        }

        /**
         * Backgrounded Meetup: send on 20 m movement or 60 s (the core drops to a 5-minute
         * keep-alive when stationary on its own). Compass open: GPS at 1 Hz.
         */
        fun of(mode: LocationMode): ContinuousRequest? = when (mode) {
            LocationMode.IDLE, LocationMode.DWELL -> null
            LocationMode.MEETUP_BACKGROUND -> ContinuousRequest(true, 60_000L, 10_000L, 20f)
            LocationMode.MEETUP_FOREGROUND -> ContinuousRequest(true, 1_000L, 1_000L, 0f)
        }
    }
}

/**
 * Registers OS geofences: the Geofencing API in `gms`, LocationManager proximity alerts in
 * `foss`. Each call replaces every region registered before. Transitions go to
 * [CoreSink.onGeofence].
 */
interface GeofenceRegistrar {
    fun replace(regions: List<OsRegion>, responsivenessMillis: Int)
}

/** One try at a fix: how hard, and for how long. */
data class FixAttempt(val quality: FixQuality, val timeoutMillis: Long)

/**
 * The tries one fix request makes, sharing its timeout. A balanced fix comes from Wi-Fi and
 * cell location, which a phone may not have at all (Google Location Accuracy off, network
 * location missing, an emulator): a balanced try that finds nothing in half the time gives the
 * rest to GPS, so a phone with only GPS is still placed, not left without a fix.
 */
fun fixAttempts(quality: FixQuality, timeoutMillis: Long): List<FixAttempt> {
    val total = timeoutMillis.coerceAtLeast(2)
    return when (quality) {
        FixQuality.HIGH -> listOf(FixAttempt(FixQuality.HIGH, total))
        FixQuality.BALANCED -> {
            val first = total / 2
            listOf(FixAttempt(FixQuality.BALANCED, first), FixAttempt(FixQuality.HIGH, total - first))
        }
    }
}

/**
 * Runs [attempts] in turn until one yields a fix, and answers [answer] exactly once: with the
 * first fix, or null once every try has ended. Each try is cut off at its timeout plus
 * [GRACE_MILLIS] even if the location service never calls back, so a request never waits
 * forever. [start] begins a try, reports its result, and returns what cancels it when it is cut off.
 */
class FixChain(
    private val attempts: List<FixAttempt>,
    private val schedule: (delayMillis: Long, action: () -> Unit) -> Unit,
    private val start: (attempt: FixAttempt, result: (Fix?) -> Unit) -> () -> Unit,
    private val answer: (Fix?) -> Unit,
) {
    fun run() = attempt(0)

    private fun attempt(index: Int) {
        val current = attempts.getOrNull(index) ?: return answer(null)
        val settled = java.util.concurrent.atomic.AtomicBoolean(false)
        val result = { fix: Fix? ->
            if (settled.compareAndSet(false, true)) {
                if (fix != null || index + 1 >= attempts.size) answer(fix) else attempt(index + 1)
            }
        }
        val cancel = start(current, result)
        schedule(current.timeoutMillis + GRACE_MILLIS) {
            if (!settled.get()) cancel()
            result(null)
        }
    }

    companion object {
        /** How long past its own timeout a try may take to call back. */
        const val GRACE_MILLIS = 1_000L
    }
}
