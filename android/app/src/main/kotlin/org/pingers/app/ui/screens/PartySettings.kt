package org.pingers.app.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.CreateOutcome
import org.pingers.app.data.MemberId
import org.pingers.app.data.PingersRepository
import org.pingers.app.ui.E2eTags
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.ChevronIcon
import org.pingers.app.ui.components.IconAction
import org.pingers.app.ui.components.InitialsCircle
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.PField
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SheetHandle
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.components.color
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import uniffi.pingers.LeavePartyView
import uniffi.pingers.PartyCodeView
import uniffi.pingers.PartySettingsView
import uniffi.pingers.PrivacyLevel
import uniffi.pingers.SafetyCodesView
import uniffi.pingers.ServiceRowView

/*
 * One Party's settings (design canvas section 4): Party settings in full or reduced privacy,
 * the Leave party sheet, Safety codes, and "This server needs a code" for New party. Every word
 * comes from the core's records.
 */

// ---------------------------------------------------------------- Party settings

@Composable
fun PartySettingsScreen(state: PartySettingsView?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Waiting()
    Page(
        top = { TopBar(state.title, { nav.back() }, state.back, subtitle = state.subtitle) },
        bottom = {
            PillButton(
                state.leave, { nav.go(Route.LeaveParty) }, Modifier.testTag(E2eTags.PARTY_LEAVE),
                background = Pc.Card, content = Pc.Red, border = Pc.Handle,
            )
        },
        spacing = 0,
    ) {
        Card(Modifier.padding(top = 10.dp).clickable { nav.go(Route.Invite) }.testTag(E2eTags.PARTY_SETTINGS_INVITE)) {
            Row(
                Modifier.padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Icon(PIcons.PersonAdd, null, tint = Pc.Ink, modifier = Modifier.size(22.dp))
                TwoLines(state.invite, state.members, Modifier.weight(1f), body(16.sp, 500), body(13.sp, color = Pc.Muted))
                ChevronIcon()
            }
        }
        Heading(state.operatorsHeading)
        PrivacyBanner(state) { nav.go(Route.Wakeups) }
        Card(Modifier.padding(top = 10.dp)) {
            ServiceRow(state.server)
            RowDivider()
            ServiceRow(state.relay) {
                state.relayCheck?.let { check ->
                    Row(Modifier.padding(top = 2.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        if (state.relayCheckOk) Icon(PIcons.CheckCircle, null, tint = Color.Unspecified, modifier = Modifier.size(14.dp))
                        Text(check, style = body(12.sp, 600, if (state.relayCheckOk) Pc.GreenText else Pc.OrangeText))
                    }
                }
            }
            RowDivider()
            ServiceRow(state.pushRelay)
            RowDivider()
            Box(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = 52.dp)
                    .clickable(role = Role.Button) { repo.checkRelayAgain() }
                    .testTag(E2eTags.PARTY_CHECK_RELAY)
                    .padding(horizontal = 16.dp, vertical = 14.dp),
                contentAlignment = Alignment.CenterStart,
            ) { Text(state.checkAgain, style = body(15.sp, 600, Pc.Blue)) }
        }
        Note(state.operatorsNote, top = 10.dp, size = 12)
        Card(Modifier.padding(top = 14.dp)) {
            LinkRow(state.serverSees, null, { nav.go(Route.ServerView) })
            RowDivider()
            LinkRow(state.safetyCodes, state.safetySummary, { nav.go(Route.SafetyCodes) }, Modifier.testTag(E2eTags.SAFETY_CODES_OPEN))
            RowDivider()
            LinkRow(state.security, state.securitySummary, { nav.go(Route.Security) })
        }
    }
}

/** "Full privacy" in blue, or "Reduced privacy" in orange with what it costs and the fix. */
@Composable
private fun PrivacyBanner(state: PartySettingsView, onWakeups: () -> Unit) {
    val shape = RoundedCornerShape(16.dp)
    if (state.privacy == PrivacyLevel.FULL) {
        Row(
            Modifier.fillMaxWidth().clip(shape).background(Pc.BlueTint).padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(PIcons.ShieldCheck, null, tint = Pc.BlueDark, modifier = Modifier.size(22.dp))
            TwoLines(state.privacyTitle, state.privacyDetail, titleStyle = body(15.sp, 600, Pc.BlueDark), subtitleStyle = body(13.sp, color = Pc.BlueInk, lineHeight = 1.4f))
        }
        return
    }
    Column(
        Modifier.fillMaxWidth().clip(shape).background(Pc.OrangeTintSoft).border(1.dp, Pc.OrangeLine, shape).padding(horizontal = 14.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Icon(PIcons.ShieldAlert, null, tint = Pc.OrangeText, modifier = Modifier.size(22.dp))
            TwoLines(state.privacyTitle, state.privacyDetail, titleStyle = body(15.sp, 600, Pc.OrangeText), subtitleStyle = body(13.sp, color = Pc.OrangeBrown, lineHeight = 1.4f))
        }
        if (state.consequenceBefore != null) {
            Text(
                buildAnnotatedString {
                    append(state.consequenceBefore.orEmpty())
                    withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(state.consequenceBold.orEmpty()) }
                    append(state.consequenceAfter.orEmpty())
                },
                style = body(13.sp, color = Pc.OrangeBrown, lineHeight = 1.45f),
                modifier = Modifier.padding(start = 32.dp),
            )
        }
        state.wakeupsLink?.let { link ->
            Box(
                Modifier.padding(start = 32.dp).heightIn(min = 32.dp).clickable(role = Role.Button, onClick = onWakeups).testTag(E2eTags.PARTY_WAKEUPS_LINK),
                contentAlignment = Alignment.CenterStart,
            ) { Text(link, style = body(14.sp, 600, Pc.OrangeText)) }
        }
    }
}

