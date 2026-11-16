package org.pingers.app

import androidx.compose.runtime.CompositionLocalProvider
import org.pingers.app.maps.Maps
import org.pingers.app.ui.map.LocalMaps
import org.pingers.app.ui.map.LocalOpenMapRegions
import org.pingers.app.ui.screens.MapRegionsScreen
import android.Manifest
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.core.view.WindowCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import org.pingers.app.data.CoreRepositories
import org.pingers.app.data.CoreRepository
import org.pingers.app.data.FakeRepository
import org.pingers.app.data.PartyId
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.SetupLinkUi
import org.pingers.app.platform.Notifications
import org.pingers.app.ui.screens.AdvancedScreen
import org.pingers.app.ui.screens.AlertScreen
import org.pingers.app.ui.screens.AskedToJoinScreen
import org.pingers.app.ui.screens.JoinRequestScreen
import org.pingers.app.ui.screens.LetInScreen
import org.pingers.app.ui.screens.OpenInviteScreen
import org.pingers.app.ui.screens.ChatScreen
import org.pingers.app.ui.screens.CompassScreen
import org.pingers.app.ui.screens.DrawAreaScreen
import org.pingers.app.ui.screens.ExclusionZonesScreen
import org.pingers.app.ui.screens.HomeScreen
import org.pingers.app.ui.screens.InviteScreen
import org.pingers.app.ui.screens.LocationRequestScreen
import org.pingers.app.ui.screens.MeetPointScreen
import org.pingers.app.ui.screens.MeetupEndedScreen
import org.pingers.app.ui.screens.MeetupScreen
import org.pingers.app.ui.screens.NewPartyScreen
import org.pingers.app.ui.screens.Onboarding
import org.pingers.app.ui.screens.SetupLinkScreen
import org.pingers.app.ui.screens.SetupScreen
import org.pingers.app.ui.screens.SettingsScreen
import org.pingers.app.ui.screens.YourNameScreen
import org.pingers.app.ui.screens.QuietHoursScreen
import org.pingers.app.ui.screens.ZoneTimesScreen
import org.pingers.app.ui.screens.WakeupsScreen
import org.pingers.app.ui.screens.PartySettingsScreen
import org.pingers.app.ui.screens.SafetyCodesScreen
import org.pingers.app.ui.screens.LeavePartySheet
import org.pingers.app.ui.screens.PartyCodeScreen
import org.pingers.app.ui.screens.PartyScreen
import org.pingers.app.ui.screens.PrivacyScreen
import org.pingers.app.ui.screens.SecurityScreen
import org.pingers.app.ui.screens.ServerViewScreen
import org.pingers.app.ui.screens.SpotEditorScreen
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.PingersTheme

class MainActivity : ComponentActivity() {
    private val nav = Navigator()

    // The one repository: the Rust core's. Debug builds launched with `--ez fake true` use
    // FakeRepository instead, for the canvas screenshots (android/docs/screenshots).
    private lateinit var repo: PingersRepository

    /** A notification action or join link waiting for the UI. */
    private var pending by mutableStateOf<Intent?>(null)

    /** Whether first-run setup has been through (each permission granted or put off). */
    private var onboarded by mutableStateOf(false)

