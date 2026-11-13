package org.pingers.app.platform

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import androidx.core.content.ContextCompat

/**
 * Do Not Disturb (design B.10): `getCurrentInterruptionFilter() != INTERRUPTION_FILTER_ALL`,
 * which needs no permission and covers Android 15 Modes. `ACTION_INTERRUPTION_FILTER_CHANGED`
 * reaches runtime-registered receivers only, so [watch] lasts as long as the process; the core
 * reads [current] at every wake and knock as well.
 */
class Focus(private val context: Context) {
    private val manager = context.getSystemService(NotificationManager::class.java)
    private var receiver: BroadcastReceiver? = null

    fun current(): FocusState = map(manager.currentInterruptionFilter)

    /** Calls [onChange] with each new state until [stop]. */
    @Synchronized
    fun watch(onChange: (FocusState) -> Unit) {
        stop()
        val r = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                if (intent.action != NotificationManager.ACTION_INTERRUPTION_FILTER_CHANGED) return
                val state = current()
                runAsync { onChange(state) }
            }
        }
        // A protected system broadcast: no other app can send it, so the receiver need not be exported.
        ContextCompat.registerReceiver(
            context,
            r,
            IntentFilter(NotificationManager.ACTION_INTERRUPTION_FILTER_CHANGED),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        receiver = r
    }

    @Synchronized
    fun stop() {
        receiver?.let { context.unregisterReceiver(it) }
        receiver = null
    }

    companion object {
        fun map(filter: Int): FocusState = when (filter) {
            NotificationManager.INTERRUPTION_FILTER_ALL -> FocusState.OFF
            NotificationManager.INTERRUPTION_FILTER_PRIORITY,
            NotificationManager.INTERRUPTION_FILTER_ALARMS,
            NotificationManager.INTERRUPTION_FILTER_NONE,
            -> FocusState.ON
            else -> FocusState.UNKNOWN
        }
    }
}