@Composable
private fun ServiceRow(row: ServiceRowView, extra: @Composable () -> Unit = {}) {
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(row.title, style = body(15.sp, 600))
            Text(row.detail, style = body(13.sp, color = Pc.Muted))
            extra()
        }
        Text(
            row.operator,
            style = body(13.sp, 600, if (row.shared) Pc.OrangeText else Pc.Body),
            maxLines = 1,
            modifier = Modifier
                .clip(RoundedCornerShape(999.dp))
                .background(if (row.shared) Pc.OrangeTint else Pc.Chip)
                .padding(horizontal = 10.dp, vertical = 4.dp),
        )
    }
}

// ---------------------------------------------------------------- Leave party (a sheet over Party settings)

@Composable
fun LeavePartySheet(state: LeavePartyView?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return
    Box(Modifier.fillMaxSize()) {
        Box(
            Modifier
                .fillMaxSize()
                .background(Pc.Ink.copy(alpha = 0.45f))
                .clickable(remember { MutableInteractionSource() }, indication = null) { nav.back() },
        )
        Column(
            Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .clip(RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp))
                .background(Pc.Card)
                .clickable(remember { MutableInteractionSource() }, indication = null) {}
                .verticalScroll(rememberScrollState())
                .navigationBarsPadding()
                .padding(start = 20.dp, end = 20.dp, top = 10.dp, bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            SheetHandle(Modifier.align(Alignment.CenterHorizontally))
            Text(state.title, style = heading(24.sp), modifier = Modifier.semantics { heading() })
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                state.points.forEachIndexed { i, point ->
                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        Icon(LeaveIcons.getOrElse(i) { PIcons.Lock }, null, tint = Pc.Muted, modifier = Modifier.padding(top = 1.dp).size(20.dp))
                        Text(point, style = body(15.sp, lineHeight = 1.45f))
                    }
                }
            }
            state.meetupWarning?.let { warning ->
                Row(
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(Pc.OrangeTintSoft).padding(horizontal = 14.dp, vertical = 12.dp),
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                ) {
                    Icon(PIcons.AlertCircle, null, tint = Pc.OrangeText, modifier = Modifier.padding(top = 1.dp).size(18.dp))
                    Text(warning, style = body(14.sp, color = Pc.OrangeBrown, lineHeight = 1.4f))
                }
            }
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                PillButton(state.leave, { repo.leaveParty { nav.reset(Route.Home) } }, Modifier.testTag(E2eTags.LEAVE_PARTY_CONFIRM), background = Pc.Red)
                PillButton(state.cancel, { nav.back() }, background = Color.Transparent, content = Pc.Ink, height = 48.dp)
            }
        }
    }
}

/** The points' icons, in the order pingers-words lists them: Spots, keys, coming back. */
private val LeaveIcons: List<ImageVector> = listOf(PIcons.Pin, PIcons.Lock, PIcons.PersonMinus)

// ---------------------------------------------------------------- Safety codes

@Composable
fun SafetyCodesScreen(state: SafetyCodesView?, repo: PingersRepository, nav: Navigator) {
    LaunchedEffect(Unit) { repo.openSafetyCodes() }
    if (state == null) return Waiting()
    Page(top = { TopBar(state.title, { nav.back() }, stringResource(R.string.cd_back), subtitle = state.subtitle) }, spacing = 0) {
        if (state.codeHeading != null) {
            Column(
                Modifier.padding(top = 10.dp).fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(Pc.BlueTint).padding(18.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text(state.codeHeading.orEmpty(), style = body(13.sp, 600, Pc.BlueDark))
                Column(Modifier.testTag(E2eTags.SAFETY_CODE)) {
                    state.codeGroups.chunked(4).forEach { line ->
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                            line.forEach { Text(it, style = heading(19.sp, spacing = 1.sp).copy(lineHeight = 30.sp)) }
                        }
                    }
                }
                state.compare?.let { Text(it, style = body(13.sp, color = Pc.BlueDark, lineHeight = 1.45f)) }
                state.mark?.let {
                    PillButton(it, { repo.markChecked() }, Modifier.testTag(E2eTags.SAFETY_MARK_CHECKED), height = 44.dp, fontSize = 15)
                }
                state.checkedNote?.let {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Icon(PIcons.CheckCircle, null, tint = Color.Unspecified, modifier = Modifier.size(20.dp))
                        Text(it, style = body(15.sp, 600, Pc.GreenText))
                    }
                }
            }
        }
        Heading(state.membersHeading)
        Card {
            state.members.forEachIndexed { i, m ->
                if (i > 0) RowDivider()
                Row(
                    Modifier
                        .fillMaxWidth()
                        .background(if (m.selected) Pc.BlueTintSoft else Color.Transparent)
                        .clickable { repo.showSafetyCode(MemberId(m.member.leaf.toLong())) }
                        .padding(horizontal = 16.dp, vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    InitialsCircle(m.avatar, m.tint.toUi().color(), 36.dp, 13)
                    Text(m.name, style = body(15.sp, if (m.selected) 600 else 500), modifier = Modifier.weight(1f))
                    Text(m.state, style = body(13.sp, if (m.checked) 600 else 400, if (m.checked) Pc.GreenText else Pc.Muted))
                }
            }
        }
        Note(state.footnote)
    }
}

