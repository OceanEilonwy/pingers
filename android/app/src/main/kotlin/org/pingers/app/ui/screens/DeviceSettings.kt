package org.pingers.app.ui.screens

import android.content.Context
import android.content.pm.PackageManager
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import org.pingers.app.R
import org.pingers.app.data.PingersRepository
import org.pingers.app.platform.AndroidPlatform
import org.pingers.app.platform.Push
import org.pingers.app.ui.components.ChevronIcon
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SwitchRow
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.theme.body

/**
 * The device's own delivery settings, under "No wake-up pushes" in Privacy: which UnifiedPush
 * distributor wakes Pingers, and "Keep Pingers running". Both live on this phone (the
 * platform layer), not in the core.
 */
@Composable
fun DeviceSettingsRows(noWakePushes: Boolean, pushDelivery: String? = null) {
    val context = LocalContext.current
    val platform = remember { AndroidPlatform.get(context) }
    // Read again on resume: a distributor may have been installed or removed meanwhile.
    var refresh by remember { mutableIntStateOf(0) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { refresh++ }
    val distributors = remember(refresh) { Push.distributors(context) }
    var chosen by remember(refresh) { mutableStateOf(Push.distributor(context)) }
    var picking by remember { mutableStateOf(false) }
    var keepRunning by remember { mutableStateOf(platform.modes.keepRunning) }

    // The core's reason wake-ups are downgraded (key mismatch, FCM holding them back, no push
    // service) comes first: it knows what the platform layer cannot.
    val subtitle = when {
        noWakePushes -> stringResource(R.string.device_push_off)
        pushDelivery != null -> pushDelivery
        chosen != null -> label(context, chosen!!)
        distributors.isEmpty() && platform.fcmAvailable -> stringResource(R.string.device_push_none_fcm)
        distributors.isEmpty() -> stringResource(R.string.device_push_none)
        else -> stringResource(R.string.device_push_pick)
    }
    ListRow(if (!noWakePushes && distributors.isNotEmpty()) ({ picking = true }) else null) {
        TwoLines(stringResource(R.string.device_push_service), subtitle, Modifier.weight(1f), titleStyle = body(16.sp, 500))
        if (!noWakePushes && distributors.isNotEmpty()) ChevronIcon()
    }
    RowDivider()
    SwitchRow(stringResource(R.string.device_keep_running), stringResource(R.string.device_keep_running_help), keepRunning, {
        platform.modes.keepRunning = it
        keepRunning = it
    })

    if (picking) {
        AlertDialog(
            onDismissRequest = { picking = false },
            title = { Text(stringResource(R.string.device_push_dialog_title)) },
            text = {
                Column(Modifier.selectableGroup()) {
                    Text(stringResource(R.string.device_push_dialog_body), style = body(14.sp))
                    distributors.forEach { pkg ->
                        ListRow(null, Modifier.selectable(pkg == chosen, role = Role.RadioButton) {
                            Push.registerUnifiedPush(context, pkg)
                            chosen = pkg
                            picking = false
                        }) {
                            RadioButton(selected = pkg == chosen, onClick = null)
                            Text(label(context, pkg), style = body(16.sp), modifier = Modifier.weight(1f).align(Alignment.CenterVertically))
                        }
                    }
                }
            },
            confirmButton = {},
            dismissButton = { TextButton({ picking = false }) { Text(stringResource(R.string.device_cancel)) } },
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * "No wake-up pushes": the core stops asking the push relay to wake this phone, and the
 * UnifiedPush registration ends with it (design B.7). Turning it off registers again.
 */
fun setNoWakePushes(context: Context, repo: PingersRepository, on: Boolean) {
    repo.setNoWakePushes(on)
    if (on) Push.unregisterUnifiedPush(context) else Push.registerUnifiedPush(context)
}

private fun label(context: Context, pkg: String): String = try {
    val pm = context.packageManager
    pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString()
} catch (_: PackageManager.NameNotFoundException) {
    pkg
}
