package org.pingers.app.platform

import android.annotation.SuppressLint
import android.content.Context
import android.location.LocationManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import androidx.core.location.LocationListenerCompat
import androidx.core.location.LocationManagerCompat
import androidx.core.location.LocationRequestCompat
import androidx.core.os.CancellationSignal
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Fixes from the platform LocationManager, for the `foss` flavour (no Play services). High
 * quality asks GPS; balanced asks the platform's fused provider where there is one (Android 12+),
 * else the network provider, else GPS.
 */
class ManagerFixes(context: Context, private val sink: () -> CoreSink) : FixSource {
    private val app = context.applicationContext
    private val manager = app.getSystemService(LocationManager::class.java)
    private val thread = HandlerThread("pingers-location").apply { start() }
    private val handler = Handler(thread.looper)
    private val executor = Executor { handler.post(it) }
    private var continuous: LocationListenerCompat? = null

    private fun provider(quality: FixQuality): String {
        val enabled = manager.getProviders(true)
        if (quality == FixQuality.HIGH && LocationManager.GPS_PROVIDER in enabled) return LocationManager.GPS_PROVIDER
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && LocationManager.FUSED_PROVIDER in enabled) return LocationManager.FUSED_PROVIDER
        if (LocationManager.NETWORK_PROVIDER in enabled) return LocationManager.NETWORK_PROVIDER
        return LocationManager.GPS_PROVIDER
    }

    @SuppressLint("MissingPermission") // Checked first.
    override fun request(quality: FixQuality, timeoutMillis: Long) {
        if (!hasLocationPermission(app)) {
            sink().onFix(null)
            return
        }
        // A recent enough fix answers at once, as Fused Location's max update age does.
        if (quality == FixQuality.BALANCED) {
            val last = manager.getLastKnownLocation(provider(quality))
            if (last != null && System.currentTimeMillis() - last.time < ContinuousRequest.MAX_CACHED_AGE_MILLIS) {
                executor.execute { sink().onFix(last.toFix()) }
                return
            }
        }
        val done = AtomicBoolean(false)
        val cancel = CancellationSignal()
        handler.postDelayed({
            if (done.compareAndSet(false, true)) {
                cancel.cancel()
                sink().onFix(null)
            }
        }, timeoutMillis.coerceAtLeast(1))
        LocationManagerCompat.getCurrentLocation(manager, provider(quality), cancel, executor) { location ->
            if (done.compareAndSet(false, true)) sink().onFix(location?.toFix())
        }
    }

    @SuppressLint("MissingPermission") // Checked first.
    @Synchronized
    override fun follow(mode: LocationMode) {
        continuous?.let { LocationManagerCompat.removeUpdates(manager, it) }
        continuous = null
        val wanted = ContinuousRequest.of(mode) ?: return
        if (!hasLocationPermission(app)) return
        val request = LocationRequestCompat.Builder(wanted.intervalMillis)
            .setQuality(if (wanted.highAccuracy) LocationRequestCompat.QUALITY_HIGH_ACCURACY else LocationRequestCompat.QUALITY_BALANCED_POWER_ACCURACY)
            .setMinUpdateIntervalMillis(wanted.minIntervalMillis)
            .setMinUpdateDistanceMeters(wanted.minDistanceMeters)
            .build()
        val listener = LocationListenerCompat { sink().onFix(it.toFix()) }
        val quality = if (wanted.highAccuracy) FixQuality.HIGH else FixQuality.BALANCED
        LocationManagerCompat.requestLocationUpdates(manager, provider(quality), request, executor, listener)
        continuous = listener
    }
}
