package org.pingers.app.platform

import android.Manifest
import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import androidx.core.content.ContextCompat
import com.google.android.gms.location.Geofence
import com.google.android.gms.location.GeofencingClient
import com.google.android.gms.location.GeofencingEvent
import com.google.android.gms.location.GeofencingRequest
import com.google.android.gms.location.LocationServices
import org.json.JSONArray
import org.json.JSONObject
import java.util.concurrent.Executors

/**
 * Geofencing API registration (design E.2). The core picks the regions (up to 97 Spots plus
 * the horizon region); each call replaces every region registered before. The last set is kept
 * in the secure store (Spot positions never sit on disk in the clear), because Play services
 * forgets all geofences on reboot, on app update and when its data is cleared; [BootReceiver]
 * registers them again.
 */
class Geofences(
    private val context: Context,
    private val store: SecureStore,
    /** Notification responsiveness; design E.2 says 60 s. Tests pass 0. */
    private val responsivenessMillis: Int = RESPONSIVENESS_MILLIS,
) {
    private val client: GeofencingClient = LocationServices.getGeofencingClient(context)

    /** Replaces all registered OS geofences with [regions]. */
    fun replaceAll(regions: List<OsRegion>) {
        require(regions.size <= MAX_REGIONS) { "Android allows $MAX_REGIONS geofences per app, got ${regions.size}" }
        try {
            store.put(STORE_KEY, encode(regions).toByteArray())
        } catch (e: PlatformException) {
            Log.w(TAG, "geofences not saved for restore: ${e.message}")
        }
        register(regions)
    }

    /** Registers the last set again (after reboot or update). */
    fun restore() = register(saved())

    internal fun saved(): List<OsRegion> = try {
        store.get(STORE_KEY)?.let { decode(String(it)) }.orEmpty()
    } catch (_: PlatformException) {
        emptyList()
    }

    @SuppressLint("MissingPermission") // Checked first.
    private fun register(regions: List<OsRegion>) {
        val pending = pendingIntent(context)
        client.removeGeofences(pending).addOnCompleteListener {
            if (regions.isEmpty()) return@addOnCompleteListener
            if (!canRegister(context)) {
                Log.w(TAG, "geofences not registered: background location not granted")
                return@addOnCompleteListener
            }
            val request = GeofencingRequest.Builder()
                // Already inside when registered: report it, so the core does not wait for a crossing.
                .setInitialTrigger(GeofencingRequest.INITIAL_TRIGGER_ENTER or GeofencingRequest.INITIAL_TRIGGER_DWELL)
                .addGeofences(regions.map { it.toGeofence(responsivenessMillis) })
                .build()
            client.addGeofences(request, pending)
                .addOnFailureListener { e -> Log.w(TAG, "geofence registration failed: ${e.message}") }
        }
    }

    companion object {
        private const val TAG = "PingersGeofences"
        private val STORE_KEY = SecureStoreKey("pingers.geofences")

        /** Android's per-app limit. The core keeps 97 Spots, a horizon region and one spare. */
        const val MAX_REGIONS = 100
        const val LOITERING_MILLIS = 90_000
        const val RESPONSIVENESS_MILLIS = 60_000

        /** Geofences need fine location, and background location on Android 10 and up. */
        fun canRegister(context: Context): Boolean =
            ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED &&
                ContextCompat.checkSelfPermission(context, Manifest.permission.ACCESS_BACKGROUND_LOCATION) == PackageManager.PERMISSION_GRANTED

        internal fun OsRegion.toGeofence(responsivenessMillis: Int): Geofence = Geofence.Builder()
            .setRequestId(id)
            .setCircularRegion(latitude, longitude, radiusMeters)
            .setExpirationDuration(Geofence.NEVER_EXPIRE)
            .setTransitionTypes(Geofence.GEOFENCE_TRANSITION_ENTER or Geofence.GEOFENCE_TRANSITION_DWELL or Geofence.GEOFENCE_TRANSITION_EXIT)
            .setLoiteringDelay(LOITERING_MILLIS)
            .setNotificationResponsiveness(responsivenessMillis)
            .build()

        /** Mutable: Play services adds the event to it. Explicit, so only [GeofenceReceiver] gets it. */
        private fun pendingIntent(context: Context): PendingIntent = PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, GeofenceReceiver::class.java).setAction(GeofenceReceiver.ACTION),
            PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0),
        )

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

/** Forwards Geofencing API transitions to the core. */
class GeofenceReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION) return
        val event = GeofencingEvent.fromIntent(intent) ?: return
        if (event.hasError()) {
            // GEOFENCE_NOT_AVAILABLE: location was turned off and Play services dropped every geofence.
            Log.w(TAG, "geofence error ${event.errorCode}")
            return
        }
        val transition = when (event.geofenceTransition) {
            Geofence.GEOFENCE_TRANSITION_ENTER -> GeofenceTransition.ENTER
            Geofence.GEOFENCE_TRANSITION_DWELL -> GeofenceTransition.DWELL
            Geofence.GEOFENCE_TRANSITION_EXIT -> GeofenceTransition.EXIT
            else -> return
        }
        val ids = event.triggeringGeofences.orEmpty().map { it.requestId }
        val location = event.triggeringLocation?.toFix()
        runAsync { PlatformHost.sink.onGeofence(ids, transition, location) }
    }

    companion object {
        private const val TAG = "PingersGeofences"
        const val ACTION = "org.pingers.app.platform.GEOFENCE"
    }
}
