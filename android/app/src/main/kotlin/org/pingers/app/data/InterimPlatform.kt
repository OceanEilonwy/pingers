package org.pingers.app.data

import android.util.Log
import uniffi.pingers.FixQuality
import uniffi.pingers.FocusState
import uniffi.pingers.LocationMode
import uniffi.pingers.OsRegion
import uniffi.pingers.Platform
import uniffi.pingers.PushTokens
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap

/**
 * A stand-in for the generated [Platform] interface, so CoreRepository can open the core
 * before the shell's platform layer (package org.pingers.app.platform, owned by the platform
 * work) implements it. It keeps the secure store in memory with a fresh random database key
 * (so the database is unreadable after a restart), reports Focus as unknown, and logs what the
 * core asks for.
 *
 * REPLACE at integration: CoreRepositories passes the platform package's `AndroidPlatform`
 * (Keystore store holding the database key, Fused Location, geofences, DND, WorkManager,
 * push) once it implements `uniffi.pingers.Platform`, and this file goes.
 */
class InterimPlatform : Platform {
    private val store = ConcurrentHashMap<String, ByteArray>().apply {
        // pingers-client opens the SQLCipher database with the key under this name.
        put(DATABASE_KEY, ByteArray(32).also(SecureRandom()::nextBytes))
    }

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
        const val DATABASE_KEY = "pingers.database-key"
    }
}
