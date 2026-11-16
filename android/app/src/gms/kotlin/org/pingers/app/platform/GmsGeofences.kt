package org.pingers.app.platform

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import com.google.android.gms.location.Geofence
import com.google.android.gms.location.GeofencingClient
import com.google.android.gms.location.GeofencingEvent
import com.google.android.gms.location.GeofencingRequest
import com.google.android.gms.location.LocationServices

/** The Geofencing API: ENTER, DWELL (90 s) and EXIT, one PendingIntent for every region. */
class GmsGeofences(private val context: Context) : GeofenceRegistrar {
    private val client: GeofencingClient = LocationServices.getGeofencingClient(context)

    @SuppressLint("MissingPermission") // Checked first.
    override fun replace(regions: List<OsRegion>, responsivenessMillis: Int) {
        val pending = pendingIntent(context)
        client.removeGeofences(pending).addOnCompleteListener {
            if (regions.isEmpty()) return@addOnCompleteListener
            if (!Geofences.canRegister(context)) {
                Log.w(Geofences.TAG, "geofences not registered: background location not granted")
                return@addOnCompleteListener
            }
            val request = GeofencingRequest.Builder()
                // Already inside when registered: report it, so the core does not wait for a crossing.
                .setInitialTrigger(GeofencingRequest.INITIAL_TRIGGER_ENTER or GeofencingRequest.INITIAL_TRIGGER_DWELL)
                .addGeofences(regions.map { it.toGeofence(responsivenessMillis) })
                .build()
            client.addGeofences(request, pending)
                .addOnFailureListener { e -> Log.w(Geofences.TAG, "geofence registration failed: ${e.forLog()}") }
        }
    }

    companion object {
        internal fun OsRegion.toGeofence(responsivenessMillis: Int): Geofence = Geofence.Builder()
            .setRequestId(id)
            .setCircularRegion(latitude, longitude, radiusMeters)
            .setExpirationDuration(Geofence.NEVER_EXPIRE)
            .setTransitionTypes(Geofence.GEOFENCE_TRANSITION_ENTER or Geofence.GEOFENCE_TRANSITION_DWELL or Geofence.GEOFENCE_TRANSITION_EXIT)
            .setLoiteringDelay(Geofences.LOITERING_MILLIS)
            .setNotificationResponsiveness(responsivenessMillis)
            .build()

        /** Mutable: Play services adds the event to it. Explicit, so only [GeofenceReceiver] gets it. */
        private fun pendingIntent(context: Context): PendingIntent = PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, GeofenceReceiver::class.java).setAction(GeofenceReceiver.ACTION),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
    }
}

/** Forwards Geofencing API transitions to the core. */
class GeofenceReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION) return
        val event = GeofencingEvent.fromIntent(intent) ?: return
        if (event.hasError()) {
            // GEOFENCE_NOT_AVAILABLE: location was turned off and Play services dropped every geofence.
            Log.w(Geofences.TAG, "geofence error ${event.errorCode}")
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
        const val ACTION = "org.pingers.app.platform.GEOFENCE"
    }
}
