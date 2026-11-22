package org.pingers.app.platform

import android.content.Context
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/**
 * Scheduled wakes through WorkManager. The core asks for one next wake at a time (cover board
 * sync, lease refresh); a new request replaces the pending one. WorkManager may run it late
 * under Doze, which the core tolerates: leases live for four buckets (design F.1).
 */
class Wakes(private val context: Context) {
    fun schedule(afterMillis: Long) {
        // Asked from inside a running WakeWorker (every wake ends by scheduling the next):
        // replacing the unique work now would cancel that worker mid-wake, ending its job and
        // with it the time the OS grants. It enqueues the request itself once the wake is done.
        if (WakeWorker.running.get()) {
            WakeWorker.next.set(afterMillis)
            return
        }
        enqueue(afterMillis)
    }

    private fun enqueue(afterMillis: Long) {
        val request = OneTimeWorkRequestBuilder<WakeWorker>()
            .setInitialDelay(afterMillis.coerceAtLeast(0), TimeUnit.MILLISECONDS)
            .addTag(TAG)
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(UNIQUE, ExistingWorkPolicy.REPLACE, request)
    }

    fun cancel() {
        // A wake asked for by a running worker would otherwise be enqueued after it.
        WakeWorker.next.set(null)
        WorkManager.getInstance(context).cancelUniqueWork(UNIQUE)
    }

    companion object {
        const val UNIQUE = "pingers.wake"
        const val TAG = "pingers.wake"
    }
}

/**
 * Runs one core wake on WorkManager's background thread. The core schedules the next one
 * itself; that request is held while the wake runs and enqueued after, so it does not cancel
 * this worker partway (see [Wakes.schedule]).
 */
class WakeWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        next.set(null)
        running.set(true)
        try {
            PlatformHost.sink.onWake(WakeReason.SCHEDULED)
        } finally {
            running.set(false)
            // Replacing this, now finished, work is harmless: its result no longer matters.
            next.getAndSet(null)?.let { Wakes(applicationContext).schedule(it) }
        }
        return Result.success()
    }

    companion object {
        /** A WakeWorker is running its wake. */
        internal val running = AtomicBoolean(false)

        /** The next wake asked for while it ran, in milliseconds from then. */
        internal val next = AtomicReference<Long?>(null)
    }
}