    /**
     * Whether a setup link opened before first run was through (the owner's first phone, just
     * installed) has its step showing: it comes before first run. Afterwards App routes them.
     */
    private var firstRunSetupLink by mutableStateOf(false)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        val fake = debuggable(this) && intent.getBooleanExtra(EXTRA_FAKE, false)
        // Debug builds only: `--es variant shared` and the like pick a screen's state (android/README.md).
        val variant = if (debuggable(this)) intent.getStringExtra(EXTRA_VARIANT) else null
        repo = if (fake) {
            FakeRepository(resources, lifecycleScope, variant)
        } else {
            CoreRepositories.get(applicationContext).also(::follow)
        }
        // First run: permissions before anything else (not for the fake screenshots).
        onboarded = fake || Onboarding.done(this)
        if (savedInstanceState != null && repo.setupLink.value != SetupLinkUi.Closed) {
            // Recreated while a setup link's step is open (the repository outlives the activity):
            // show it again as it is, without checking the link again or stopping "Set up".
            if (onboarded) nav.go(Route.SetupLink) else firstRunSetupLink = true
        } else {
            take(intent)
        }
        setContent {
            PingersTheme {
                // Debug builds: test tags as view resource ids, so UI Automator on the host finds them (ui/E2eTags.kt).
                Box(Modifier.fillMaxSize().semantics { testTagsAsResourceId = BuildConfig.DEBUG }) {
                    if (onboarded) {
                        App(repo, nav, pending, variant) { pending = null }
                    } else if (firstRunSetupLink) {
                        SetupLinkScreen(repo) { firstRunSetupLink = false }
                    } else {
                        SetupScreen(repo) {
                            Onboarding.finish(this@MainActivity)
                            onboarded = true
                        }
                    }
                }
            }
        }
    }

    /**
     * Keeps [intent] for the UI. Before first run is through, a setup link goes to its step at
     * once instead (first run follows it); any other link waits for first run to finish.
     */
    private fun take(intent: Intent) {
        val setupLink = if (onboarded) null else setupLinkOf(intent)
        if (setupLink != null) {
            repo.showSetupLink(setupLink)
            firstRunSetupLink = true
        } else {
            pending = intent
        }
    }

    override fun onResume() {
        super.onResume()
        // Push app, battery, notifications: any may have changed in Android's settings meanwhile.
        repo.refreshPhone()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        take(intent)
    }

    /** Routes the core's events while this activity is started. */
    private fun follow(core: CoreRepository) {
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                core.events.collect { event ->
                    when (event) {
                        // The core posted the notification already (CoreHost); in the app the alert screen opens.
                        is CoreRepository.Event.MeetupStarted -> nav.go(Route.Alert)
                        CoreRepository.Event.LocationRequested -> nav.go(Route.LocationRequest)
                        CoreRepository.Event.MeetupEnded -> nav.reset(Route.MeetupEnded)
                    }
                }
            }
        }
        // You're in: once per Party, when Events.let_in published it or when the app starts
        // with one not yet dismissed. CoreHost posts the notification.
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                core.letIn.map { it?.party }.distinctUntilChanged().filterNotNull().collect { party ->
                    if (party != shownLetIn) {
                        shownLetIn = party
                        if (nav.current != Route.LetIn) nav.go(Route.LetIn)
                    }
                }
            }
        }
    }

    /** The Party You're in was last routed to, so coming back to the app doesn't route again. */
    private var shownLetIn: PartyId? = null
}

private const val EXTRA_SCREEN = "screen"
private const val EXTRA_FAKE = "fake"
private const val EXTRA_MAP_HOST = "map_host"
private const val EXTRA_VARIANT = "variant"

private fun debuggable(context: android.content.Context) =
    context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0

/** The setup link an opened link carries (InviteLinks.setupFromView), or null. */
private fun setupLinkOf(intent: Intent): String? =
    if (intent.action == Intent.ACTION_VIEW) {
        InviteLinks.setupFromView(intent.dataString, intent.getStringExtra(InviteLinks.EXTRA_INVITE))
    } else {
        null
    }

