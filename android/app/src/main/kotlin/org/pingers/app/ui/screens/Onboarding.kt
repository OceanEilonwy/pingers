package org.pingers.app.ui.screens

import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.edit
import org.pingers.app.R
import org.pingers.app.platform.Push
import org.pingers.app.platform.onboarding.PermissionOnboarding
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading

/** Whether first-run onboarding has been through (each permission granted or put off). */
object Onboarding {
    private const val PREFS = "pingers.onboarding"
    private const val KEY_DONE = "done"

    fun done(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY_DONE, false)

    fun finish(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit { putBoolean(KEY_DONE, true) }
        // Push with the default (or only) UnifiedPush distributor; Privacy offers the choice later.
        Push.registerUnifiedPush(context)
    }
}

/** First run: a welcome, then the platform layer's permission steps. */
@Composable
fun OnboardingScreen(onFinished: () -> Unit) {
    val context = LocalContext.current
    Column(
        Modifier
            .fillMaxSize()
            .background(Pc.Paper)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        Text(
            stringResource(R.string.onboarding_title),
            style = heading(30.sp, 700),
            modifier = Modifier.padding(start = 24.dp, end = 24.dp, top = 32.dp).semantics { heading() },
        )
        Text(
            stringResource(R.string.onboarding_body),
            style = body(16.sp, color = Pc.Muted, lineHeight = 1.4f),
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        PermissionOnboarding(onFinished = {
            Onboarding.finish(context)
            onFinished()
        })
    }
}
