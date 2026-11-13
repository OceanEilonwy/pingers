package org.pingers.app.platform

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * FCM pushes (design B.7), in builds with `-PpingersFcm` only. A stub until the project has
 * google-services credentials: it stores the token for the core to seal and hands each push to
 * the same core entry point as UnifiedPush.
 *
 * Knocks and alerts are sent high priority. FCM downgrades high-priority messages from apps
 * that don't show a notification for them; a downgraded push is flagged so the core can count
 * them and raise `Notice::PushDowngraded` (design B.4.9).
 */
class FcmService : FirebaseMessagingService() {
    override fun onNewToken(token: String) {
        Push.setFcmToken(this, token)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        val downgraded = message.originalPriority == RemoteMessage.PRIORITY_HIGH &&
            message.priority != RemoteMessage.PRIORITY_HIGH
        // The relay sends the content-free body {"v":1,"h":handle} as the data field "p".
        val payload = message.data["p"]?.toByteArray() ?: message.rawData ?: ByteArray(0)
        Push.wake(this, PushPlatform.FCM, payload, downgraded)
    }
}
