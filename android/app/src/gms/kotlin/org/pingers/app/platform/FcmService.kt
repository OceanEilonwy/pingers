package org.pingers.app.platform

import android.content.Context
import android.util.Log
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * FCM pushes (design B.7). The relay sends content-free data messages (fields `v` and `h`, see [PushBody]);
 * this hands each to the same core entry point as UnifiedPush and posts only the notification
 * the core returns. The token goes to the core as an FCM push target; the core seals it to the
 * push relay's key, so the Pingers server never sees it (B.4.9).
 *
 * Every message's original and delivered priority go to the core: FCM downgrades high-priority
 * messages from apps that don't show a notification for them, and the core raises
 * `Notice::PushDowngraded` and marks wake-ups downgraded when that happens too often.
 */
class FcmService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        Push.setFcmToken(this, token)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val priority = FcmPriority(
            sentHigh = message.originalPriority == RemoteMessage.PRIORITY_HIGH,
            deliveredHigh = message.priority == RemoteMessage.PRIORITY_HIGH,
        )
        // For Wake-ups: when it came, and whether FCM held it back below its sent priority.
        PhoneLog.recordWake(this, onTime = message.originalPriority == message.priority)
        // The relay sends the body's two fields, v and h, as the data message's map.
        val payload = PushBody.fromFcmData(message.data)
        Push.wake(PushPlatform.FCM, payload, priority)
    }
}

/** Firebase Messaging, when the build has a Firebase configuration (google-services.json). */
object Fcm {
    private const val TAG = "PingersFcm"

    /** Whether Firebase was configured at build time; without it FCM is off and push is UnifiedPush only. */
    fun available(context: Context): Boolean = FirebaseApp.getApps(context).isNotEmpty()

    /** Fetches the current token (FCM also calls [FcmService.onNewToken] when it changes). */
    fun refreshToken(context: Context) {
        if (!available(context)) {
            Log.i(TAG, "FCM not configured in this build")
            return
        }
        val app = context.applicationContext
        FirebaseMessaging.getInstance().token
            .addOnSuccessListener { Push.setFcmToken(app, it) }
            .addOnFailureListener { Log.w(TAG, "no FCM token: ${it.forLog()}") }
    }
}
