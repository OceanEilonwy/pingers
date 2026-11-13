package org.pingers.app.platform

import android.annotation.SuppressLint
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * The Android shell's [Platform] (architecture section 4). One per process: [get]. The app
 * hands it to the core when it builds the core, and sets [PlatformHost.sinkProvider] so the
 * receivers, services and workers below reach that core.
 */
class AndroidPlatform private constructor(context: Context) : Platform {
    private val app = context.applicationContext
    val secureStore = SecureStore(app)
    val geofences = Geofences(app, secureStore)
    val fixes = FusedFixes(app) { PlatformHost.sink }
    val modes = LocationModes(app, fixes)
    val focus = Focus(app)
    val wakes = Wakes(app)

    init {
        Notifications.ensureChannels(app)
        focus.watch { PlatformHost.sink.onFocusChanged(it) }
    }

    override fun secureStoreGet(key: SecureStoreKey): ByteArray? = secureStore.get(key)

    override fun secureStorePut(key: SecureStoreKey, value: ByteArray) = secureStore.put(key, value)

    override fun setOsGeofences(regions: List<OsRegion>) = geofences.replaceAll(regions)

    override fun requestFix(quality: FixQuality, timeoutMillis: Long) = fixes.request(quality, timeoutMillis)

    override fun setLocationMode(mode: LocationMode) = modes.set(mode)

    override fun focusState(): FocusState = focus.current()

    override fun scheduleWake(afterMillis: Long) = wakes.schedule(afterMillis)

    override fun pushTokens(): PushTokens? = Push.tokens(app)

    companion object {
        // Holds the application context only, which lives as long as the process.
        @SuppressLint("StaticFieldLeak")
        @Volatile
        private var instance: AndroidPlatform? = null

        fun get(context: Context): AndroidPlatform =
            instance ?: synchronized(this) { instance ?: AndroidPlatform(context).also { instance = it } }
    }
}

/**
 * After a reboot or an app update Play services has forgotten every geofence: register the
 * last set again, bring back "Keep Pingers running", and wake the core.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val reason = when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED -> WakeReason.BOOT
            Intent.ACTION_MY_PACKAGE_REPLACED -> WakeReason.APP_UPDATED
            else -> return
        }
        val platform = AndroidPlatform.get(context)
        if (platform.modes.keepRunning) LocationService.start(context, meetup = false)
        runAsync {
            platform.geofences.restore()
            PlatformHost.sink.onWake(reason)
        }
    }
}
