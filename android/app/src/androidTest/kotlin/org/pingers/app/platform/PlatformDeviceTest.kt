package org.pingers.app.platform

import android.Manifest
import android.content.Context
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * The platform layer on a device or emulator. Two tests need the host:
 * - [geofenceFiresOnMockLocation]: the host sends `adb emu geo fix -0.1246 51.5007` while it runs.
 * - [unifiedPushWakesTheCore]: a UnifiedPush distributor (ntfy) must be installed; the test logs
 *   `PINGERS_ENDPOINT <url>` and the host POSTs a push to that URL.
 * Host script: android/platform-tests.sh.
 */
@RunWith(AndroidJUnit4::class)
class PlatformDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context = instrumentation.targetContext
    private val sink = RecordingSink()

    @Before
    fun setUp() {
        for (p in listOf(
            Manifest.permission.ACCESS_FINE_LOCATION,
            Manifest.permission.ACCESS_COARSE_LOCATION,
            Manifest.permission.ACCESS_BACKGROUND_LOCATION,
            Manifest.permission.POST_NOTIFICATIONS,
        )) {
            instrumentation.uiAutomation.grantRuntimePermission(context.packageName, p)
        }
        PlatformHost.sinkProvider = { sink }
    }

    @After
    fun tearDown() {
        PlatformHost.sinkProvider = { LoggingSink }
    }

    private fun shell(command: String): String {
        val pfd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(pfd).use { String(it.readBytes()) }
    }

    @Test
    fun secureStoreWrapsWithTheKeystore() {
        val store = SecureStore(context)
        val key = ByteArray(32) { (it * 7).toByte() }
        store.put(SecureStoreKey.DATABASE, key)
        assertArrayEquals(key, SecureStore(context).get(SecureStoreKey.DATABASE))
        val raw = context.getSharedPreferences(SecureStore.PREFS, Context.MODE_PRIVATE).getString(SecureStoreKey.DATABASE.name, null)
        assertNotNull(raw)
        assertTrue("stored in the clear", !raw!!.contains(android.util.Base64.encodeToString(key, android.util.Base64.NO_WRAP)))
    }

    @Test
    fun interruptionFilterChangeIsSeen() {
        val focus = Focus(context)
        focus.watch { sink.onFocusChanged(it) }
        try {
            shell("cmd notification set_dnd priority")
            assertEquals(RecordingSink.Focused(FocusState.ON), sink.await<RecordingSink.Focused>(10))
            assertEquals(FocusState.ON, focus.current())
            shell("cmd notification set_dnd off")
            assertEquals(RecordingSink.Focused(FocusState.OFF), sink.await<RecordingSink.Focused>(10))
            assertEquals(FocusState.OFF, focus.current())
        } finally {
            shell("cmd notification set_dnd off")
            focus.stop()
        }
    }

    @Test
    fun foregroundServiceStartsAndStops() {
        val modes = AndroidPlatform.get(context).modes
        modes.set(LocationMode.MEETUP_BACKGROUND)
        assertTrue("service did not reach the foreground", waitFor(10) { LocationService.running })
        assertTrue(shell("dumpsys activity services ${context.packageName}").contains("isForeground=true"))
        modes.set(LocationMode.IDLE)
        assertTrue("service did not stop", waitFor(10) { !LocationService.running })

        // Stopped before it reached startForeground: Android kills an app whose service does
        // that, so the stop must wait for the start.
        modes.set(LocationMode.MEETUP_BACKGROUND)
        modes.set(LocationMode.IDLE)
        Thread.sleep(2_000)
        assertTrue("service still running", waitFor(10) { !LocationService.running })
    }

    @Test
    fun geofenceFiresOnMockLocation() {
        // Westminster Bridge; the host holds the emulator's GPS there.
        val geofences = Geofences(context, SecureStore(context), responsivenessMillis = 0)
        // A fresh id each run, so Play services can't match it to an earlier registration.
        val id = "spot-bridge-${System.currentTimeMillis()}"
        geofences.replaceAll(listOf(OsRegion(id, 51.5007, -0.1246, 200f)))
        Log.i("PingersTest", "PINGERS_GEOFENCE_READY")
        // On an idle emulator Play services' geofencer may not compute a location for many
        // minutes; an app holding location updates (as in a Meetup) makes it evaluate at once.
        val modes = AndroidPlatform.get(context).modes
        modes.set(LocationMode.MEETUP_FOREGROUND)
        try {
            val event = sink.await<RecordingSink.Geofenced>(300)
            assertNotNull("no geofence transition reached the sink", event)
            assertEquals(listOf(id), event!!.ids)
            assertTrue(event.transition == GeofenceTransition.ENTER || event.transition == GeofenceTransition.DWELL)
            Log.i("PingersTest", "PINGERS_GEOFENCE ${event.transition}")
        } finally {
            modes.set(LocationMode.IDLE)
            geofences.replaceAll(emptyList())
        }
    }

    @Test
    fun unifiedPushWakesTheCore() {
        assumeTrue("no UnifiedPush distributor installed", Push.distributors(context).isNotEmpty())
        context.getSharedPreferences(Push.PREFS, Context.MODE_PRIVATE).edit().clear().commit()
        assertTrue(Push.registerUnifiedPush(context, Push.distributors(context).first()))
        val tokens = sink.await<RecordingSink.Tokens>(60)?.tokens
        assertNotNull("no endpoint from the distributor", tokens)
        assertEquals(PushPlatform.UNIFIED_PUSH, tokens!!.platform)
        Log.i("PingersTest", "PINGERS_ENDPOINT ${String(tokens.token)}")
        val push = sink.await<RecordingSink.Pushed>(120)
        assertNotNull("no push reached the core", push)
        assertEquals("""{"v":1,"h":"AQIDBAUGBwg="}""", push!!.payload)
        // UnifiedPush has no priorities to report.
        assertEquals(null, push.priority)
        Log.i("PingersTest", "PINGERS_PUSH_OK")
    }

    @Test
    fun aNotificationTheCoreWroteIsPostedAndOpensTheMeetup() {
        val manager = context.getSystemService(android.app.NotificationManager::class.java)
        Notifications.post(context, CoreNotification(42, "Climbing crew", "Ben is at the wall too. Say hi?", NotificationKind.MEETUP_STARTED))
        assertTrue(waitFor(5) { manager.activeNotifications.any { it.id == 42 } })
        val posted = manager.activeNotifications.first { it.id == 42 }
        assertEquals(Notifications.CHANNEL_ALERTS, posted.notification.channelId)
        manager.cancel(42)
    }

    @Test
    fun fcmTokenIsObtained() {
        assumeTrue("FCM not configured in this build", FlavorServices.fcmAvailable(context))
        context.getSharedPreferences(Push.PREFS, Context.MODE_PRIVATE).edit().clear().commit()
        FlavorServices.startPush(context)
        val tokens = sink.await<RecordingSink.Tokens>(60)?.tokens
        assertNotNull("no FCM token", tokens)
        assertEquals(PushPlatform.FCM, tokens!!.platform)
        assertTrue(tokens.token.size > 100)
        // Its length only: the token itself is a push target and stays out of logs.
        Log.i("PingersTest", "PINGERS_FCM_TOKEN ${tokens.token.size} bytes")
    }

    private fun waitFor(seconds: Long, condition: () -> Boolean): Boolean {
        val deadline = System.currentTimeMillis() + seconds * 1000
        while (System.currentTimeMillis() < deadline) {
            if (condition()) return true
            Thread.sleep(100)
        }
        return condition()
    }
}
