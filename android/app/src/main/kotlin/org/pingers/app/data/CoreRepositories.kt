package org.pingers.app.data

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob

/**
 * The process's one [CoreRepository]. It outlives activities (a rotation must not build it
 * again); the core under it is [CoreHost]'s, shared with receivers, services and workers.
 */
object CoreRepositories {
    @Volatile
    private var instance: CoreRepository? = null

    fun get(context: Context): CoreRepository = instance ?: synchronized(this) {
        instance ?: CoreRepository(
            context = context.applicationContext,
            scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate),
        ).also { instance = it }
    }
}
