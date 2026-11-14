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
 * endpoint URL; in the `gms` flavour it can be the FCM registration token (FcmService). UnifiedPush wins when both exist, because it needs no publisher credentials.
 */
object Push {
    private const val TAG = "PingersPush"
    const val PREFS = "pingers.push"
    private const val KEY_UP_ENDPOINT = "unifiedpush.endpoint"
    private const val KEY_FCM_TOKEN = "fcm.token"
    private const val KEY_OPTED_OUT = "unifiedpush.opted-out"

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

    /** A distributor's name as the launcher shows it ("ntfy"), or its package name. */
    fun label(context: Context, pkg: String): String = try {
        val pm = context.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString()
    } catch (_: android.content.pm.PackageManager.NameNotFoundException) {
        pkg
    }

    /** The distributor Pingers is registered with, if it is still installed. */
    fun distributor(context: Context): String? =
        UnifiedPush.getSavedDistributor(context)?.takeIf { it in distributors(context) }

    /**
     * Registers with a UnifiedPush distributor: [distributor] if given, else the one saved,
     * the system default, or the only one installed. Returns false when there is none to pick
     * (the UI then offers the list from [distributors], or explains that push needs one).
     * The endpoint arrives later through [PingersPushService].
     */
    fun registerUnifiedPush(context: Context, distributor: String? = null): Boolean {
        prefs(context).edit { putBoolean(KEY_OPTED_OUT, false) }
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
        prefs(context).edit { putBoolean(KEY_OPTED_OUT, true) }
        UnifiedPush.unregister(context, INSTANCE)
        setEndpoint(context, null)
    }

    /**
     * At every start, as UnifiedPush asks: registers again with the saved distributor (or the
     * default one), so a new endpoint reaches the core; unless the user turned wake-up pushes off.
     * When no distributor is left (it was uninstalled), the endpoint goes, and the core marks
     * wake-ups downgraded ("no push service").
     */
    fun renew(context: Context) {
        if (prefs(context).getBoolean(KEY_OPTED_OUT, false)) return
        if (!registerUnifiedPush(context)) setEndpoint(context, null)
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
     * A push woke the app: the core reads the mailbox or group log the handle names, and posts
     * what the wake led to through its events (CoreHost) before this returns. Runs off the main
     * thread; the distributor (or FCM) holds a wake lock for the few seconds this takes. FCM
     * messages bring their [priority], which the core counts (design B.4.9).
     */
    fun wake(platform: PushPlatform, payload: ByteArray, priority: FcmPriority?) {
        executor.execute {
            try {
                PlatformHost.sink.onPush(platform, payload, priority)
            } catch (e: RuntimeException) {
                Log.w(TAG, "core failed on push: ${e.message}")
            }
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
        // UnifiedPush has no priorities to lose: every push counts as on time.
        PhoneLog.recordWake(this, onTime = true)
        Push.wake(PushPlatform.UNIFIED_PUSH, message.content, priority = null)
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
