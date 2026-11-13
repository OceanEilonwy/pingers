package org.pingers.app.platform.onboarding

import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import org.pingers.app.R

/**
 * The permission part of onboarding: precise location, then background location behind the
 * in-context explanation Google Play requires, then notifications. Each step can be put off
 * with "Not now". Calls [onFinished] once nothing is left to ask. The surrounding screen
 * (title, theme, the rest of onboarding) is the UI layer's.
 */
@Composable
fun PermissionOnboarding(onFinished: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var skipped by rememberSaveable { mutableStateOf(emptySet<PermissionStep>()) }
    // Bumped on resume and after each request, so the state is read again (the user may have
    // changed a permission in Settings).
    var refresh by remember { mutableIntStateOf(0) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { refresh++ }
    val state = remember(refresh, skipped) { PermissionState.read(context, skipped) }
    val step = state.next()

    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { results ->
        // Denied (or the dialog was dismissed): don't ask again this session.
        if (results.values.none { it }) step?.let { skipped = skipped + it }
        refresh++
    }

    LaunchedEffect(step) { if (step == null) onFinished() }
    if (step == null) return

    Column(
        modifier.verticalScroll(rememberScrollState()).padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        val (title, body, action) = when (step) {
            PermissionStep.FINE_LOCATION -> Triple(R.string.platform_perm_location_title, R.string.platform_perm_location_body, R.string.platform_perm_location_action)
            PermissionStep.BACKGROUND_LOCATION -> Triple(R.string.platform_perm_background_title, R.string.platform_perm_background_body, R.string.platform_perm_background_action)
            PermissionStep.NOTIFICATIONS -> Triple(R.string.platform_perm_notifications_title, R.string.platform_perm_notifications_body, R.string.platform_perm_notifications_action)
        }
        Text(stringResource(title), style = MaterialTheme.typography.headlineSmall)
        Text(stringResource(body), style = MaterialTheme.typography.bodyLarge)
        if (step == PermissionStep.BACKGROUND_LOCATION) {
            Text(stringResource(R.string.platform_perm_background_how), style = MaterialTheme.typography.bodyMedium)
        }
        Button(onClick = { launcher.launch(step.permissions()) }, modifier = Modifier.fillMaxWidth()) {
            Text(stringResource(action))
        }
        if (step == PermissionStep.BACKGROUND_LOCATION) {
            // Once refused twice, Android shows no dialog: the app's settings page is the only way.
            TextButton(
                onClick = {
                    context.startActivity(
                        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null))
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                },
                modifier = Modifier.fillMaxWidth(),
            ) { Text(stringResource(R.string.platform_perm_open_settings)) }
        }
        TextButton(onClick = { skipped = skipped + step }, modifier = Modifier.fillMaxWidth()) {
            Text(stringResource(R.string.platform_perm_not_now))
        }
    }
}
