package org.pingers.app

import android.app.Activity
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Bundle

/** What tapping a Pingers notification, or one of its buttons, asks the app to do. */
enum class NotificationAction(internal val key: String) {
    /** The co-presence alert, or a Meetup notification: open the Meetup. */
    OPEN_MEETUP("open-meetup"),

    /** The co-presence alert's "Hide me here". */
    HIDE_ME("hide-me"),

    /** You're in ("Ana let you in"). */
    OPEN_LET_IN("let-in"),
    ;

    internal companion object {
        fun of(key: String?): NotificationAction? = entries.firstOrNull { it.key == key }
    }
}

/**
 * Notification actions reach [MainActivity] only through [NotificationTrampoline], which is not
 * exported: a notification's PendingIntent starts the trampoline, the trampoline leaves the
 * action here, in this process, and brings MainActivity forward with no extras. MainActivity is
 * exported and opens links from any app, so it never acts on an extra: another app could put
 * one in an intent and, say, hide this phone from a Meetup.
 */
object NotificationActions {
    internal const val EXTRA = "org.pingers.app.NOTIFICATION_ACTION"

    private var pending: NotificationAction? = null

    /** Leaves [action] for MainActivity; a later one replaces one not yet taken. */
    @Synchronized
    internal fun offer(action: NotificationAction) {
        pending = action
    }

    /** The action a notification left, once. */
    @Synchronized
    fun take(): NotificationAction? = pending.also { pending = null }

    /**
     * The PendingIntent for [action]. Each action is its own intent action, so PendingIntents of
     * different actions never replace each other; [code] keeps those of one action apart.
     */
    fun pendingIntent(context: Context, action: NotificationAction, code: Int): PendingIntent = PendingIntent.getActivity(
        context,
        code,
        Intent(context, NotificationTrampoline::class.java)
            .setAction("org.pingers.app.notification.${action.key}")
            .putExtra(EXTRA, action.key),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
}

/**
 * The non-exported activity every notification action goes through (see [NotificationActions]).
 * It shows nothing and finishes at once.
 */
class NotificationTrampoline : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val action = NotificationAction.of(intent.getStringExtra(NotificationActions.EXTRA))
        if (action != null) {
            NotificationActions.offer(action)
            if (action == NotificationAction.OPEN_MEETUP || action == NotificationAction.HIDE_ME) {
                CoPresenceNotifier.cancel(this)
            }
        }
        startActivity(
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP),
        )
        finish()
    }
}
