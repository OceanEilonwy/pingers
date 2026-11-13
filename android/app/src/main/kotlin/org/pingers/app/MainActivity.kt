package org.pingers.app

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
import androidx.core.view.WindowCompat
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import org.pingers.app.data.FakeRepository
import org.pingers.app.data.PingersRepository
import org.pingers.app.ui.screens.AcceptInviteScreen
import org.pingers.app.ui.screens.AdvancedScreen
import org.pingers.app.ui.screens.AlertScreen
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
import org.pingers.app.ui.screens.PartyScreen
import org.pingers.app.ui.screens.PrivacyScreen
import org.pingers.app.ui.screens.SecurityScreen
import org.pingers.app.ui.screens.ServerViewScreen
import org.pingers.app.ui.screens.SpotEditorScreen
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.PingersTheme

class MainActivity : ComponentActivity() {
    private val nav = Navigator()

    // The one repository. FakeRepository until the Rust core is wired in.
    private lateinit var repo: PingersRepository

    /** A notification action or join link waiting for the UI. */
    private var pending by mutableStateOf<Intent?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        repo = FakeRepository(resources, lifecycleScope)
        pending = intent
        setContent {
            PingersTheme {
                App(repo, nav, pending) { pending = null }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        pending = intent
    }
}

private const val EXTRA_SCREEN = "screen"

private fun debuggable(context: android.content.Context) =
    context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0

@Composable
private fun App(repo: PingersRepository, nav: Navigator, pending: Intent?, consumed: () -> Unit) {
    val context = LocalContext.current
    val activity = context as ComponentActivity
    val route = nav.current

    LaunchedEffect(pending) {
        val intent = pending ?: return@LaunchedEffect
        when {
            intent.getStringExtra(CoPresenceNotifier.EXTRA_ACTION) == CoPresenceNotifier.OPEN_MEETUP -> {
                repo.dismissAlert()
                nav.reset(Route.Meetup)
            }
            intent.getStringExtra(CoPresenceNotifier.EXTRA_ACTION) == CoPresenceNotifier.HIDE_ME -> {
                repo.hideMeHere()
                nav.reset(Route.Home)
            }
            intent.action == Intent.ACTION_VIEW && intent.data != null -> nav.go(Route.AcceptInvite)
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
            Screen(r, repo, nav, simulateAlert)
        }
    }
}

@Composable
private fun Screen(route: Route, repo: PingersRepository, nav: Navigator, simulateAlert: () -> Unit) {
    when (route) {
        Route.Home -> HomeScreen(repo.home.collectAsStateWithLifecycle().value, repo, nav, simulateAlert)
        Route.NewParty -> NewPartyScreen(repo.newParty.collectAsStateWithLifecycle().value, repo, nav)
        Route.Advanced -> AdvancedScreen(repo.advanced.collectAsStateWithLifecycle().value, repo, nav)
        Route.Invite -> InviteScreen(repo.invite.collectAsStateWithLifecycle().value, repo, nav)
        Route.AcceptInvite -> AcceptInviteScreen(repo.acceptInvite.collectAsStateWithLifecycle().value, repo, nav)
        Route.Party -> PartyScreen(repo.party.collectAsStateWithLifecycle().value, repo, nav)
        Route.SpotEditor -> SpotEditorScreen(repo.spotEditor.collectAsStateWithLifecycle().value, repo, nav)
        Route.DrawArea -> DrawAreaScreen(repo.drawArea.collectAsStateWithLifecycle().value, repo, nav)
        Route.ExclusionZones -> ExclusionZonesScreen(repo.exclusionZones.collectAsStateWithLifecycle().value, repo, nav)
        Route.Privacy -> PrivacyScreen(repo.privacy.collectAsStateWithLifecycle().value, repo, nav)
        Route.ServerView -> ServerViewScreen(repo.serverView.collectAsStateWithLifecycle().value, nav)
        Route.Security -> SecurityScreen(remember { CoreInfo.read() }, nav)
        Route.Alert -> AlertScreen(repo.home.collectAsStateWithLifecycle().value.alert, repo, nav)
        Route.Meetup -> MeetupScreen(repo.meetup.collectAsStateWithLifecycle().value, repo, nav)
        Route.Chat -> ChatScreen(repo.chat.collectAsStateWithLifecycle().value, repo, nav)
        Route.MeetPoint -> MeetPointScreen(repo.meetPoint.collectAsStateWithLifecycle().value, repo, nav)
        Route.Compass -> CompassScreen(repo.compass.collectAsStateWithLifecycle().value, repo, nav)
        Route.LocationRequest -> LocationRequestScreen(repo.locationRequest.collectAsStateWithLifecycle().value, repo, nav)
        Route.MeetupEnded -> MeetupEndedScreen(repo.meetupEnded.collectAsStateWithLifecycle().value, nav)
    }
}
