package org.pingers.app.platform

import android.content.Context
import androidx.work.ExistingWorkPolicy
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit

/**
 * Scheduled wakes through WorkManager. The core asks for one next wake at a time (cover board
 * sync, lease refresh); a new request replaces the pending one. WorkManager may run it late
 * under Doze, which the core tolerates: leases live for four buckets (design F.1).
 */
class Wakes(private val context: Context) {
    fun schedule(afterMillis: Long) {
        val request = OneTimeWorkRequestBuilder<WakeWorker>()
            .setInitialDelay(afterMillis.coerceAtLeast(0), TimeUnit.MILLISECONDS)
            .addTag(TAG)
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(UNIQUE, ExistingWorkPolicy.REPLACE, request)
    }

    fun cancel() {
        WorkManager.getInstance(context).cancelUniqueWork(UNIQUE)
    }

    companion object {
        const val UNIQUE = "pingers.wake"
        const val TAG = "pingers.wake"
    }
}

/** Runs one core wake on WorkManager's background thread. The core schedules the next one itself. */
class WakeWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    override fun doWork(): Result {
        PlatformHost.sink.onWake(WakeReason.SCHEDULED)
        return Result.success()
    }
}
