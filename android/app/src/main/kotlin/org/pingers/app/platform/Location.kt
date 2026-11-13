package org.pingers.app.platform

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.location.Location
import androidx.core.content.ContextCompat

internal fun Location.toFix() = Fix(latitude, longitude, accuracy, time)

/** Whether the app may read location at all (fine, or coarse alone). */
internal fun hasLocationPermission(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

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
        /** The pipeline asks for a fix only when it has nothing younger than 60 s (design E.2). */
        const val MAX_CACHED_AGE_MILLIS = 60_000L

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
