package org.pingers.app.platform

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.location.Location
import android.os.HandlerThread
import android.os.Looper
import androidx.core.content.ContextCompat
import com.google.android.gms.location.CurrentLocationRequest
import com.google.android.gms.location.FusedLocationProviderClient
import com.google.android.gms.location.LocationCallback
import com.google.android.gms.location.LocationRequest
import com.google.android.gms.location.LocationResult
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.Priority
import com.google.android.gms.tasks.CancellationTokenSource
import java.util.concurrent.Executors

internal fun Location.toFix() = Fix(latitude, longitude, accuracy, time)

/** Whether the app may read location at all (fine, or coarse alone). */
internal fun hasLocationPermission(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

/**
 * Fused Location: single fixes on request, and the continuous updates of the Meetup modes
 * (design F.3). Continuous updates run only while the location foreground service holds the
 * process: [LocationModes] starts both together.
 */
class FusedFixes(private val context: Context, private val sink: () -> CoreSink) {
    private val client: FusedLocationProviderClient = LocationServices.getFusedLocationProviderClient(context)
    private val thread = HandlerThread("pingers-location").apply { start() }
    private val looper: Looper get() = thread.looper
    private val executor = Executors.newSingleThreadExecutor()
    private var continuous: LocationCallback? = null

    /** One fix within [timeoutMillis]; [CoreSink.onFix] gets it, or null on timeout or without permission. */
    @SuppressLint("MissingPermission") // Checked first.
    fun request(quality: FixQuality, timeoutMillis: Long) {
        if (!hasLocationPermission(context)) {
            sink().onFix(null)
            return
        }
        val request = CurrentLocationRequest.Builder()
            .setPriority(quality.priority())
            .setDurationMillis(timeoutMillis.coerceAtLeast(1))
            // The pipeline asks only when it has nothing younger than 60 s (design E.2).
            .setMaxUpdateAgeMillis(if (quality == FixQuality.HIGH) 0 else MAX_CACHED_AGE_MILLIS)
            .build()
        val cancel = CancellationTokenSource()
        client.getCurrentLocation(request, cancel.token)
            // Off the main thread: the core may take a while with a fix.
            .addOnSuccessListener(executor) { location -> sink().onFix(location?.toFix()) }
            .addOnFailureListener(executor) { sink().onFix(null) }
    }

    /** Starts (or retunes) continuous updates for a Meetup mode; stops them for the others. */
    @SuppressLint("MissingPermission") // Checked first.
    @Synchronized
    fun follow(mode: LocationMode) {
        continuous?.let { client.removeLocationUpdates(it) }
        continuous = null
        val request = mode.continuousRequest() ?: return
        if (!hasLocationPermission(context)) return
        val callback = object : LocationCallback() {
            override fun onLocationResult(result: LocationResult) {
                result.lastLocation?.let { sink().onFix(it.toFix()) }
            }
        }
        client.requestLocationUpdates(request, callback, looper)
        continuous = callback
    }

    companion object {
        const val MAX_CACHED_AGE_MILLIS = 60_000L

        internal fun FixQuality.priority() = when (this) {
            FixQuality.BALANCED -> Priority.PRIORITY_BALANCED_POWER_ACCURACY
            FixQuality.HIGH -> Priority.PRIORITY_HIGH_ACCURACY
        }

        /**
         * Design F.3. Backgrounded Meetup: send on 20 m movement or 60 s (the core drops to a
         * 5-minute keep-alive when stationary on its own). Compass open: GPS at 1 Hz.
         */
        internal fun LocationMode.continuousRequest(): LocationRequest? = when (this) {
            LocationMode.IDLE, LocationMode.DWELL -> null
            LocationMode.MEETUP_BACKGROUND -> LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, 60_000L)
                .setMinUpdateIntervalMillis(10_000L)
                .setMinUpdateDistanceMeters(20f)
                .build()
            LocationMode.MEETUP_FOREGROUND -> LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, 1_000L)
                .setMinUpdateIntervalMillis(1_000L)
                .build()
        }
    }
}
