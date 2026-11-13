package org.pingers.app.platform

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.location.LocationManager
import android.net.Uri
import android.util.Log
import androidx.core.content.edit

/**
 * OS geofences without Play services, for the `foss` flavour: one LocationManager proximity
 * alert per region. They report ENTER and EXIT only; the core confirms dwelling with fixes
 * (design F.2), so the missing DWELL costs a fix, not correctness. Responsiveness is the
 * platform's.
 */
class ProximityGeofences(private val context: Context) : GeofenceRegistrar {
    private val manager = context.getSystemService(LocationManager::class.java)
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    @SuppressLint("MissingPermission") // Checked first.
    @Synchronized
    override fun replace(regions: List<OsRegion>, responsivenessMillis: Int) {
        // Proximity alerts are removed by an equal PendingIntent, so the registered ids are kept.
        prefs.getStringSet(KEY_IDS, emptySet()).orEmpty().forEach { manager.removeProximityAlert(pendingIntent(context, it)) }
        prefs.edit { remove(KEY_IDS) }
        if (regions.isEmpty()) return
        if (!Geofences.canRegister(context)) {
            Log.w(Geofences.TAG, "geofences not registered: background location not granted")
            return
        }
        regions.forEach { manager.addProximityAlert(it.latitude, it.longitude, it.radiusMeters, -1, pendingIntent(context, it.id)) }
        prefs.edit { putStringSet(KEY_IDS, regions.map { it.id }.toSet()) }
    }

    companion object {
        private const val PREFS = "pingers.proximity"
        private const val KEY_IDS = "ids"

        /** Mutable: the system adds KEY_PROXIMITY_ENTERING. The region id is the data URI, so each alert has its own PendingIntent. */
        private fun pendingIntent(context: Context, id: String): PendingIntent = PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, ProximityReceiver::class.java)
                .setAction(ProximityReceiver.ACTION)
                .setData(Uri.fromParts(ProximityReceiver.SCHEME, id, null)),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
    }
}

/** Forwards proximity alerts to the core as geofence transitions. */
class ProximityReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != ACTION || !intent.hasExtra(LocationManager.KEY_PROXIMITY_ENTERING)) return
        val id = intent.data?.schemeSpecificPart ?: return
        val transition = if (intent.getBooleanExtra(LocationManager.KEY_PROXIMITY_ENTERING, false)) GeofenceTransition.ENTER else GeofenceTransition.EXIT
        runAsync { PlatformHost.sink.onGeofence(listOf(id), transition, null) }
    }

    companion object {
        const val ACTION = "org.pingers.app.platform.PROXIMITY"
        const val SCHEME = "pingers-region"
    }
}
