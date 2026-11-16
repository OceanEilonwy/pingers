package org.pingers.app.ui.screens

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.edit
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.first
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.SetupLinkUi
import org.pingers.app.platform.Push
import org.pingers.app.platform.onboarding.PermissionState
import org.pingers.app.platform.onboarding.PermissionStep
import org.pingers.app.platform.onboarding.permissions
import org.pingers.app.ui.E2eTags
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import uniffi.pingers.SetupLinkAction
import uniffi.pingers.SetupLinkState
import uniffi.pingers.SetupStep

/** Whether first-run setup has been through (each permission granted or put off). */
object Onboarding {
    private const val PREFS = "pingers.onboarding"
    private const val KEY_DONE = "done"

    fun done(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_DONE, false)

    fun finish(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit { putBoolean(KEY_DONE, true) }
        // Push with the default (or only) UnifiedPush distributor; Wake-ups offers the choice later.
        Push.registerUnifiedPush(context)
    }
}

private fun SetupStep.permission(): PermissionStep = when (this) {
    SetupStep.LOCATION -> PermissionStep.FINE_LOCATION
    SetupStep.BACKGROUND -> PermissionStep.BACKGROUND_LOCATION
    SetupStep.NOTIFICATIONS -> PermissionStep.NOTIFICATIONS
}

private fun PermissionState.granted(step: SetupStep) = when (step) {
    SetupStep.LOCATION -> fineLocation
    SetupStep.BACKGROUND -> backgroundLocation
    SetupStep.NOTIFICATIONS -> notifications
}

/**
 * First run (design canvas SetupWelcome, SetupBackground; step 3 uses step 2's layout): the
 * location permission, location while the app is closed, then notifications. Each step asks
 * Android for its permission or can be put off with "Not now"; a step already granted, or one
 * that can't be asked yet (background location before location), is passed over. It asks for
 * no server or code. The words are pingers-words' (`setupView`).
 */
@Composable
fun SetupScreen(repo: PingersRepository, start: SetupStep? = null, onFinished: () -> Unit) {
    val context = LocalContext.current
    var refresh by remember { mutableIntStateOf(0) }
    val permissions = remember(refresh) { PermissionState.read(context) }
    var step by rememberSaveable { mutableStateOf(start ?: SetupStep.LOCATION) }
    // Android shows no dialog for background location once refused twice: then Continue opens
    // the app's settings page instead, where "Allow all the time" is.
    var backgroundRefused by rememberSaveable { mutableStateOf(false) }

    fun advance(from: SetupStep) {
        var next = repo.setupStep(from).next
        val now = PermissionState.read(context)
        while (next != null && (now.granted(next) || (next == SetupStep.BACKGROUND && !now.fineLocation))) {
            next = repo.setupStep(next).next
        }
        if (next == null) onFinished() else step = next
    }

    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { results ->
        if (step == SetupStep.BACKGROUND && results.values.none { it }) backgroundRefused = true
        refresh++
        advance(step)
    }
    // Back from Android's settings: move on if the permission is now there.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
        refresh++
        if (step == SetupStep.BACKGROUND && backgroundRefused && PermissionState.read(context).backgroundLocation) advance(step)
    }

    val view = remember(step) { repo.setupStep(step) }
    val primary = {
        when {
            permissions.granted(step) -> advance(step)
            step == SetupStep.NOTIFICATIONS && Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU -> advance(step)
            step == SetupStep.BACKGROUND && backgroundRefused -> context.startActivity(
                Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
            else -> launcher.launch(step.permission().permissions())
        }
    }
    Column(
        Modifier
            .fillMaxSize()
            .background(Pc.Paper)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 24.dp),
    ) {
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(top = 28.dp),
            verticalArrangement = Arrangement.spacedBy(if (view.cardTitle != null) 22.dp else 20.dp),
        ) {
            if (view.cardTitle != null) Welcome(view.heading, view.body, view.cardTitle.orEmpty(), view.cardBody.orEmpty())
            else Step(if (step == SetupStep.BACKGROUND) PIcons.Moon else PIcons.Bell, view.heading, view.body, view.hint, view.choices)
            Dots(view.dot.toInt(), view.dots.toInt(), view.progress)
        }
        Column(Modifier.padding(top = 12.dp, bottom = 16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            PillButton(view.primary, primary, Modifier.testTag(E2eTags.SETUP_CONTINUE))
            PillButton(view.notNow, { advance(step) }, Modifier.testTag(E2eTags.SETUP_NOT_NOW), background = Color.Transparent, content = Pc.Ink, height = 48.dp)
        }
    }
}

