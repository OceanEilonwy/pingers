package org.pingers.app.ui.screens

import org.pingers.app.ui.map.LocalMaps
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringArrayResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.CoreInfo
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.PrivacyState
import org.pingers.app.data.ServerViewState
import org.pingers.app.data.Visibility
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.ChevronIcon
import org.pingers.app.ui.components.IconAction
import org.pingers.app.ui.components.IconBadge
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SectionLabel
import org.pingers.app.ui.components.SwitchRow
import org.pingers.app.ui.components.Tag
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading

// ---------------------------------------------------------------- Sharing + Focus

@Composable
fun PrivacyScreen(state: PrivacyState, repo: PingersRepository, nav: Navigator) {
    Column(Modifier.fillMaxSize().background(Pc.Paper)) {
        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .statusBarsPadding()
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
        ) {
            Row(Modifier.padding(top = 12.dp, bottom = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                if (nav.stack.size > 1 && nav.stack[nav.stack.size - 2] != Route.Home) {
                    IconAction(PIcons.Back, stringResource(R.string.cd_back), { nav.back() }, Modifier.padding(end = 4.dp))
                }
                Text(stringResource(R.string.privacy_title), style = heading(30.sp, 700), modifier = Modifier.semantics { heading() })
            }
            state.pausedBecause?.let { reason ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clip(RoundedCornerShape(20.dp))
                        .background(Pc.Ink)
                        .padding(horizontal = 18.dp, vertical = 16.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(14.dp),
                ) {
                    IconBadge(PIcons.Moon, Pc.Lavender, Pc.InkCircle, size = 42.dp)
                    TwoLines(
                        stringResource(R.string.privacy_paused, reason), stringResource(R.string.privacy_paused_detail),
                        titleStyle = body(15.sp, 600, Color.White), subtitleStyle = body(13.sp, color = Pc.InkSubtle, lineHeight = 1.4f),
                    )
                }
            }
            Section(stringResource(R.string.privacy_when))
            Card {
                SwitchRow(stringResource(R.string.privacy_sharing), null, state.sharing, repo::setSharing)
                RowDivider()
                SwitchRow(stringResource(R.string.privacy_pause_focus), stringResource(R.string.privacy_pause_focus_help), state.pauseInFocus, repo::setPauseInFocus)
                RowDivider()
                SwitchRow(stringResource(R.string.privacy_quiet), state.quietHoursRange, state.quietHours, repo::setQuietHours)
                RowDivider()
                ListRow({ nav.go(Route.ExclusionZones) }) {
                    Text(stringResource(R.string.zones_title), style = body(16.sp, 500), modifier = Modifier.weight(1f))
                    Text(state.exclusionZones.toString(), style = body(15.sp, color = Pc.Muted))
                    ChevronIcon()
                }
            }
            Section(stringResource(R.string.privacy_who))
            Card(Modifier.selectableGroup(), padding = PaddingValues(horizontal = 16.dp, vertical = 4.dp)) {
                VisibilityRow(stringResource(R.string.vis_ask), stringResource(R.string.vis_ask_help), state.visibility == Visibility.AskEachTime) {
                    repo.setVisibility(Visibility.AskEachTime)
                }
                RowDivider()
                VisibilityRow(stringResource(R.string.vis_everyone), null, state.visibility == Visibility.Everyone) {
                    repo.setVisibility(Visibility.Everyone)
                }
                RowDivider()
                VisibilityRow(stringResource(R.string.vis_nobody), null, state.visibility == Visibility.Nobody) {
                    repo.setVisibility(Visibility.Nobody)
                }
            }
            Section(stringResource(R.string.privacy_delivery))
            Card {
                SwitchRow(stringResource(R.string.privacy_no_wake), stringResource(R.string.privacy_no_wake_help), state.noWakePushes, repo::setNoWakePushes)
            }
            Row(
                Modifier
                    .padding(top = 20.dp)
                    .fillMaxWidth()
                    .clip(RoundedCornerShape(18.dp))
                    .background(Pc.BlueTint)
                    .clickable { nav.go(Route.Security) }
                    .padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Icon(PIcons.Shield, null, tint = Pc.BlueDark, modifier = Modifier.size(20.dp))
                Text(stringResource(R.string.security_title), style = body(15.sp, 600, Pc.BlueDark), modifier = Modifier.weight(1f))
                ChevronIcon(Pc.BlueDark)
            }
        }
        MainTabs(Route.Privacy, nav)
    }
}

@Composable
private fun Section(text: String) {
    SectionLabel(text, Modifier.padding(start = 4.dp, top = 20.dp, bottom = 8.dp))
}

@Composable
private fun VisibilityRow(title: String, subtitle: String?, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 52.dp)
            .selectable(selected, role = Role.RadioButton, onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TwoLines(title, subtitle, Modifier.weight(1f), body(16.sp, 500), body(12.sp, color = Pc.Muted))
        RadioButton(selected, onClick = null, colors = RadioButtonDefaults.colors(selectedColor = Pc.Blue))
    }
}

// ---------------------------------------------------------------- What your server sees

