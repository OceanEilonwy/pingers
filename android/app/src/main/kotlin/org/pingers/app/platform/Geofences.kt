package org.pingers.app.platform

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.pm.PackageManager
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.Executors

/**
 * OS geofences (design E.2). The core picks the regions (up to 97 Spots plus the horizon
 * region); each call replaces every region registered before. [registrar] is the flavour's:
 * the Geofencing API in `gms`, proximity alerts in `foss`. The last set is kept in the secure
 * store (Spot positions never sit on disk in the clear), because the OS forgets geofences on
 * reboot, on app update and when Play services' data is cleared; [BootReceiver] registers them
 * again.
 */
class Geofences(
    context: Context,
    private val store: SecureStore,
    /** Notification responsiveness; design E.2 says 60 s. Tests pass 0. */
    private val responsivenessMillis: Int = RESPONSIVENESS_MILLIS,
    private val registrar: GeofenceRegistrar = FlavorServices.geofences(context.applicationContext),
) {
    /** Replaces all registered OS geofences with [regions]. */
    fun replaceAll(regions: List<OsRegion>) {
        require(regions.size <= MAX_REGIONS) { "Android allows $MAX_REGIONS geofences per app, got ${regions.size}" }
        try {
            store.put(STORE_KEY, encode(regions).toByteArray())
        } catch (e: PlatformException) {
            Log.w(TAG, "geofences not saved for restore: ${e.message}")
        }
        registrar.replace(regions, responsivenessMillis)
    }

    /** Registers the last set again (after reboot or update). */
    fun restore() = registrar.replace(saved(), responsivenessMillis)

    internal fun saved(): List<OsRegion> = try {
        store.get(STORE_KEY)?.let { decode(String(it)) }.orEmpty()
    } catch (_: PlatformException) {
        emptyList()
    }

    companion object {
        internal const val TAG = "PingersGeofences"
        private val STORE_KEY = SecureStoreKey("pingers.geofences")

        /** Android's per-app limit. The core keeps 97 Spots, a horizon region and one spare. */
        const val MAX_REGIONS = 100
        const val LOITERING_MILLIS = 90_000
        const val RESPONSIVENESS_MILLIS = 60_000

        /** Geofences need fine location, and background location on Android 10 and up. */
        fun canRegister(context: Context): Boolean =
            ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED &&
                ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_BACKGROUND_LOCATION) == PackageManager.PERMISSION_GRANTED

        internal fun encode(regions: List<OsRegion>): String = JSONArray().apply {
            regions.forEach {
                put(JSONObject().put("id", it.id).put("lat", it.latitude).put("lon", it.longitude).put("r", it.radiusMeters.toDouble()))
            }
        }.toString()

        internal fun decode(json: String): List<OsRegion> = try {
            val array = JSONArray(json)
            List(array.length()) { i ->
                val o = array.getJSONObject(i)
                OsRegion(o.getString("id"), o.getDouble("lat"), o.getDouble("lon"), o.getDouble("r").toFloat())
            }
        } catch (_: org.json.JSONException) {
            emptyList()
        }
    }
}

/** Runs [block] off the main thread within the receiver's goAsync window. */
internal fun BroadcastReceiver.runAsync(block: () -> Unit) {
    val pending = goAsync()
    receiverExecutor.execute {
        try {
            block()
        } finally {
            pending.finish()
        }
    }
}

private val receiverExecutor = Executors.newSingleThreadExecutor()
