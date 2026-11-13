package org.pingers.app.data

import android.Manifest
import android.content.Context
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.pingers.app.platform.AndroidPlatform
import org.pingers.app.platform.LocationMode
import org.pingers.app.platform.OsRegion
import org.pingers.app.platform.PlatformHost

/**
 * The integration on a device: PingersApp points the platform's sink at [CoreHost], so an OS
 * geofence transition reaches the process's core. The host holds the emulator's GPS inside the
 * region (`adb emu geo fix -0.1246 51.5007`, see android/platform-tests.sh). The outcome is read
 * from CoreHost's `PingersCore` logcat markers, which carry no ids or coordinates.
 */
@RunWith(AndroidJUnit4::class)
class CoreHostDeviceTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context: Context = instrumentation.targetContext

    @Before
    fun setUp() {
        for (p in listOf(
            Manifest.permission.ACCESS_FINE_LOCATION,
            Manifest.permission.ACCESS_COARSE_LOCATION,
            Manifest.permission.ACCESS_BACKGROUND_LOCATION,
        )) {
            instrumentation.uiAutomation.grantRuntimePermission(context.packageName, p)
        }
    }

    private fun shell(command: String): String {
        val pfd = instrumentation.uiAutomation.executeShellCommand(command)
        return ParcelFileDescriptor.AutoCloseInputStream(pfd).use { String(it.readBytes()) }
    }

    @Test
    fun sinkIsTheCoreHost() {
        assertTrue(PlatformHost.sink.javaClass.name.startsWith(CoreHost::class.java.name))
    }

    @Test
    fun geofenceReachesTheCore() {
        shell("logcat -c")
        val platform = AndroidPlatform.get(context)
        platform.setOsGeofences(listOf(OsRegion("core-host-${System.currentTimeMillis()}", 51.5007, -0.1246, 200f)))
        Log.i("PingersTest", "PINGERS_GEOFENCE_READY")
        // Location updates make Play services evaluate geofences at once (see PlatformDeviceTest).
        platform.modes.set(LocationMode.MEETUP_FOREGROUND)
        try {
            val deadline = System.currentTimeMillis() + 300_000
            var line: String? = null
            while (line == null && System.currentTimeMillis() < deadline) {
                line = shell("logcat -d -s PingersCore").lines().firstOrNull { "onGeofence ENTER" in it || "onGeofence DWELL" in it }
                if (line == null) Thread.sleep(2_000)
            }
            assertTrue("no geofence transition reached CoreHost", line != null)
            Log.i("PingersTest", "PINGERS_CORE_GEOFENCE ${line!!.substringAfter("PingersCore: ")}")
        } finally {
            platform.modes.set(LocationMode.IDLE)
            platform.setOsGeofences(emptyList())
        }
    }
}
