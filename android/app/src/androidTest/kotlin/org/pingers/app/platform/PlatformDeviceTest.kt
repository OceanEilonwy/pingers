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
    private val sink = RecordingSink(CoreNotification(42, "Climbing", "Someone is at the wall", alert = true))

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
    }

    @Test
    fun geofenceFiresOnMockLocation() {
        // Westminster Bridge; the host holds the emulator's GPS there.
        val geofences = Geofences(context, SecureStore(context), responsivenessMillis = 0)
        geofences.replaceAll(listOf(OsRegion("spot-bridge", 51.5007, -0.1246, 200f)))
        Log.i("PingersTest", "PINGERS_GEOFENCE_READY")
        try {
            val event = sink.await<RecordingSink.Geofenced>(240)
            assertNotNull("no geofence transition reached the sink", event)
            assertEquals(listOf("spot-bridge"), event!!.ids)
            assertTrue(event.transition == GeofenceTransition.ENTER || event.transition == GeofenceTransition.DWELL)
            Log.i("PingersTest", "PINGERS_GEOFENCE ${event.transition}")
        } finally {
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
        // The notification the core returned is posted.
        val manager = context.getSystemService(android.app.NotificationManager::class.java)
        assertTrue(waitFor(5) { manager.activeNotifications.any { it.id == 42 } })
        Log.i("PingersTest", "PINGERS_PUSH_OK")
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
