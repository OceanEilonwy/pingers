package org.pingers.app.data

import android.util.Log
import org.pingers.app.platform.AndroidPlatform
import org.pingers.app.platform.FixQuality
import org.pingers.app.platform.FocusState
import org.pingers.app.platform.LocationMode
import org.pingers.app.platform.OsRegion
import org.pingers.app.platform.PlatformException
import org.pingers.app.platform.PushPlatform
import org.pingers.app.platform.SecureStoreKey
import uniffi.pingers.Platform
import uniffi.pingers.PushTokens

/**
 * The UniFFI-generated [Platform] (pingers-ffi `trait Platform`), delegating to the shell's
 * [AndroidPlatform]. Conversion only: the generated types become the platform layer's.
 *
 * The core calls these on its own threads. Only the secure store may throw (as the core's
 * `PlatformError`); anything else the platform throws is logged here, because an exception
 * out of a callback without an error type would abort the core.
 */
class FfiPlatform(private val platform: AndroidPlatform) : Platform {

    override fun secureStoreGet(key: String): ByteArray? = store { platform.secureStoreGet(SecureStoreKey(key)) }

    override fun secureStorePut(key: String, value: ByteArray) = store { platform.secureStorePut(SecureStoreKey(key), value) }

    override fun setOsGeofences(regions: List<uniffi.pingers.OsRegion>) = guarded("geofences") {
        platform.setOsGeofences(
            regions.map { OsRegion(it.id, it.center.latitude, it.center.longitude, it.radiusM.toFloat()) },
        )
    }

    override fun requestFix(quality: uniffi.pingers.FixQuality, timeoutMs: UInt) = guarded("fix") {
        val q = when (quality) {
            uniffi.pingers.FixQuality.BALANCED -> FixQuality.BALANCED
            uniffi.pingers.FixQuality.HIGH -> FixQuality.HIGH
        }
        platform.requestFix(q, timeoutMs.toLong())
    }

    override fun setLocationMode(mode: uniffi.pingers.LocationMode) = guarded("location mode") {
        platform.setLocationMode(
            when (mode) {
                uniffi.pingers.LocationMode.IDLE -> LocationMode.IDLE
                uniffi.pingers.LocationMode.DWELL -> LocationMode.DWELL
                uniffi.pingers.LocationMode.MEETUP_BACKGROUND -> LocationMode.MEETUP_BACKGROUND
                uniffi.pingers.LocationMode.MEETUP_FOREGROUND -> LocationMode.MEETUP_FOREGROUND
            },
        )
    }

    override fun focusState(): uniffi.pingers.FocusState {
        val state = try {
            platform.focusState()
        } catch (e: RuntimeException) {
            Log.w(TAG, "focus: ${e::class.simpleName}")
            FocusState.UNKNOWN
        }
        return when (state) {
            FocusState.ON -> uniffi.pingers.FocusState.ON
            FocusState.OFF -> uniffi.pingers.FocusState.OFF
            FocusState.UNKNOWN -> uniffi.pingers.FocusState.UNKNOWN
        }
    }

    override fun scheduleWake(afterSeconds: UInt) = guarded("wake") {
        platform.scheduleWake(afterSeconds.toLong() * 1000L)
    }

    override fun pushTokens(): PushTokens? = try {
        platform.pushTokens()?.let {
            PushTokens(
                platform = when (it.platform) {
                    PushPlatform.FCM -> uniffi.pingers.PushPlatform.FCM
                    PushPlatform.UNIFIED_PUSH -> uniffi.pingers.PushPlatform.UNIFIED_PUSH
                },
                token = it.token,
            )
        }
    } catch (e: RuntimeException) {
        Log.w(TAG, "push tokens: ${e::class.simpleName}")
        null
    }

    private inline fun <T> store(block: () -> T): T = try {
        block()
    } catch (e: PlatformException) {
        Log.w(TAG, "secure store: ${e.error}")
        throw uniffi.pingers.PlatformException.SecureStoreUnavailable()
    }

    private inline fun guarded(what: String, block: () -> Unit) {
        try {
            block()
        } catch (e: RuntimeException) {
            Log.w(TAG, "$what: ${e::class.simpleName}")
        }
    }

    private companion object {
        const val TAG = "PingersCore"
    }
}
