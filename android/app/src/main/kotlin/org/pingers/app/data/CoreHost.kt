package org.pingers.app.data

import android.content.Context
import android.os.SystemClock
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.pingers.app.BuildConfig
import org.pingers.app.platform.AndroidPlatform
import org.pingers.app.platform.CoreNotification
import org.pingers.app.platform.CoreSink
import org.pingers.app.platform.FcmPriority
import org.pingers.app.platform.Fix
import org.pingers.app.platform.GeofenceTransition
import org.pingers.app.platform.NotificationKind
import org.pingers.app.platform.Notifications
import org.pingers.app.platform.PlatformException
import org.pingers.app.platform.PlatformHost
import org.pingers.app.platform.PushBody
import org.pingers.app.platform.PushPlatform
import org.pingers.app.platform.PushTokens
import org.pingers.app.platform.SecureStoreKey
import org.pingers.app.platform.WakeReason
import org.pingers.app.platform.FocusState as ShellFocusState
import uniffi.pingers.Coordinate
import uniffi.pingers.Core
import uniffi.pingers.CoreException
import uniffi.pingers.Events
import uniffi.pingers.InstanceConfig
import uniffi.pingers.GeofenceEvent
import java.io.File
import java.security.SecureRandom
import java.util.concurrent.CopyOnWriteArraySet
import uniffi.pingers.Fix as CoreFix

/**
 * The process's one Rust [Core], opened on first use by whoever needs it first: the UI
 * ([CoreRepository]) or a receiver, service or worker the OS started the process for (a
 * geofence, a push, a scheduled wake, a boot). [install] runs in `Application.onCreate`, so
 * [PlatformHost.sink] reaches this core from every entry point.
 *
 * The core gets one [Events] at open; [listen] fans its events out to whoever is interested
 * (the repository, while it exists).
 *
 * Logcat markers (tag `PingersCore`) name each entry point and its outcome, never a
 * coordinate, region id, token or payload.
 */
object CoreHost {
    private const val TAG = "PingersCore"
    private const val DB_NAME = "pingers.db"
    private const val KEY_BYTES = 32

    /** How long a failed open is remembered before the next caller tries again. */
    private const val RETRY_AFTER_MILLIS = 60_000L

    /** A sink call's budget: a receiver's goAsync window is about ten seconds. */
    private const val SINK_TIMEOUT_MILLIS = 9_000L

    private lateinit var app: Context
    private val mutex = Mutex()

    @Volatile
    private var core: Core? = null

    @Volatile
    private var failedAt = 0L

    /** Why the last open failed, for the UI. */
    @Volatile
    var openError: String? = null
        private set

    private val listeners = CopyOnWriteArraySet<Events>()

    /** Called once from `Application.onCreate`. Opens nothing: the core opens on first use. */
    fun install(context: Context) {
        app = context.applicationContext
        // The platform starts Focus watching and push registration (FCM token) with the process.
        AndroidPlatform.get(app)
        PlatformHost.sinkProvider = { Sink }
    }

    fun listen(events: Events) = listeners.add(events)

    fun unlisten(events: Events) = listeners.remove(events)

    /** The core, opening it if need be; null if it does not open (then [openError] says why). */
    suspend fun core(): Core? {
        core?.let { return it }
        // Off the caller's thread: the Keystore and the database file are slow on first run.
        return withContext(Dispatchers.IO) {
            mutex.withLock { core ?: open() }
        }
    }

    private suspend fun open(): Core? {
        if (failedAt != 0L && SystemClock.elapsedRealtime() - failedAt < RETRY_AFTER_MILLIS) return null
        val platform = AndroidPlatform.get(app)
        val dbPath = File(app.noBackupFilesDir, DB_NAME)
        return try {
            ensureDatabaseKey(platform, dbPath)
            Core.open(dbPath.absolutePath, instance(), FfiPlatform(platform), Fanout).also {
                core = it
                openError = null
                failedAt = 0L
                Log.i(TAG, "core opened")
            }
        } catch (e: CoreException) {
            fail("core did not open: ${e.message}")
        } catch (e: PlatformException) {
            fail("secure store unavailable: ${e.error}")
        }
    }