@Composable
fun ServerViewScreen(state: ServerViewState, nav: Navigator) {
    Page(
        top = { TopBar(stringResource(R.string.server_title), { nav.back() }, stringResource(R.string.cd_back)) },
        bottom = {
            PillButton(
                stringResource(R.string.security_title), { nav.go(Route.Security) },
                background = Pc.Card, content = Pc.Ink, border = Pc.Handle,
            )
        },
        spacing = 0,
    ) {
        Card(Modifier.padding(top = 8.dp), padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                IconBadge(PIcons.Servers, Pc.Ink, Pc.Chip, shape = RoundedCornerShape(12.dp))
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(state.server, style = body(15.sp, 600))
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        if (state.selfHosted) Tag(stringResource(R.string.server_self_hosted), Pc.Body, Pc.Chip)
                        Tag(privacyLabel(state.privacy), Pc.BlueDark, Pc.BlueTint, icon = PIcons.ShieldCheck)
                    }
                }
            }
        }
        Section(stringResource(R.string.server_can_see))
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
            state.canSee.forEachIndexed { i, t ->
                if (i > 0) RowDivider()
                Text(t, style = body(14.sp, lineHeight = 1.45f), modifier = Modifier.padding(vertical = 10.dp))
            }
        }
        Section(stringResource(R.string.server_never_sees))
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
            state.neverSees.forEachIndexed { i, t ->
                if (i > 0) RowDivider()
                Row(Modifier.padding(vertical = 10.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    Icon(PIcons.Close, null, tint = Pc.Blue, modifier = Modifier.padding(top = 1.dp).size(18.dp))
                    Text(t, style = body(14.sp, lineHeight = 1.45f))
                }
            }
        }
        // The map host is a separate operator: what it learns, and the per-region fallback.
        LocalMaps.current?.let { MapsServerSection(it) }
    }
}

// ---------------------------------------------------------------- How Pingers keeps you private

private data class Point(val icon: ImageVector, val tint: Color, val bg: Color, val title: Int, val body: Int)

private val points = listOf(
    Point(PIcons.Lock, Pc.BlueDark, Pc.BlueTint, R.string.sec_party_title, R.string.sec_party_body),
    Point(PIcons.Pin, Pc.BlueDark, Pc.BlueTint, R.string.sec_finding_title, R.string.sec_finding_body),
    Point(PIcons.Bell, Pc.BlueDark, Pc.BlueTint, R.string.sec_waking_title, R.string.sec_waking_body),
    Point(PIcons.Globe, Pc.BlueDark, Pc.BlueTint, R.string.sec_address_title, R.string.sec_address_body),
    Point(PIcons.People, Pc.OrangeText, Pc.OrangeTint, R.string.sec_meetups_title, R.string.sec_meetups_body),
    Point(PIcons.Eye, Pc.OrangeText, Pc.OrangeTint, R.string.sec_exact_title, R.string.sec_exact_body),
    Point(PIcons.Moon, Pc.Ink, Pc.Chip, R.string.sec_pauses_title, R.string.sec_pauses_body),
    Point(PIcons.Check, Pc.Ink, Pc.Chip, R.string.sec_account_title, R.string.sec_account_body),
)

@Composable
fun SecurityScreen(core: CoreInfo, nav: Navigator) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val docNote = stringResource(R.string.sec_doc_soon)
    Page(
        top = { TopBar(stringResource(R.string.privacy_title), { nav.back() }, stringResource(R.string.cd_back), titleSize = 15, titleColor = Pc.Muted) },
        spacing = 12,
    ) {
        Column(Modifier.padding(horizontal = 4.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                stringResource(R.string.security_title),
                style = heading(30.sp, 700).copy(lineHeight = 34.sp),
                modifier = Modifier.semantics { heading() },
            )
            Text(stringResource(R.string.sec_intro), style = body(15.sp, color = Pc.Body, lineHeight = 1.5f))
        }
        Spacer(Modifier.height(0.dp))
        points.forEach { p ->
            Card(padding = PaddingValues(16.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                    IconBadge(p.icon, p.tint, p.bg)
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(stringResource(p.title), style = body(16.sp, 600), modifier = Modifier.semantics { heading() })
                        Text(stringResource(p.body), style = body(14.sp, color = Pc.Body, lineHeight = 1.5f))
                    }
                }
            }
        }
        Text(stringResource(R.string.sec_limits), style = heading(20.sp), modifier = Modifier.padding(start = 4.dp, top = 12.dp).semantics { heading() })
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
            stringArrayResource(R.array.sec_limits_items).forEachIndexed { i, item ->
                if (i > 0) RowDivider()
                val (lead, rest) = item.split("|", limit = 2).let { it[0] to it.getOrElse(1) { "" } }
                Text(
                    buildAnnotatedString {
                        withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(lead) }
                        append(" ")
                        append(rest)
                    },
                    style = body(14.sp, lineHeight = 1.5f),
                    modifier = Modifier.padding(vertical = 11.dp),
                )
            }
        }
        Text(stringResource(R.string.sec_hood), style = heading(20.sp), modifier = Modifier.padding(start = 4.dp, top = 12.dp).semantics { heading() })
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
            val hood = stringArrayResource(R.array.sec_hood_items).map { item ->
                item.split("|", limit = 2).let { it[0] to it.getOrElse(1) { "" } }
            } + listOfNotNull(
                stringResource(R.string.sec_core) to stringResource(R.string.sec_core_version, core.version),
                core.bucket?.let { b ->
                    stringResource(R.string.sec_bucket) to
                        stringResource(R.string.sec_bucket_value, b.index.toString(), (b.lengthSeconds / 60u).toInt())
                },
            )
            hood.forEachIndexed { i, (k, v) ->
                if (i > 0) RowDivider()
                Row(Modifier.padding(vertical = 10.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(k, style = body(14.sp), modifier = Modifier.weight(1f))
                    Text(v, style = body(14.sp, color = Pc.Muted), textAlign = TextAlign.End, modifier = Modifier.weight(1f))
                }
            }
        }
        PillButton(
            stringResource(R.string.sec_read_doc), { android.widget.Toast.makeText(context, docNote, android.widget.Toast.LENGTH_SHORT).show() },
            Modifier.padding(top = 8.dp), background = Pc.Card, content = Pc.Ink, border = Pc.Handle,
        )
    }
}
