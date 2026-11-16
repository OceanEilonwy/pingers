package org.pingers.app.platform

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import org.pingers.app.NotificationAction
import org.pingers.app.NotificationActions
import org.pingers.app.R

/** The platform layer's notification channels, and posting what the core returns from a push. */
object Notifications {
    /** The persistent notification of the location foreground service. */
    const val CHANNEL_RUNNING = "pingers.running"

    /** Notifications the core asks for that should make a sound (knocks, Meetups, chat alerts). */
    const val CHANNEL_ALERTS = "pingers.alerts"

    /** Notifications the core asks for that only appear in the shade. */
    const val CHANNEL_QUIET = "pingers.quiet"

    const val RUNNING_ID = 0x5049_4E47 // "PING"

    /** You're in's notification ("Ana let you in"); one at a time, a later one replaces it. */
    const val LET_IN_ID = 0x4C45_5449 // "LETI"

    fun ensureChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannels(
            listOf(
                NotificationChannel(CHANNEL_RUNNING, context.getString(R.string.platform_channel_running), NotificationManager.IMPORTANCE_LOW).apply {
                    description = context.getString(R.string.platform_channel_running_description)
                    setShowBadge(false)
                    lockscreenVisibility = Notification.VISIBILITY_PRIVATE
                },
                NotificationChannel(CHANNEL_ALERTS, context.getString(R.string.platform_channel_alerts), NotificationManager.IMPORTANCE_HIGH).apply {
                    description = context.getString(R.string.platform_channel_alerts_description)
                    lockscreenVisibility = Notification.VISIBILITY_PRIVATE
                },
                NotificationChannel(CHANNEL_QUIET, context.getString(R.string.platform_channel_quiet), NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = context.getString(R.string.platform_channel_quiet_description)
                    setSound(null, null)
                    lockscreenVisibility = Notification.VISIBILITY_PRIVATE
                },
            ),
        )
    }

    /**
     * Hides [builder]'s notification on a locked screen: with [lockScreen] (the core's
     * "New activity in a Party"), a locked phone shows only that; without it, nothing at all
     * (chat messages and location asks). Every Pingers notification goes through here or sets
     * a public version itself.
     */
    internal fun lockScreen(context: Context, builder: NotificationCompat.Builder, channel: String, lockScreen: String?): NotificationCompat.Builder =
        if (lockScreen == null) {
            builder.setVisibility(NotificationCompat.VISIBILITY_SECRET)
        } else {
            builder.setVisibility(NotificationCompat.VISIBILITY_PRIVATE).setPublicVersion(
                NotificationCompat.Builder(context, channel)
                    .setSmallIcon(R.drawable.ic_stat_pingers)
                    .setColor(ContextCompat.getColor(context, R.color.blue))
                    .setContentTitle(lockScreen)
                    .build(),
            )
        }

    fun canPost(context: Context): Boolean =
        (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) &&
            NotificationManagerCompat.from(context).areNotificationsEnabled()

    /**
     * Posts what a wake led to (design B.7: Meetups, chat, requests; all alerts). Tapping a
     * Meetup notification opens the Meetup; a join request opens the app. Without the
     * permission, nothing is shown.
     */
    fun post(context: Context, notification: CoreNotification) {
        if (!canPost(context)) return
        ensureChannels(context)
        val tap = when (notification.kind) {
            NotificationKind.JOIN_REQUEST -> openApp(context)
            NotificationKind.MEETUP_STARTED, NotificationKind.CHAT_MESSAGE, NotificationKind.LOCATION_REQUEST -> openMeetup(context, notification.id)
        }
        val builder = NotificationCompat.Builder(context, CHANNEL_ALERTS)
            .setSmallIcon(R.drawable.ic_stat_pingers)
            .setColor(ContextCompat.getColor(context, R.color.blue))
            .setContentTitle(notification.title)
            .setContentText(notification.body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(notification.body))
            .setCategory(NotificationCompat.CATEGORY_SOCIAL)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(tap)
        val n = lockScreen(context, builder, CHANNEL_ALERTS, notification.lockScreen).build()
        try {
            NotificationManagerCompat.from(context).notify(notification.id, n)
        } catch (_: SecurityException) {
            // The permission was revoked between the check and the post.
        }
    }

    /**
     * This phone was let into a Party (`Events.let_in`): [title], [body] and what a locked phone
     * shows instead, [lockScreen], are the core's words. Tapping it opens the app on You're in.
     * Without the permission, nothing is shown.
     */
    fun letIn(context: Context, title: String, body: String, lockScreen: String) {
        if (!canPost(context)) return
        ensureChannels(context)
        val open = NotificationActions.pendingIntent(context, NotificationAction.OPEN_LET_IN, LET_IN_ID)
        val builder = NotificationCompat.Builder(context, CHANNEL_ALERTS)
            .setSmallIcon(R.drawable.ic_stat_pingers)
            .setColor(ContextCompat.getColor(context, R.color.blue))
            .setContentTitle(title)
            .setContentText(body)
            .setCategory(NotificationCompat.CATEGORY_SOCIAL)
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(open)
        val n = lockScreen(context, builder, CHANNEL_ALERTS, lockScreen).build()
        try {
            NotificationManagerCompat.from(context).notify(LET_IN_ID, n)
        } catch (_: SecurityException) {
            // The permission was revoked between the check and the post.
        }
    }

    fun cancelLetIn(context: Context) = NotificationManagerCompat.from(context).cancel(LET_IN_ID)

    /** The foreground service's notification; a locked phone shows only "Pingers is running". */
    fun running(context: Context, meetup: Boolean): Notification {
        ensureChannels(context)
        val builder = NotificationCompat.Builder(context, CHANNEL_RUNNING)
            .setSmallIcon(R.drawable.ic_stat_pingers)
            .setColor(ContextCompat.getColor(context, R.color.blue))
            .setContentTitle(context.getString(if (meetup) R.string.platform_running_meetup else R.string.platform_running_keep))
            .setContentText(context.getString(if (meetup) R.string.platform_running_meetup_text else R.string.platform_running_keep_text))
            .setOngoing(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setContentIntent(openApp(context))
        return lockScreen(context, builder, CHANNEL_RUNNING, context.getString(R.string.platform_running_keep)).build()
    }

    /** Opens the Meetup screen, as the co-presence alert's "Open meetup" does. */
    private fun openMeetup(context: Context, code: Int): PendingIntent =
        NotificationActions.pendingIntent(context, NotificationAction.OPEN_MEETUP, code)

    private fun openApp(context: Context): PendingIntent? {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName) ?: return null
        launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(context, 0, launch, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }
}
