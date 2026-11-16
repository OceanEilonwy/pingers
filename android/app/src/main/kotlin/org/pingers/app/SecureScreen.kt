package org.pingers.app

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.view.Window
import android.view.WindowManager
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.platform.LocalContext

/**
 * Keeps screenshots, screen recording and the recent-apps thumbnail away from screens that show
 * a secret or a location: the invite link and QR code (a bearer capability), the safety codes,
 * and the Meetup map (`FLAG_SECURE`). Counted, so that two such screens overlapping during a
 * transition do not clear the flag when the first one leaves.
 */
object SecureWindow {
    private val holds = mutableMapOf<Window, Int>()

    /** Sets `FLAG_SECURE` on [window] until the returned release runs (once). */
    fun hold(window: Window): () -> Unit {
        val count = holds.getOrDefault(window, 0) + 1
        holds[window] = count
        if (count == 1) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        var released = false
        return release@{
            if (released) return@release
            released = true
            val left = holds.getOrDefault(window, 1) - 1
            if (left <= 0) {
                holds.remove(window)
                window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
            } else {
                holds[window] = left
            }
        }
    }
}

/** While this is in the composition, the activity's window is secure ([SecureWindow]). */
@Composable
fun SecureScreen() {
    val context = LocalContext.current
    DisposableEffect(context) {
        val window = context.activity()?.window
        val release = window?.let(SecureWindow::hold)
        onDispose { release?.invoke() }
    }
}

private tailrec fun Context.activity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.activity()
    else -> null
}
