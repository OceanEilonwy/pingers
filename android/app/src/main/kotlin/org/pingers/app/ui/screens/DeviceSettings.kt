package org.pingers.app.ui.screens

import android.content.Context
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.sp
import org.pingers.app.R
import org.pingers.app.data.PingersRepository
import org.pingers.app.platform.Push
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.theme.body

/**
 * Wake-ups' "Push service" choice: which UnifiedPush distributor wakes Pingers. The choice
 * lives on this phone (the platform layer), not in the core.
 */
@Composable
fun PushServicePicker(onDismiss: () -> Unit, onPicked: () -> Unit) {
    val context = LocalContext.current
    val distributors = remember { Push.distributors(context) }
    val chosen = remember { Push.distributor(context) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.device_push_dialog_title)) },
        text = {
            Column(Modifier.selectableGroup()) {
                Text(stringResource(R.string.device_push_dialog_body), style = body(14.sp))
                distributors.forEach { pkg ->
                    ListRow(null, Modifier.selectable(pkg == chosen, role = Role.RadioButton) {
                        Push.registerUnifiedPush(context, pkg)
                        onPicked()
                    }) {
                        RadioButton(selected = pkg == chosen, onClick = null)
                        Text(Push.label(context, pkg), style = body(16.sp), modifier = Modifier.weight(1f).align(Alignment.CenterVertically))
                    }
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onDismiss) { Text(stringResource(R.string.device_cancel)) } },
        modifier = Modifier.fillMaxWidth(),
    )
}

/**
 * "No wake-up pushes": the core stops asking the push relay to wake this phone, and the
 * UnifiedPush registration ends with it (design B.7). Turning it off registers again.
 */
fun setNoWakePushes(context: Context, repo: PingersRepository, on: Boolean) {
    repo.setNoWakePushes(on)
    if (on) Push.unregisterUnifiedPush(context) else Push.registerUnifiedPush(context)
}
