package org.pingers.app.data

import android.util.Log
import uniffi.pingers.FixQuality
import uniffi.pingers.FocusState
import uniffi.pingers.LocationMode
import uniffi.pingers.OsRegion
import uniffi.pingers.Platform
import uniffi.pingers.PushTokens
import java.util.concurrent.ConcurrentHashMap

/**
 * A stand-in for the generated [Platform] interface, so CoreRepository can open the core
 * before the shell's platform layer (package org.pingers.app.platform, owned by the platform
 * work) implements it. It keeps the secure store in memory, reports Focus as unknown, and
 * logs what the core asks for.
 *
 * REPLACE at integration: MainActivity passes the platform package's `AndroidPlatform`
 * (Keystore store, Fused Location, geofences, DND, WorkManager, push) once it implements
 * `uniffi.pingers.Platform`, and this file goes.
 */
class InterimPlatform : Platform {
    private val store = ConcurrentHashMap<String, ByteArray>()

    override fun secureStoreGet(key: String): ByteArray? = store[key]?.copyOf()

    override fun secureStorePut(key: String, value: ByteArray) {
        store[key] = value.copyOf()
    }

    override fun setOsGeofences(regions: List<OsRegion>) {
        Log.i(TAG, "geofences: ${regions.size}")
    }

    override fun requestFix(quality: FixQuality, timeoutMs: UInt) {
        Log.i(TAG, "fix requested: $quality")
    }

    override fun setLocationMode(mode: LocationMode) {
        Log.i(TAG, "location mode: $mode")
    }

    override fun focusState(): FocusState = FocusState.UNKNOWN

    override fun scheduleWake(afterSeconds: UInt) {
        Log.i(TAG, "wake in ${afterSeconds}s")
    }

    override fun pushTokens(): PushTokens? = null

    private companion object {
        const val TAG = "InterimPlatform"
    }
}