@Composable
private fun App(repo: PingersRepository, nav: Navigator, pending: Intent?, variant: String?, consumed: () -> Unit) {
    val context = LocalContext.current
    val activity = context as ComponentActivity
    val route = nav.current
    val maps = remember { Maps.get(context) }
    // Where "Download map for this area" was pressed, for the picker's "This area". On this phone only.
    var regionsAround by remember { mutableStateOf<org.pingers.app.data.Coordinate?>(null) }
    val openRegions: (org.pingers.app.data.Coordinate?) -> Unit = {
        regionsAround = it
        nav.go(Route.MapRegions)
    }

    LaunchedEffect(pending) {
        val intent = pending ?: return@LaunchedEffect
        // Debug builds only: `--es map_host https://10.0.2.2:9446` points maps at a host on the
        // development machine, such as the local stack's (android/docs/maps.md, "Testing").
        if (debuggable(context) && intent.hasExtra(EXTRA_MAP_HOST)) maps.setHostOverride(intent.getStringExtra(EXTRA_MAP_HOST))
        // A notification's action comes only from NotificationTrampoline, in this process; this
        // activity is exported, so no extra of the intent itself is ever taken for one.
        val action = NotificationActions.take()
        when {
            action == NotificationAction.OPEN_MEETUP -> {
                repo.dismissAlert()
                nav.reset(Route.Meetup)
            }
            action == NotificationAction.HIDE_ME -> {
                repo.hideMeHere()
                nav.reset(Route.Home)
            }
            action == NotificationAction.OPEN_LET_IN -> {
                Notifications.cancelLetIn(context)
                nav.reset(Route.LetIn)
            }
            // A first-phone setup link (`#s1.`, in the same forms as a join link): its step
            // checks it with the core, then asks before making this phone's account.
            setupLinkOf(intent) != null -> {
                repo.showSetupLink(setupLinkOf(intent).orEmpty())
                nav.go(Route.SetupLink)
            }
            // A join link (InviteLinks: any server's https link, pingers://join, or the landing
            // page's intent): Open invite remembers it; only "Ask to join" hands it to the core.
            intent.action == Intent.ACTION_VIEW &&
                InviteLinks.fromView(intent.dataString, intent.getStringExtra(InviteLinks.EXTRA_INVITE)) != null -> {
                repo.showInvite(
                    InviteLinks.fromView(intent.dataString, intent.getStringExtra(InviteLinks.EXTRA_INVITE)).orEmpty(),
                )
                nav.go(Route.OpenInvite)
            }
            // Debug builds only: `adb shell am start -n ... --es screen Compass` opens a screen
            // directly, for screenshots (android/docs/screenshots).
            debuggable(context) && intent.hasExtra(EXTRA_SCREEN) ->
                Route.entries.firstOrNull { it.name == intent.getStringExtra(EXTRA_SCREEN) }?.let {
                    if (it == Route.Alert) repo.simulateCoPresence()
                    nav.reset(it)
                }
        }
        consumed()
    }

    SideEffect {
        WindowCompat.getInsetsController(activity.window, activity.window.decorView).apply {
            isAppearanceLightStatusBars = !route.dark
            isAppearanceLightNavigationBars = !route.dark
        }
    }

    BackHandler(enabled = nav.stack.size > 1) { nav.back() }

    val notificationPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        repo.home.value.alert?.let { alert -> CoPresenceNotifier.post(context, alert) }
    }
    val simulateAlert = {
        repo.simulateCoPresence()
        val alert = repo.home.value.alert
        if (alert != null) {
            if (CoPresenceNotifier.canPost(context)) {
                CoPresenceNotifier.post(context, alert)
            } else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        }
        nav.go(Route.Alert)
    }

    Box(Modifier.fillMaxSize().background(if (route.dark) Pc.Ink else Pc.Paper)) {
        AnimatedContent(
            targetState = route,
            transitionSpec = { fadeIn() togetherWith fadeOut() },
            label = "screen",
        ) { r ->
            CompositionLocalProvider(LocalMaps provides maps, LocalOpenMapRegions provides openRegions) {
                if (r == Route.MapRegions) MapRegionsScreen(maps, regionsAround, nav) else Screen(r, repo, nav, variant, simulateAlert)
            }
        }
    }
}

/** How often a screen waiting on a join reads the mailboxes while it shows. */
private const val CHECK_JOINS_EVERY_MS = 5_000L

/**
 * Reads what joins wait for every few seconds while the screen shows and the app is in front:
 * Asked to join learns it was let in, Invite people sees who asked, even with no wake-up push.
 */
@Composable
private fun CheckJoinsWhileShown(repo: PingersRepository) {
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            while (true) {
                repo.checkJoins()
                delay(CHECK_JOINS_EVERY_MS)
            }
        }
    }
}