    /**
     * The instance from the build configuration (see app/build.gradle.kts). Blank pins mean a
     * development build without them (a release cannot be built so): the core pins nothing on
     * first use, so such a build reaches its server only once a setup link or invite has
     * recorded that server's pins, and its push relay gets no token. The test CA is passed in debug builds
     * only; a release build's EXTRA_CA is always empty as well. So is the first owner's
     * registration code (DEV_REGISTRATION_CODE).
     */
    private fun instance() = InstanceConfig(
        server = BuildConfig.SERVER_HOST.trimEnd('/'),
        ohttpRelay = BuildConfig.OHTTP_RELAY.trimEnd('/'),
        pushRelay = BuildConfig.PUSH_RELAY.trimEnd('/'),
        gatewayKeyPins = BuildConfig.GATEWAY_KEY_PINS.ifBlank { null },
        pushRelayKeyPins = BuildConfig.PUSH_RELAY_KEY_PINS.ifBlank { null },
        mapHostKeyPins = BuildConfig.MAP_HOST_KEY_PINS.ifBlank { null },
        extraCaPem = BuildConfig.EXTRA_CA.takeIf { BuildConfig.DEBUG && it.isNotBlank() },
        registrationCode =
            BuildConfig.DEV_REGISTRATION_CODE.takeIf { BuildConfig.DEBUG && it.isNotBlank() },
    )

    /**
     * The map host the user's Pingers server advertises (`map_host` in `/v1/params`), for the
     * map host chain; null until the core is open and has read it.
     */
    fun serverMapHost(): String? = core?.snapshot()?.serverMapHost()

    private fun fail(why: String): Core? {
        Log.w(TAG, why)
        openError = why
        failedAt = SystemClock.elapsedRealtime()
        return null
    }

    /**
     * pingers-client opens the database with the key the secure store holds and never makes
     * one, so the shell makes it on first run. A database without a key (left by an earlier
     * build that kept its key in memory) can never be read again, so it goes.
     */
    private fun ensureDatabaseKey(platform: AndroidPlatform, dbPath: File) {
        if (platform.secureStoreGet(SecureStoreKey.DATABASE) != null) return
        if (dbPath.exists()) {
            Log.i(TAG, "database without a key: starting a new one")
            dbPath.parentFile?.listFiles { f -> f.name.startsWith(DB_NAME) }?.forEach { it.delete() }
        }
        val key = ByteArray(KEY_BYTES).also(SecureRandom()::nextBytes)
        try {
            platform.secureStorePut(SecureStoreKey.DATABASE, key)
        } finally {
            key.fill(0)
        }
    }

    /** Runs one core entry point for a background caller, blocking it until done or out of time. */
    private fun blocking(what: String, block: suspend (Core) -> Unit) = runBlocking(Dispatchers.IO) {
        val done = withTimeoutOrNull(SINK_TIMEOUT_MILLIS) {
            val c = core()
            when {
                c == null -> Log.i(TAG, "$what: core not open")
                else -> try {
                    block(c)
                    Log.i(TAG, "$what: ok")
                } catch (e: CoreException) {
                    // The variant only (Unavailable, Network, ...): its message may carry more.
                    Log.i(TAG, "$what: ${e::class.simpleName}")
                }
            }
        }
        if (done == null) Log.w(TAG, "$what: timed out")
    }

    /** The shell's entry points, onto the core. */
    private object Sink : CoreSink {
        override fun onFix(fix: Fix?) {
            if (fix == null) {
                Log.i(TAG, "onFix: none in time")
                return
            }
            blocking("onFix") { it.onLocation(fix.toCore()) }
        }

        override fun onGeofence(regionIds: List<String>, transition: GeofenceTransition, location: Fix?) {
            val event = when (transition) {
                GeofenceTransition.ENTER -> GeofenceEvent.ENTER
                GeofenceTransition.DWELL -> GeofenceEvent.DWELL
                GeofenceTransition.EXIT -> GeofenceEvent.EXIT
            }
            blocking("onGeofence $transition ${regionIds.size} region(s)") { core ->
                // The fix that triggered them first, so the pipeline need not ask for another.
                location?.let { core.onLocation(it.toCore()) }
                regionIds.forEach { core.onOsGeofence(it, event) }
            }
        }

        override fun onPush(platform: PushPlatform, payload: ByteArray, priority: FcmPriority?) {
            val handle = PushBody.handle(payload)
            if (handle == null) {
                Log.i(TAG, "onPush $platform: not a Pingers push")
                return
            }
            blocking("onPush $platform") { core ->
                priority?.let { core.reportPushPriority(it.sentHigh.toPriority(), it.deliveredHigh.toPriority()) }
                // What the wake leads to comes back through Events.notify (Fanout) before this returns.
                core.onPush(handle)
            }
        }

        override fun onPushTokensChanged(tokens: PushTokens?) {
            // The core reads the tokens back through Platform.pushTokens().
            blocking("onPushTokensChanged ${tokens?.platform ?: "none"}") { it.pushTokensChanged() }
        }