@Composable
private fun ColumnScope.Welcome(heading: String, body: String, cardTitle: String, cardBody: String) {
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Tile(PIcons.People, Pc.Blue, Color.White, 76, 24, 36)
        Text(heading, style = heading(30.sp, 700), textAlign = TextAlign.Center, modifier = Modifier.semantics { heading() })
        Text(body, style = body(16.sp, color = Pc.Muted, lineHeight = 1.45f), textAlign = TextAlign.Center)
    }
    Card(radius = 20.dp, padding = androidx.compose.foundation.layout.PaddingValues(18.dp)) {
        Tile(PIcons.Pin, Pc.BlueTint, Pc.BlueDark, 44, 14, 22)
        Spacer(Modifier.size(12.dp))
        Text(cardTitle, style = heading(20.sp))
        Spacer(Modifier.size(12.dp))
        Text(cardBody, style = body(14.sp, color = Pc.Body, lineHeight = 1.5f))
    }
}

@Composable
private fun Step(icon: ImageVector, heading: String, body: String, hint: String?, choices: List<String>) {
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Tile(icon, Pc.BlueTint, Pc.BlueDark, 56, 18, 28)
        Text(heading, style = heading(28.sp, 700).copy(lineHeight = 32.sp), modifier = Modifier.semantics { heading() })
        Text(body, style = body(15.sp, color = Pc.Body, lineHeight = 1.5f))
    }
    if (hint != null) {
        Column(
            Modifier.fillMaxWidth().clip(RoundedCornerShape(18.dp)).background(Pc.BlueTint).padding(horizontal = 16.dp, vertical = 14.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(hint, style = body(14.sp, 600, Pc.BlueDark, lineHeight = 1.4f))
            if (choices.isNotEmpty()) {
                // A picture of Android's dialog, the option to choose first; not a control.
                Column(
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(Pc.Card).padding(horizontal = 14.dp, vertical = 6.dp)
                        .semantics(mergeDescendants = true) {},
                ) {
                    choices.forEachIndexed { i, choice ->
                        if (i > 0) RowDivider()
                        Row(Modifier.heightIn(min = 40.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            Box(Modifier.size(18.dp).clip(CircleShape).border(if (i == 0) 5.dp else 2.dp, if (i == 0) Pc.Blue else Pc.InkSubtle, CircleShape))
                            Text(choice, style = body(14.sp, if (i == 0) 600 else 400, if (i == 0) Pc.Ink else Pc.Muted))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Tile(icon: ImageVector, background: Color, tint: Color, size: Int, radius: Int, iconSize: Int) {
    Box(Modifier.size(size.dp).clip(RoundedCornerShape(radius.dp)).background(background), contentAlignment = Alignment.Center) {
        Icon(icon, null, tint = tint, modifier = Modifier.size(iconSize.dp))
    }
}

/** The progress dots: past ones blue, the current one long, the rest grey. */
@Composable
private fun Dots(current: Int, count: Int, label: String) {
    Row(
        Modifier.fillMaxWidth().semantics(mergeDescendants = true) { contentDescription = label },
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
    ) {
        repeat(count) { i ->
            Box(
                Modifier
                    .size(width = if (i == current) 22.dp else 8.dp, height = 8.dp)
                    .clip(RoundedCornerShape(4.dp))
                    .background(if (i <= current) Pc.Blue else Pc.Handle),
            )
        }
    }
}

/**
 * The first-phone setup link's step: the owner scanned their appliance's setup page with the
 * phone's camera. It asks before the core makes this phone's account with the link's code,
 * then shows the progress and what came of it, in first run's layout. Every word comes from
 * the core (pingers-words through `setupLinkView`). [onDone] runs once the step closes, by its
 * buttons, Back, or because the core could not check the link; then first run or Home follows.
 * The link is shown to the repository before the screen opens ([PingersRepository.showSetupLink]).
 */
@Composable
fun SetupLinkScreen(repo: PingersRepository, onDone: () -> Unit) {
    val ui = repo.setupLink.collectAsStateWithLifecycle().value
    // Watches the flow itself rather than [ui]: a step that opens and closes between two frames
    // (a check the core could not make) still ends here.
    LaunchedEffect(repo) {
        repo.setupLink.first { it == SetupLinkUi.Closed }
        onDone()
    }
    val shown = ui as? SetupLinkUi.Shown
    val view = shown?.view
    val leave = { repo.closeSetupLink() }
    // Back leaves the step like "Not now", except while the account is being made.
    BackHandler(enabled = view?.busy != true, onBack = leave)
    Column(
        Modifier
            .fillMaxSize()
            .background(Pc.Paper)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(horizontal = 24.dp),
    ) {
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(top = 28.dp),
            verticalArrangement = Arrangement.spacedBy(20.dp),
        ) {
            if (shown != null) {
                val (icon, background, tint) = shown.state.look()
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Tile(icon, background, tint, 56, 18, 28)
                    Text(
                        shown.view.heading,
                        style = heading(28.sp, 700).copy(lineHeight = 32.sp),
                        modifier = Modifier.semantics { heading() }.testTag(E2eTags.SETUP_LINK_HEADING),
                    )
                    Text(shown.view.body, style = body(15.sp, color = Pc.Body, lineHeight = 1.5f))
                }
            }
        }
        Column(
            Modifier.padding(top = 12.dp, bottom = 16.dp).heightIn(min = 106.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp, Alignment.Bottom),
        ) {
            when {
                view == null || view.busy -> Box(Modifier.fillMaxWidth().height(96.dp), contentAlignment = Alignment.Center) {
                    // The system's indicator; the heading above says what it waits for.
                    CircularProgressIndicator(Modifier.size(32.dp).testTag(E2eTags.SETUP_LINK_BUSY), color = Pc.Blue)
                }
                else -> {
                    val primary = view.primary
                    if (primary != null) {
                        PillButton(primary, {
                            when (view.action) {
                                SetupLinkAction.SET_UP, SetupLinkAction.TRY_AGAIN -> repo.actOnSetupLink()
                                SetupLinkAction.LEAVE, null -> leave()
                            }
                        }, Modifier.testTag(E2eTags.SETUP_LINK_PRIMARY))
                    }
                    val notNow = view.notNow
                    if (notNow != null) {
                        PillButton(
                            notNow, leave, Modifier.testTag(E2eTags.SETUP_LINK_NOT_NOW),
                            background = Color.Transparent, content = Pc.Ink, height = 48.dp,
                        )
                    }
                }
            }
        }
    }
}

/** The tile above the heading: the question and the wait, done, or something to fix. */
private fun SetupLinkState.look(): Triple<ImageVector, Color, Color> = when (this) {
    is SetupLinkState.Confirm, is SetupLinkState.Working -> Triple(PIcons.ShieldCheck, Pc.BlueTint, Pc.BlueDark)
    SetupLinkState.SetUp, SetupLinkState.AlreadySetUp -> Triple(PIcons.Check, Pc.BlueTint, Pc.BlueDark)
    is SetupLinkState.Unreachable -> Triple(PIcons.Signal, Pc.OrangeTint, Pc.OrangeText)
    SetupLinkState.Refused, is SetupLinkState.OtherServer, is SetupLinkState.BuildKeysDiffer,
    is SetupLinkState.ServerKeysDiffer, SetupLinkState.Broken -> Triple(PIcons.AlertCircle, Pc.OrangeTint, Pc.OrangeText)
}
