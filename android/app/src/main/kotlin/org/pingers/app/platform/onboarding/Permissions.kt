package org.pingers.app.platform.onboarding

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat

/** The permission steps of onboarding, in the order they must be asked (architecture section 4). */
enum class PermissionStep {
    /** Precise location while the app is open. Without it nothing works. */
    FINE_LOCATION,

    /**
     * "Allow all the time". Android 11+ grants it only on its own, after fine location, from
     * the system settings page; Play requires an in-context explanation first.
     */
    BACKGROUND_LOCATION,

    /** Android 13+: knocks and Meetups are notifications. */
    NOTIFICATIONS,
}

/** What has been granted, and which steps the user has put off with "Not now" this session. */
data class PermissionState(
    val fineLocation: Boolean,
    val backgroundLocation: Boolean,
    val notifications: Boolean,
    val skipped: Set<PermissionStep> = emptySet(),
) {
    /** The next step to show, or null when onboarding is done. */
    fun next(): PermissionStep? = when {
        !fineLocation && PermissionStep.FINE_LOCATION !in skipped -> PermissionStep.FINE_LOCATION
        // Background location can't be asked for before fine location is granted.
        fineLocation && !backgroundLocation && PermissionStep.BACKGROUND_LOCATION !in skipped -> PermissionStep.BACKGROUND_LOCATION
        !notifications && PermissionStep.NOTIFICATIONS !in skipped -> PermissionStep.NOTIFICATIONS
        else -> null
    }

    companion object {
        fun read(context: Context, skipped: Set<PermissionStep> = emptySet()) = PermissionState(
            fineLocation = granted(context, Manifest.permission.ACCESS_FINE_LOCATION),
            backgroundLocation = granted(context, Manifest.permission.ACCESS_BACKGROUND_LOCATION),
            notifications = Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                granted(context, Manifest.permission.POST_NOTIFICATIONS),
            skipped = skipped,
        )

        private fun granted(context: Context, permission: String) =
            ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED
    }
}

/** The runtime permissions one step asks for. */
@android.annotation.SuppressLint("InlinedApi") // POST_NOTIFICATIONS is only asked for on Android 13 and up.
fun PermissionStep.permissions(): Array<String> = when (this) {
    // Coarse alongside fine, as Android 12+ requires; the user may still pick approximate.
    PermissionStep.FINE_LOCATION -> arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION)
    PermissionStep.BACKGROUND_LOCATION -> arrayOf(Manifest.permission.ACCESS_BACKGROUND_LOCATION)
    PermissionStep.NOTIFICATIONS -> arrayOf(Manifest.permission.POST_NOTIFICATIONS)
}