        override fun onFocusChanged(focus: ShellFocusState) {
            // The core reads the state back through Platform.focusState().
            blocking("onFocusChanged") { it.onDndChanged() }
        }

        override fun onWake(reason: WakeReason) {
            blocking("onWake $reason") { it.onWake() }
        }
    }

    /** Whether the app is on screen: a chat message shown in the app needs no notification. */
    @Volatile
    private var visible = false

    /** The app came on screen or left it. */
    fun onForeground(visible: Boolean) {
        this.visible = visible
        Thread {
            blocking("onForeground $visible") { it.onForeground(visible) }
        }.start()
    }

    private fun Boolean.toPriority() = if (this) uniffi.pingers.PushPriority.HIGH else uniffi.pingers.PushPriority.NORMAL

    /** A notification the core wrote, as the platform layer posts it. */
    internal fun uniffi.pingers.CoreNotification.toShell() = CoreNotification(
        id = id,
        title = title,
        body = body,
        kind = when (kind) {
            uniffi.pingers.NotificationKind.MEETUP_STARTED -> NotificationKind.MEETUP_STARTED
            uniffi.pingers.NotificationKind.JOIN_REQUEST -> NotificationKind.JOIN_REQUEST
            uniffi.pingers.NotificationKind.CHAT_MESSAGE -> NotificationKind.CHAT_MESSAGE
            uniffi.pingers.NotificationKind.LOCATION_REQUEST -> NotificationKind.LOCATION_REQUEST
        },
        lockScreen = lockScreen,
    )

    /**
     * This phone was let into a Party: posts You're in's notification, words from the core.
     * Here rather than in the repository so it also shows when the OS started the process for
     * a push alone. Off the callback's thread, so the snapshot is not taken inside the core's
     * call.
     */
    private fun notifyLetIn() {
        Thread {
            blocking("letIn notification") { c ->
                c.snapshot().use { s ->
                    s.letIn()?.let { Notifications.letIn(app, it.notificationTitle, it.notificationBody, it.notificationLockScreen) }
                }
            }
        }.start()
    }


    private fun Fix.toCore() = CoreFix(
        coordinate = Coordinate(latitude, longitude),
        accuracyM = accuracyMeters.toDouble(),
        takenAt = (timeMillis / 1000).coerceAtLeast(0).toULong(),
    )

    /** The core's one Events, handed on to each listener. */
    private object Fanout : Events {
        private inline fun each(block: (Events) -> Unit) = listeners.forEach(block)

        override fun partiesChanged() = each { it.partiesChanged() }
        override fun joinRequested(party: uniffi.pingers.PartyId) = each { it.joinRequested(party) }
        override fun letIn(party: uniffi.pingers.PartyId) {
            notifyLetIn()
            each { it.letIn(party) }
        }
        override fun meetupStarted(meetup: uniffi.pingers.MeetupId) = each { it.meetupStarted(meetup) }
        override fun meetupUpdated(meetup: uniffi.pingers.MeetupId) = each { it.meetupUpdated(meetup) }
        override fun meetupMessage(meetup: uniffi.pingers.MeetupId) = each { it.meetupMessage(meetup) }
        override fun peerLocation(meetup: uniffi.pingers.MeetupId, peer: uniffi.pingers.MemberId) = each { it.peerLocation(meetup, peer) }
        override fun locationRequest(
            meetup: uniffi.pingers.MeetupId,
            from: uniffi.pingers.MemberId,
            request: uniffi.pingers.LocationRequestId,
        ) = each { it.locationRequest(meetup, from, request) }
        override fun locationAccessChanged(meetup: uniffi.pingers.MeetupId, peer: uniffi.pingers.MemberId) =
            each { it.locationAccessChanged(meetup, peer) }
        override fun meetupEnded(meetup: uniffi.pingers.MeetupId) = each { it.meetupEnded(meetup) }
        override fun notice(notice: uniffi.pingers.Notice) {
            Log.i(TAG, "notice: ${notice::class.simpleName}")
            // Kept with its time for Home's notice and the Wake-ups screens (PhoneFacts).
            PhoneFacts.record(app, notice)
            each { it.notice(notice) }
        }
        override fun privacyStatusChanged() = each { it.privacyStatusChanged() }

        /** Posted here, for the whole process: a push may have started it with no screen open. */
        override fun notify(notification: uniffi.pingers.CoreNotification) {
            val shown = notification.toShell()
            Log.i(TAG, "notify: ${shown.kind}")
            if (shown.kind == NotificationKind.CHAT_MESSAGE && visible) return
            Notifications.post(app, shown)
            each { it.notify(notification) }
        }
    }
}