@Composable
private fun Screen(route: Route, repo: PingersRepository, nav: Navigator, variant: String?, simulateAlert: () -> Unit) {
    when (route) {
        Route.Home -> HomeScreen(repo.home.collectAsStateWithLifecycle().value, repo, nav, simulateAlert)
        Route.NewParty -> {
            // One check each time the screen opens: Create enables once server and relay answer.
            LaunchedEffect(Unit) { repo.openNewParty() }
            NewPartyScreen(repo.newParty.collectAsStateWithLifecycle().value, repo, nav)
        }
        Route.Advanced -> AdvancedScreen(repo.advanced.collectAsStateWithLifecycle().value, repo, nav)
        Route.Invite -> {
            CheckJoinsWhileShown(repo)
            InviteScreen(repo.invite.collectAsStateWithLifecycle().value, repo, nav)
        }
        Route.JoinRequest -> JoinRequestScreen(repo.joinRequest.collectAsStateWithLifecycle().value, repo, nav)
        Route.OpenInvite -> OpenInviteScreen(repo.openInvite.collectAsStateWithLifecycle().value, repo, nav)
        Route.AskedToJoin -> {
            CheckJoinsWhileShown(repo)
            AskedToJoinScreen(repo.askedToJoin.collectAsStateWithLifecycle().value, repo, nav)
        }
        Route.LetIn -> LetInScreen(repo.letIn.collectAsStateWithLifecycle().value, repo, nav)
        Route.Party -> PartyScreen(repo.party.collectAsStateWithLifecycle().value, repo, nav)
        Route.SpotEditor -> SpotEditorScreen(repo.spotEditor.collectAsStateWithLifecycle().value, repo, nav)
        Route.DrawArea -> DrawAreaScreen(repo.drawArea.collectAsStateWithLifecycle().value, repo, nav)
        Route.ExclusionZones -> ExclusionZonesScreen(repo.exclusionZones.collectAsStateWithLifecycle().value, repo, nav)
        Route.Privacy -> PrivacyScreen(repo.privacy.collectAsStateWithLifecycle().value, repo, nav)
        Route.ServerView -> {
            // One check each time the screen opens; its notes then say whether the relay answered.
            LaunchedEffect(Unit) { repo.checkReachability() }
            ServerViewScreen(repo.serverView.collectAsStateWithLifecycle().value, nav)
        }
        Route.Security -> SecurityScreen(remember { CoreInfo.read() }, nav)
        Route.Alert -> AlertScreen(repo.home.collectAsStateWithLifecycle().value.alert, repo, nav)
        Route.Meetup -> MeetupScreen(repo.meetup.collectAsStateWithLifecycle().value, repo, nav)
        Route.Chat -> ChatScreen(repo.chat.collectAsStateWithLifecycle().value, repo, nav)
        Route.MeetPoint -> MeetPointScreen(repo.meetPoint.collectAsStateWithLifecycle().value, repo, nav)
        Route.Compass -> CompassScreen(repo.compass.collectAsStateWithLifecycle().value, repo, nav)
        Route.LocationRequest -> LocationRequestScreen(repo.locationRequest.collectAsStateWithLifecycle().value, repo, nav)
        Route.MeetupEnded -> MeetupEndedScreen(repo.meetupEnded.collectAsStateWithLifecycle().value, nav)
        Route.MapRegions -> {}
        // Debug builds open first run with `--es screen Setup` (`--es variant background` or
        // `notifications` for steps 2 and 3); it ends back on Home.
        Route.Setup -> SetupScreen(
            repo,
            when (variant) {
                "background" -> uniffi.pingers.SetupStep.BACKGROUND
                "notifications" -> uniffi.pingers.SetupStep.NOTIFICATIONS
                else -> uniffi.pingers.SetupStep.LOCATION
            },
        ) { nav.reset(Route.Home) }
        Route.Settings -> SettingsScreen(repo.deviceSettings.collectAsStateWithLifecycle().value, nav)
        Route.YourName -> YourNameScreen(repo.yourName.collectAsStateWithLifecycle().value, repo, nav)
        Route.QuietHours -> QuietHoursScreen(repo.quietHours.collectAsStateWithLifecycle().value, repo, nav)
        Route.ZoneTimes -> ZoneTimesScreen(repo.zoneTimes.collectAsStateWithLifecycle().value, repo, nav)
        Route.Wakeups -> WakeupsScreen(repo.wakeups.collectAsStateWithLifecycle().value, repo, nav)
        Route.PartySettings -> PartySettingsScreen(repo.partySettings.collectAsStateWithLifecycle().value, repo, nav)
        Route.SafetyCodes -> SafetyCodesScreen(repo.safetyCodes.collectAsStateWithLifecycle().value, repo, nav)
        Route.LeaveParty -> Box(Modifier.fillMaxSize()) {
            PartySettingsScreen(repo.partySettings.collectAsStateWithLifecycle().value, repo, nav)
            LeavePartySheet(repo.leaveParty.collectAsStateWithLifecycle().value, repo, nav)
        }
        Route.SetupLink -> SetupLinkScreen(repo) { nav.reset(Route.Home) }
        Route.PartyCode -> PartyCodeScreen(
            repo.partyCode.collectAsStateWithLifecycle().value,
            repo.registrationCode.collectAsStateWithLifecycle().value,
            repo, nav,
        )
    }
}
