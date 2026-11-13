package org.pingers.app.platform

import android.annotation.SuppressLint
import android.content.Context
import android.os.HandlerThread
import com.google.android.gms.location.CurrentLocationRequest
import com.google.android.gms.location.FusedLocationProviderClient
import com.google.android.gms.location.LocationCallback
import com.google.android.gms.location.LocationRequest
import com.google.android.gms.location.LocationResult
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.Priority
import com.google.android.gms.tasks.CancellationTokenSource
import java.util.concurrent.Executors

/**
 * Fused Location: single fixes on request, and the continuous updates of the Meetup modes.
 * Continuous updates run only while the location foreground service holds the process:
 * [LocationModes] starts both together.
 */
class FusedFixes(private val context: Context, private val sink: () -> CoreSink) : FixSource {
    private val client: FusedLocationProviderClient = LocationServices.getFusedLocationProviderClient(context)
    private val thread = HandlerThread("pingers-location").apply { start() }
    private val executor = Executors.newSingleThreadExecutor()
    private var continuous: LocationCallback? = null

    @SuppressLint("MissingPermission") // Checked first.
    override fun request(quality: FixQuality, timeoutMillis: Long) {
        if (!hasLocationPermission(context)) {
            sink().onFix(null)
            return
        }
        val request = CurrentLocationRequest.Builder()
            .setPriority(quality.priority())
            .setDurationMillis(timeoutMillis.coerceAtLeast(1))
            .setMaxUpdateAgeMillis(if (quality == FixQuality.HIGH) 0 else ContinuousRequest.MAX_CACHED_AGE_MILLIS)
            .build()
        client.getCurrentLocation(request, CancellationTokenSource().token)
            // Off the main thread: the core may take a while with a fix.
            .addOnSuccessListener(executor) { location -> sink().onFix(location?.toFix()) }
            .addOnFailureListener(executor) { sink().onFix(null) }
    }

    @SuppressLint("MissingPermission") // Checked first.
    @Synchronized
    override fun follow(mode: LocationMode) {
        continuous?.let { client.removeLocationUpdates(it) }
        continuous = null
        val request = ContinuousRequest.of(mode)?.toLocationRequest() ?: return
        if (!hasLocationPermission(context)) return
        val callback = object : LocationCallback() {
            override fun onLocationResult(result: LocationResult) {
                result.lastLocation?.let { sink().onFix(it.toFix()) }
            }
        }
        client.requestLocationUpdates(request, callback, thread.looper)
        continuous = callback
    }

    companion object {
        internal fun FixQuality.priority() = when (this) {
            FixQuality.BALANCED -> Priority.PRIORITY_BALANCED_POWER_ACCURACY
            FixQuality.HIGH -> Priority.PRIORITY_HIGH_ACCURACY
        }

        internal fun ContinuousRequest.toLocationRequest(): LocationRequest = LocationRequest.Builder(
            if (highAccuracy) Priority.PRIORITY_HIGH_ACCURACY else Priority.PRIORITY_BALANCED_POWER_ACCURACY,
            intervalMillis,
        )
            .setMinUpdateIntervalMillis(minIntervalMillis)
            .setMinUpdateDistanceMeters(minDistanceMeters)
            .build()
    }
}
