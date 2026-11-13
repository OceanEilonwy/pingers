package org.pingers.app.platform

import android.content.Context

/** The `gms` flavour: Fused Location, the Geofencing API, and FCM alongside UnifiedPush. */
object FlavorServices {
    const val NAME = "gms"

    fun fixes(context: Context, sink: () -> CoreSink): FixSource = FusedFixes(context, sink)

    fun geofences(context: Context): GeofenceRegistrar = GmsGeofences(context)

    /** Whether FCM is configured in this build. */
    fun fcmAvailable(context: Context): Boolean = Fcm.available(context)

    /** Starts FCM: fetches the token, which goes to the core through [Push.setFcmToken]. */
    fun startPush(context: Context) = Fcm.refreshToken(context)
}
