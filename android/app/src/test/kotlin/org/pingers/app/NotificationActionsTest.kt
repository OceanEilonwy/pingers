package org.pingers.app

import android.app.Application
import android.content.ComponentName
import android.content.pm.PackageManager
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class NotificationActionsTest {
    private val context: Application = ApplicationProvider.getApplicationContext()

    @After
    fun tearDown() {
        NotificationActions.take()
    }

    @Test
    fun onlyTheTrampolineTakesActionsAndOnlyItIsNotExported() {
        val packages = context.packageManager
        val trampoline = packages.getActivityInfo(ComponentName(context, NotificationTrampoline::class.java), PackageManager.GET_META_DATA)
        assertFalse(trampoline.exported)
        // MainActivity opens links from any app: it is exported, which is why it reads no action.
        val main = packages.getActivityInfo(ComponentName(context, MainActivity::class.java), PackageManager.GET_META_DATA)
        assertTrue(main.exported)
    }

    @Test
    fun theTrampolineLeavesTheActionAndOpensTheAppWithoutExtras() {
        for (action in NotificationAction.entries) {
            val pending = shadowOf(NotificationActions.pendingIntent(context, action, 7)).savedIntent
            assertEquals(NotificationTrampoline::class.java.name, pending.component?.className)
            val controller = Robolectric.buildActivity(NotificationTrampoline::class.java, pending).create()
            val started = shadowOf(controller.get()).nextStartedActivity
            assertEquals(MainActivity::class.java.name, started.component?.className)
            assertNull(started.extras)
            assertTrue(controller.get().isFinishing)
            assertEquals(action, NotificationActions.take())
            assertNull(NotificationActions.take())
        }
    }

    @Test
    fun actionsHaveDistinctPendingIntents() {
        val intents = NotificationAction.entries.map { shadowOf(NotificationActions.pendingIntent(context, it, 1)).savedIntent }
        assertEquals(NotificationAction.entries.size, intents.map { it.action }.toSet().size)
    }
}
