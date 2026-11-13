package org.pingers.app.platform

import android.content.Context
import android.util.Log
import androidx.core.content.edit
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import org.unifiedpush.android.connector.data.ResolvedDistributor
import java.util.concurrent.Executors

/**
 * Push registration (design B.4.9, B.7). The push token never leaves the phone raw: the core
 * seals it per mailbox and subscription. With UnifiedPush the token is the distributor's
 * endpoint URL; with an FCM build it is the FCM registration token (FcmService, built with
 * `-PpingersFcm`). UnifiedPush wins when both exist, because it needs no publisher credentials.
 */
object Push {
    private const val TAG = "PingersPush"
    const val PREFS = "pingers.push"
    private const val KEY_UP_ENDPOINT = "unifiedpush.endpoint"
    private const val KEY_FCM_TOKEN = "fcm.token"

    /** The connector's instance; Pingers has exactly one registration. */
    const val INSTANCE = "pingers"

    private val executor = Executors.newSingleThreadExecutor()

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    fun tokens(context: Context): PushTokens? {
        val prefs = prefs(context)
        prefs.getString(KEY_UP_ENDPOINT, null)?.let { return PushTokens(PushPlatform.UNIFIED_PUSH, it.toByteArray()) }
        prefs.getString(KEY_FCM_TOKEN, null)?.let { return PushTokens(PushPlatform.FCM, it.toByteArray()) }
        return null
    }

    /** The UnifiedPush distributors installed (ntfy, a Nextcloud app, ...), by package name. */
    fun distributors(context: Context): List<String> = UnifiedPush.getDistributors(context)

    /**
     * Registers with a UnifiedPush distributor: [distributor] if given, else the one saved,
     * the system default, or the only one installed. Returns false when there is none to pick
     * (the UI then offers the list from [distributors], or explains that push needs one).
     * The endpoint arrives later through [PingersPushService].
     */
    fun registerUnifiedPush(context: Context, distributor: String? = null): Boolean {
        val chosen = distributor
            ?: UnifiedPush.getSavedDistributor(context)?.takeIf { it in distributors(context) }
            ?: when (val resolved = UnifiedPush.resolveDefaultDistributor(context)) {
                is ResolvedDistributor.Found -> resolved.packageName
                else -> null
            }
            ?: return false
        UnifiedPush.saveDistributor(context, chosen)
        UnifiedPush.register(context, INSTANCE, null, null)
        return true
    }

    /** Ends the UnifiedPush registration ("No wake-up pushes", design B.7). */
    fun unregisterUnifiedPush(context: Context) {
        UnifiedPush.unregister(context, INSTANCE)
        setEndpoint(context, null)
    }

    internal fun setEndpoint(context: Context, endpoint: String?) {
        if (prefs(context).getString(KEY_UP_ENDPOINT, null) == endpoint) return
        prefs(context).edit { putString(KEY_UP_ENDPOINT, endpoint) }
        val tokens = tokens(context)
        executor.execute { PlatformHost.sink.onPushTokensChanged(tokens) }
    }

    /** Called by FcmService (FCM builds only). */
    fun setFcmToken(context: Context, token: String?) {
        if (prefs(context).getString(KEY_FCM_TOKEN, null) == token) return
        prefs(context).edit { putString(KEY_FCM_TOKEN, token) }
        val tokens = tokens(context)
        executor.execute { PlatformHost.sink.onPushTokensChanged(tokens) }
    }

    /**
     * A push woke the app: the core reads the mailbox or group log the handle names and says
     * what to show. Runs off the main thread; the distributor (or FCM) holds a wake lock for
     * the few seconds this takes.
     */
    fun wake(context: Context, platform: PushPlatform, payload: ByteArray, downgraded: Boolean) {
        val app = context.applicationContext
        executor.execute {
            val notification = try {
                PlatformHost.sink.onPush(platform, payload, downgraded)
            } catch (e: RuntimeException) {
                Log.w(TAG, "core failed on push: ${e.message}")
                null
            }
            notification?.let { Notifications.post(app, it) }
        }
    }
}

/**
 * The UnifiedPush connector's service: the distributor binds to it for each new endpoint,
 * message, failure and unregistration.
 */
class PingersPushService : PushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        if (instance != Push.INSTANCE) return
        Push.setEndpoint(this, endpoint.url)
    }

    override fun onMessage(message: PushMessage, instance: String) {
        if (instance != Push.INSTANCE) return
        Push.wake(this, PushPlatform.UNIFIED_PUSH, message.content, downgraded = false)
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        if (instance != Push.INSTANCE) return
        Log.w(TAG, "UnifiedPush registration failed: $reason")
    }

    override fun onUnregistered(instance: String) {
        if (instance != Push.INSTANCE) return
        Push.setEndpoint(this, null)
    }

    private companion object {
        const val TAG = "PingersPush"
    }
}
