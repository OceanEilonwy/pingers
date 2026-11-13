package org.pingers.app.platform

/*
 * The platform layer's side of the Rust core's `Platform` callback interface (pingers-ffi
 * `trait Platform`). These types mirror the generated ones in `uniffi.pingers`, so this package
 * builds and tests without the Rust library; data/FfiPlatform.kt implements the generated
 * interface by converting to these and calling AndroidPlatform.
 */

/** Why a platform call failed (pingers-client `PlatformError`). */
class PlatformException(val error: PlatformError, cause: Throwable? = null) :
    Exception(error.name, cause)

enum class PlatformError {
    /** The secure store (Keystore-wrapped storage) refused or is locked. */
    SECURE_STORE_UNAVAILABLE,
}

/** A name for a value in the secure store (pingers-client `SecureStoreKey`). */
@JvmInline
value class SecureStoreKey(val name: String) {
    companion object {
        /** The SQLCipher database key. */
        val DATABASE = SecureStoreKey("pingers.database-key")
    }
}

/** An OS geofence: a circle with an opaque id reported back with each transition (design E.2). */
data class OsRegion(val id: String, val latitude: Double, val longitude: Double, val radiusMeters: Float)

/** How hard the platform should try for a fix. */
enum class FixQuality {
    /** Balanced power and accuracy, the pipeline default. */
    BALANCED,

    /** GPS quality, for liveness checks and the compass. */
    HIGH,
}

/** The location mode the shell runs (design E.1, F.3). */
enum class LocationMode {
    /** Region monitoring only. */
    IDLE,

    /** Inside a Spot alone: no continuous GPS. */
    DWELL,

    /** In a Meetup, app in the background: the location foreground service. */
    MEETUP_BACKGROUND,

    /** In a Meetup with the compass open: GPS at 1 Hz. */
    MEETUP_FOREGROUND,
}

/** The interruption filter as the core sees it (pingers-gate `FocusState`). */
enum class FocusState {
    /** A Focus or DND mode is on. */
    ON,

    /** No such mode is on. */
    OFF,

    /** The platform would not say. Does not suppress; the core raises `DndUnavailable`. */
    UNKNOWN,
}

/** The push service a token belongs to (pingers-proto `PushPlatform`). */
enum class PushPlatform { FCM, UNIFIED_PUSH }

/** The device's push registration (pingers-client `PushTokens`). For UnifiedPush the token is the endpoint URL. */
class PushTokens(val platform: PushPlatform, val token: ByteArray) {
    override fun equals(other: Any?): Boolean =
        other is PushTokens && other.platform == platform && other.token.contentEquals(token)

    override fun hashCode(): Int = 31 * platform.hashCode() + token.contentHashCode()

    override fun toString(): String = "PushTokens($platform, ${token.size} bytes)"
}

/**
 * What the Android shell provides to the core. Called from core threads; nothing here blocks
 * for long. Mirrors pingers-client's `trait Platform` method for method.
 */
interface Platform {
    /** Reads a value from the Keystore-wrapped secure store. */
    @Throws(PlatformException::class)
    fun secureStoreGet(key: SecureStoreKey): ByteArray?

    /** Writes a value to the secure store. */
    @Throws(PlatformException::class)
    fun secureStorePut(key: SecureStoreKey, value: ByteArray)

    /** Replaces all registered OS geofences. */
    fun setOsGeofences(regions: List<OsRegion>)

    /** Asks for a fix; the result arrives through [CoreSink.onFix]. */
    fun requestFix(quality: FixQuality, timeoutMillis: Long)

    /** Switches the location mode. */
    fun setLocationMode(mode: LocationMode)

    /** The current interruption filter, read fresh (design B.10). */
    fun focusState(): FocusState

    /** Schedules the next background wake (WorkManager). */
    fun scheduleWake(afterMillis: Long)

    /** The current push registration, if push is set up. */
    fun pushTokens(): PushTokens?
}
