package org.pingers.app.data

import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import java.io.File

/**
 * The process's one [CoreRepository]: the Rust core is opened once per process, on one
 * database, and outlives activities (a rotation must not open it again).
 */
object CoreRepositories {
    @Volatile
    private var instance: CoreRepository? = null

    fun get(context: Context): CoreRepository = instance ?: synchronized(this) {
        instance ?: CoreRepository(
            res = context.resources,
            scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate),
            dbPath = File(context.noBackupFilesDir, "pingers.db").absolutePath,
            platform = InterimPlatform(),
        ).also { instance = it }
    }
}
