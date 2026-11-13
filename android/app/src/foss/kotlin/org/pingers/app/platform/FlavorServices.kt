package org.pingers.app.platform

import android.content.Context

/** The `foss` flavour: no Google code at all. LocationManager, proximity alerts, UnifiedPush only. */
object FlavorServices {
    const val NAME = "foss"

    fun fixes(context: Context, sink: () -> CoreSink): FixSource = ManagerFixes(context, sink)

    fun geofences(context: Context): GeofenceRegistrar = ProximityGeofences(context)

    /** No FCM in this flavour. */
    @Suppress("UNUSED_PARAMETER")
    fun fcmAvailable(context: Context): Boolean = false

    /** Nothing to start: push is UnifiedPush, registered through [Push.registerUnifiedPush]. */
    @Suppress("UNUSED_PARAMETER")
    fun startPush(context: Context) = Unit
}
