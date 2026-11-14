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
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import org.pingers.app.data.CoreRepositories
import org.pingers.app.data.CoreRepository
import org.pingers.app.data.FakeRepository
import org.pingers.app.data.PartyId
import org.pingers.app.data.PingersRepository
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
import org.pingers.app.ui.screens.OnboardingScreen
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

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        val fake = debuggable(this) && intent.getBooleanExtra(EXTRA_FAKE, false)
        repo = if (fake) {
            FakeRepository(resources, lifecycleScope)
        } else {
            CoreRepositories.get(applicationContext).also(::follow)
        }
        pending = intent
        setContent {
            PingersTheme {
                // First run: permissions before anything else (not for the fake screenshots).
                var onboarded by remember { mutableStateOf(fake || Onboarding.done(this)) }
                // Test tags as view resource ids, so UI Automator on the host finds them (ui/E2eTags.kt).
                Box(Modifier.fillMaxSize().semantics { testTagsAsResourceId = true }) {
                    if (onboarded) App(repo, nav, pending) { pending = null } else OnboardingScreen { onboarded = true }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        pending = intent
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

private fun debuggable(context: android.content.Context) =
    context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0

@Composable
private fun App(repo: PingersRepository, nav: Navigator, pending: Intent?, consumed: () -> Unit) {
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
        // Debug builds only: `--es map_host http://10.0.2.2:8090` points maps at a host on the
        // development machine (android/docs/maps.md, "Testing").
        if (debuggable(context) && intent.hasExtra(EXTRA_MAP_HOST)) maps.setHostOverride(intent.getStringExtra(EXTRA_MAP_HOST))
        when {
            intent.getStringExtra(CoPresenceNotifier.EXTRA_ACTION) == CoPresenceNotifier.OPEN_MEETUP -> {
                repo.dismissAlert()
                nav.reset(Route.Meetup)
            }
            intent.getStringExtra(CoPresenceNotifier.EXTRA_ACTION) == CoPresenceNotifier.HIDE_ME -> {
                repo.hideMeHere()
                nav.reset(Route.Home)
            }
            intent.getStringExtra(Notifications.EXTRA_OPEN) == Notifications.OPEN_LET_IN -> {
                Notifications.cancelLetIn(context)
                nav.reset(Route.LetIn)
            }
            // A join link: Open invite remembers it; only "Ask to join" hands it to the core.
            intent.action == Intent.ACTION_VIEW && intent.data != null -> {
                repo.showInvite(intent.dataString.orEmpty())
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
        if (intent.hasExtra(CoPresenceNotifier.EXTRA_ACTION)) CoPresenceNotifier.cancel(context)
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
                if (r == Route.MapRegions) MapRegionsScreen(maps, regionsAround, nav) else Screen(r, repo, nav, simulateAlert)
            }
        }
    }
}

@Composable
private fun Screen(route: Route, repo: PingersRepository, nav: Navigator, simulateAlert: () -> Unit) {
    when (route) {
        Route.Home -> HomeScreen(repo.home.collectAsStateWithLifecycle().value, repo, nav, simulateAlert)
        Route.NewParty -> {
            // One check each time the screen opens: Create enables once server and relay answer.
            LaunchedEffect(Unit) { repo.openNewParty() }
            NewPartyScreen(repo.newParty.collectAsStateWithLifecycle().value, repo, nav)
        }
        Route.Advanced -> AdvancedScreen(repo.advanced.collectAsStateWithLifecycle().value, repo, nav)
        Route.Invite -> InviteScreen(repo.invite.collectAsStateWithLifecycle().value, repo, nav)
        Route.JoinRequest -> JoinRequestScreen(repo.joinRequest.collectAsStateWithLifecycle().value, repo, nav)
        Route.OpenInvite -> OpenInviteScreen(repo.openInvite.collectAsStateWithLifecycle().value, repo, nav)
        Route.AskedToJoin -> AskedToJoinScreen(repo.askedToJoin.collectAsStateWithLifecycle().value, repo, nav)
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
    }
}
