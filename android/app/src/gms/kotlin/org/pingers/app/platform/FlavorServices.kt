package org.pingers.app.platform

import android.content.Context

/** The `gms` flavour: Fused Location, the Geofencing API, and FCM alongside UnifiedPush. */
object FlavorServices {
    const val NAME = "gms"

    fun fixes(context: Context, sink: () -> CoreSink): FixSource = FusedFixes(context, sink)

    fun geofences(context: Context): GeofenceRegistrar = GmsGeofences(context)

    /** Whether FCM is configured in this build. */
    fun fcmAvailable(context: Context): Boolean = Fcm.available(context)

    /**
     * Registers UnifiedPush again ([Push.renew]; it wins when a distributor is installed) and
     * fetches the FCM token, which goes to the core through [Push.setFcmToken].
     */
    fun startPush(context: Context) {
        Push.renew(context)
        Fcm.refreshToken(context)
    }
}
