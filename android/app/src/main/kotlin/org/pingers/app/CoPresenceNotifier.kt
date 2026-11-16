package org.pingers.app

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import org.pingers.app.data.CoPresenceAlert

/**
 * The co-presence alert as a real notification. In the finished app the push
 * arrives empty and the core decrypts the names and place on the phone
 * (Core.on_push); here the alert comes from the fake repository.
 */
@android.annotation.SuppressLint("InlinedApi") // POST_NOTIFICATIONS is only checked on Android 13 and up.
object CoPresenceNotifier {
    private const val CHANNEL = "meetups"
    private const val ID = 1

    /** Whether the runtime permission must be asked for first (Android 13 and up). */
    val needsPermission: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU

    fun canPost(context: Context): Boolean = if (needsPermission) {
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
    } else {
        NotificationManagerCompat.from(context).areNotificationsEnabled()
    }

    fun post(context: Context, alert: CoPresenceAlert) {
        if (!canPost(context)) return
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, context.getString(R.string.channel_meetups), NotificationManager.IMPORTANCE_HIGH).apply {
                description = context.getString(R.string.channel_meetups_description)
            },
        )
        val text = context.getString(R.string.alert_body, Words.names(context, alert.others), alert.spotName)
        val n = NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_pingers)
            .setColor(ContextCompat.getColor(context, R.color.blue))
            .setContentTitle(alert.partyName)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setCategory(NotificationCompat.CATEGORY_SOCIAL)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(NotificationActions.pendingIntent(context, NotificationAction.OPEN_MEETUP, 1))
            .addAction(0, context.getString(R.string.alert_open), NotificationActions.pendingIntent(context, NotificationAction.OPEN_MEETUP, 1))
            .addAction(0, context.getString(R.string.alert_hide), NotificationActions.pendingIntent(context, NotificationAction.HIDE_ME, 2))
            .build()
        try {
            NotificationManagerCompat.from(context).notify(ID, n)
        } catch (_: SecurityException) {
            // The permission was revoked between the check and the post.
        }
    }

    fun cancel(context: Context) = NotificationManagerCompat.from(context).cancel(ID)
}
