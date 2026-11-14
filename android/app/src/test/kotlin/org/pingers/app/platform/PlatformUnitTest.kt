package org.pingers.app.platform

import android.app.Application
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkInfo
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.pingers.app.platform.onboarding.PermissionState
import org.pingers.app.platform.onboarding.PermissionStep
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import javax.crypto.KeyGenerator

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PlatformUnitTest {
    private val context: Application = ApplicationProvider.getApplicationContext()
    private val sink = RecordingSink()

    @Before
    fun setUp() {
        PlatformHost.sinkProvider = { sink }
    }

    @After
    fun tearDown() {
        PlatformHost.sinkProvider = { LoggingSink }
    }

    /** Robolectric has no AndroidKeyStore: a software AES key stands in for it. */
    private fun store(): SecureStore {
        val key = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
        return SecureStore(context.getSharedPreferences("test-store", Context.MODE_PRIVATE), WrappingKey { key })
    }

    @Test
    fun secureStoreKeepsOnlyWrappedValues() {
        val store = store()
        val dbKey = ByteArray(32) { it.toByte() }
        assertNull(store.get(SecureStoreKey.DATABASE))
        store.put(SecureStoreKey.DATABASE, dbKey)
        assertArrayEquals(dbKey, store.get(SecureStoreKey.DATABASE))

        val raw = context.getSharedPreferences("test-store", Context.MODE_PRIVATE).getString(SecureStoreKey.DATABASE.name, null)!!
        // iv 12 + 32 + tag 16, base64.
        assertEquals(60, android.util.Base64.decode(raw, android.util.Base64.NO_WRAP).size)
    }

    @Test
    fun secureStoreRejectsAValueMovedToAnotherName() {
        val store = store()
        store.put(SecureStoreKey.DATABASE, ByteArray(32))
        val prefs = context.getSharedPreferences("test-store", Context.MODE_PRIVATE)
        prefs.edit().putString("other", prefs.getString(SecureStoreKey.DATABASE.name, null)).commit()
        val e = assertThrows(PlatformException::class.java) { store.get(SecureStoreKey("other")) }
        assertEquals(PlatformError.SECURE_STORE_UNAVAILABLE, e.error)
    }

    @Test
    fun interruptionFilterMapsToFocusState() {
        assertEquals(FocusState.OFF, Focus.map(NotificationManager.INTERRUPTION_FILTER_ALL))
        assertEquals(FocusState.ON, Focus.map(NotificationManager.INTERRUPTION_FILTER_PRIORITY))
        assertEquals(FocusState.ON, Focus.map(NotificationManager.INTERRUPTION_FILTER_ALARMS))
        assertEquals(FocusState.ON, Focus.map(NotificationManager.INTERRUPTION_FILTER_NONE))
        assertEquals(FocusState.UNKNOWN, Focus.map(NotificationManager.INTERRUPTION_FILTER_UNKNOWN))
    }

    @Test
    fun focusChangeReachesTheSink() {
        val focus = Focus(context)
        focus.watch { sink.onFocusChanged(it) }
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_PRIORITY)
        context.sendBroadcast(Intent(NotificationManager.ACTION_INTERRUPTION_FILTER_CHANGED).setPackage(context.packageName))
        shadowOf(android.os.Looper.getMainLooper()).idle()
        assertEquals(RecordingSink.Focused(FocusState.ON), sink.await<RecordingSink.Focused>())
        assertEquals(FocusState.ON, focus.current())
        focus.stop()
    }

    @Test
    fun onboardingAsksInOrder() {
        val none = PermissionState(fineLocation = false, backgroundLocation = false, notifications = false)
        assertEquals(PermissionStep.FINE_LOCATION, none.next())
        assertEquals(PermissionStep.BACKGROUND_LOCATION, none.copy(fineLocation = true).next())
        assertEquals(PermissionStep.NOTIFICATIONS, none.copy(fineLocation = true, backgroundLocation = true).next())
        assertNull(PermissionState(fineLocation = true, backgroundLocation = true, notifications = true).next())
        // Background location is never asked for before fine location.
        assertEquals(PermissionStep.NOTIFICATIONS, none.copy(skipped = setOf(PermissionStep.FINE_LOCATION)).next())
        assertNull(none.copy(skipped = PermissionStep.entries.toSet()).next())
    }

    @Test
    fun geofencesRoundTripForRestore() {
        val regions = listOf(OsRegion("spot-1", 51.5, -0.12, 150f), OsRegion("horizon", 51.6, -0.1, 4200f))
        assertEquals(regions, Geofences.decode(Geofences.encode(regions)))
        assertEquals(emptyList<OsRegion>(), Geofences.decode("not json"))
    }

    @Test
    fun meetupModesFollowContinuously() {
        assertNull(ContinuousRequest.of(LocationMode.IDLE))
        assertNull(ContinuousRequest.of(LocationMode.DWELL))
        val fg = ContinuousRequest.of(LocationMode.MEETUP_FOREGROUND)!!
        assertEquals(1_000L, fg.intervalMillis)
        assertEquals(true, fg.highAccuracy)
        assertEquals(20f, ContinuousRequest.of(LocationMode.MEETUP_BACKGROUND)!!.minDistanceMeters)
    }

    @Test
    fun scheduledWakeRunsTheCore() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        Wakes(context).schedule(15 * 60_000L)
        val manager = WorkManager.getInstance(context)
        val info = manager.getWorkInfosForUniqueWork(Wakes.UNIQUE).get().single()
        assertEquals(WorkInfo.State.ENQUEUED, info.state)
        WorkManagerTestInitHelper.getTestDriver(context)!!.setInitialDelayMet(info.id)
        assertEquals(RecordingSink.Woken(WakeReason.SCHEDULED), sink.await<RecordingSink.Woken>())
    }

    @Test
    fun aDistributorThatIsGoneTakesTheEndpointWithItAtTheNextStart() {
        context.getSharedPreferences(Push.PREFS, Context.MODE_PRIVATE).edit().clear().commit()
        Push.setEndpoint(context, "http://10.0.2.2:8099/upAbc")
        assertEquals(PushPlatform.UNIFIED_PUSH, sink.await<RecordingSink.Tokens>()!!.tokens!!.platform)
        // No distributor is installed under Robolectric: as after an uninstall.
        Push.renew(context)
        assertNull(Push.tokens(context))
        assertNull(sink.await<RecordingSink.Tokens>()!!.tokens)
    }

    @Test
    fun aPushReachesTheCoreWithItsFcmPriority() {
        val priority = FcmPriority(sentHigh = true, deliveredHigh = false)
        Push.wake(PushPlatform.FCM, """{"v":1,"h":"0000000000000000"}""".toByteArray(), priority)
        val pushed = sink.await<RecordingSink.Pushed>()!!
        assertEquals("""{"v":1,"h":"0000000000000000"}""", pushed.payload)
        assertEquals(priority, pushed.priority)
    }

    @Test
    fun theRelayBodyGivesItsHandleWhetherItCameByUnifiedPushOrFcm() {
        val handle = byteArrayOf(1, 2, 3, 4, 5, 6, 7, -1)
        // The push relay writes the handle as 16 lowercase hex digits (pingers-push-relay).
        assertArrayEquals(handle, PushBody.handle("""{"v":1,"h":"01020304050607ff"}""".toByteArray()))
        val fromFcm = PushBody.fromFcmData(mapOf("v" to "1", "h" to "01020304050607ff"))
        assertArrayEquals(handle, PushBody.handle(fromFcm))
        assertArrayEquals(handle, PushBody.handle("""{"v":1,"h":"01020304050607FF"}""".toByteArray()))
        for (body in listOf(
            """{"v":2,"h":"01020304050607ff"}""",
            """{"v":1,"h":"010203040506"}""",
            """{"v":1,"h":"01020304050607zz"}""",
            """{"v":1,"h":"AQIDBAUGBwg="}""",
            """{"v":1}""",
            "not json",
        )) {
            assertNull(body, PushBody.handle(body.toByteArray()))
        }
        assertNull(PushBody.handle(PushBody.fromFcmData(emptyMap())))
    }

    @Test
    fun coreNotificationsGoToTheAlertsChannelAndReplaceByTheirId() {
        shadowOf(context).grantPermissions(android.Manifest.permission.POST_NOTIFICATIONS)
        Notifications.post(context, CoreNotification(7, "Ben · The wall", "Coffee after?", NotificationKind.CHAT_MESSAGE))
        Notifications.post(context, CoreNotification(7, "Chloé · The wall", "Yes please", NotificationKind.CHAT_MESSAGE))
        val manager = shadowOf(context.getSystemService(NotificationManager::class.java))
        assertEquals(1, manager.allNotifications.size)
        val posted = manager.getNotification(7)
        assertEquals(Notifications.CHANNEL_ALERTS, posted.channelId)
        assertEquals("Chloé · The wall", shadowOf(posted).contentTitle.toString())
    }
}