private fun uniffi.pingers.Tint.toUi() = when (this) {
    uniffi.pingers.Tint.PEACH -> org.pingers.app.data.Tint.Peach
    uniffi.pingers.Tint.SKY -> org.pingers.app.data.Tint.Sky
    uniffi.pingers.Tint.SAGE -> org.pingers.app.data.Tint.Sage
    uniffi.pingers.Tint.BUTTER -> org.pingers.app.data.Tint.Butter
    uniffi.pingers.Tint.LILAC -> org.pingers.app.data.Tint.Lilac
    uniffi.pingers.Tint.MINT -> org.pingers.app.data.Tint.Mint
    uniffi.pingers.Tint.STONE -> org.pingers.app.data.Tint.Stone
}

// ---------------------------------------------------------------- This server needs a code

@Composable
fun PartyCodeScreen(state: PartyCodeView?, code: String, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Waiting()
    val refused = state.refused
    Column(Modifier.fillMaxSize().background(Pc.Paper)) {
        Row(Modifier.statusBarsPadding().padding(start = 10.dp, top = 12.dp)) {
            IconAction(PIcons.Back, state.back, { nav.back() })
        }
        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp)) {
            Column(Modifier.padding(horizontal = 4.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text(state.title, style = heading(28.sp, 700), modifier = Modifier.semantics { heading() })
                Text(state.body, style = body(15.sp, color = Pc.Muted, lineHeight = 1.45f))
            }
            Card(Modifier.padding(top = 18.dp), padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Box(Modifier.size(40.dp).clip(RoundedCornerShape(12.dp)).background(Pc.Chip), contentAlignment = Alignment.Center) {
                        Icon(PIcons.Servers, null, tint = Pc.Ink, modifier = Modifier.size(20.dp))
                    }
                    TwoLines(state.server, state.operator, titleStyle = body(15.sp, 600), subtitleStyle = body(13.sp, color = Pc.Muted))
                }
            }
            Card(Modifier.padding(top = 14.dp), padding = PaddingValues(16.dp)) {
                Text(state.codeLabel, style = body(13.sp, 600, Pc.Muted))
                Spacer(Modifier.height(10.dp))
                PField(
                    code, repo::setRegistrationCode, state.codeLabel, Modifier.testTag(E2eTags.PARTY_CODE_FIELD),
                    background = Pc.Card, border = if (refused != null) Pc.Red else Pc.Blue, borderWidth = 2.dp, height = 48.dp,
                    shape = RoundedCornerShape(12.dp),
                    textStyle = heading(18.sp, spacing = 0.5.sp),
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false,
                        keyboardType = KeyboardType.Password, imeAction = ImeAction.Done,
                    ),
                )
                Spacer(Modifier.height(10.dp))
                if (refused != null) {
                    ErrorLine(refused)
                    state.nothingCreated?.let {
                        Spacer(Modifier.height(10.dp))
                        Text(it, style = body(13.sp, color = Pc.Muted, lineHeight = 1.45f))
                    }
                } else {
                    Text(state.codeHelp, style = body(13.sp, color = Pc.Muted, lineHeight = 1.45f))
                }
            }
            Row(
                Modifier.padding(top = 14.dp).fillMaxWidth().clip(RoundedCornerShape(18.dp)).background(Pc.BlueTint).padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Icon(PIcons.LinkSmall, null, tint = Pc.BlueDark, modifier = Modifier.size(22.dp))
                Text(state.invitees, style = body(14.sp, color = Pc.BlueDark, lineHeight = 1.4f), modifier = Modifier.weight(1f))
            }
        }
        Column(Modifier.navigationBarsPadding().padding(start = 20.dp, end = 20.dp, top = 12.dp, bottom = 16.dp)) {
            MainButton(state.create, state.canCreate, {
                repo.createParty { outcome ->
                    if (outcome == CreateOutcome.Created) {
                        nav.go(Route.NewParty)
                        nav.replace(Route.Party)
                        nav.go(Route.Invite)
                    }
                }
            }, Modifier.testTag(E2eTags.PARTY_CODE_CREATE))
        }
    }
}
