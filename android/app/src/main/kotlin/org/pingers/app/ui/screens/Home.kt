package org.pingers.app.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.platform.testTag
import org.pingers.app.ui.E2eTags
import org.pingers.app.R
import org.pingers.app.Navigator
import org.pingers.app.Route
import org.pingers.app.data.CoPresenceAlert
import org.pingers.app.data.HomeState
import org.pingers.app.data.LiveMeetupCard
import org.pingers.app.data.PartyRow
import org.pingers.app.data.PingersRepository
import org.pingers.app.ui.components.AvatarStack
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.IconAction
import org.pingers.app.ui.components.IconBadge
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SectionLabel
import org.pingers.app.ui.components.Tag
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.components.color
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import org.pingers.app.words

@Composable
fun HomeScreen(state: HomeState, repo: PingersRepository, nav: Navigator, simulateAlert: () -> Unit) {
    Column(Modifier.fillMaxSize().background(Pc.Paper)) {
        Column(
            Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .statusBarsPadding()
                .padding(horizontal = 20.dp)
                .padding(bottom = 20.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Row(Modifier.fillMaxWidth().padding(top = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.app_name),
                    style = heading(30.sp, 700, (-0.5).sp),
                    modifier = Modifier.weight(1f).semantics { heading() },
                )
                SettingsMenu(repo, nav, simulateAlert)
            }
            SharingCard(state, onClick = { nav.go(Route.Privacy) })
            state.alert?.let { AlertBanner(it, onOpen = { repo.dismissAlert(); nav.go(Route.Meetup) }, onHide = repo::hideMeHere) }
            state.liveMeetup?.let { LiveCard(it, onClick = { nav.go(Route.Meetup) }) }
            SectionLabel(stringResource(R.string.home_parties), Modifier.padding(top = 10.dp))
            Card {
                state.parties.forEachIndexed { i, p ->
                    if (i > 0) RowDivider()
                    PartyListRow(p) { repo.openParty(p.id); nav.go(Route.Party) }
                }
            }
            PillButton(stringResource(R.string.home_new_party), onClick = { nav.go(Route.NewParty) }, Modifier.testTag(E2eTags.HOME_NEW_PARTY), icon = PIcons.Plus)
        }
        MainTabs(Route.Home, nav)
    }
}

@Composable
private fun SettingsMenu(repo: PingersRepository, nav: Navigator, simulateAlert: () -> Unit) {
    var open by remember { mutableStateOf(false) }
    Box {
        IconAction(
            PIcons.Settings, stringResource(R.string.cd_settings), { open = true },
            background = Pc.Card, border = Pc.Line, iconSize = 20.dp,
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }, containerColor = Pc.Card) {
            DropdownMenuItem(text = { Text(stringResource(R.string.menu_privacy)) }, onClick = { open = false; nav.go(Route.Privacy) })
            HorizontalDivider(color = Pc.Divider)
            Text(
                stringResource(R.string.menu_preview),
                style = body(12.sp, 600, Pc.Muted),
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
            )
            DropdownMenuItem(text = { Text(stringResource(R.string.menu_invite_link)) }, onClick = { open = false; nav.go(Route.AcceptInvite) })
            DropdownMenuItem(text = { Text(stringResource(R.string.menu_alert)) }, onClick = { open = false; simulateAlert() })
            DropdownMenuItem(
                text = { Text(stringResource(R.string.menu_location_request)) },
                onClick = { open = false; repo.simulateLocationRequest(); nav.go(Route.LocationRequest) },
            )
            DropdownMenuItem(
                text = { Text(stringResource(R.string.menu_meetup_ended)) },
                onClick = { open = false; repo.simulateMeetupEnded(); nav.go(Route.MeetupEnded) },
            )
        }
    }
}

@Composable
private fun SharingCard(state: HomeState, onClick: () -> Unit) {
    val s = state.sharing
    val shape = RoundedCornerShape(18.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Pc.Card)
            .border(1.dp, Pc.Line, shape)
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        IconBadge(PIcons.Shield, Pc.Blue, Pc.BlueTint)
        val title = if (s.on) {
            pluralStringResource(R.plurals.home_sharing_on, s.partyCount, s.partyCount)
        } else {
            stringResource(R.string.home_sharing_off)
        }
        val detail = listOfNotNull(
            if (s.pausesInFocus) stringResource(R.string.home_pauses_in_focus) else null,
            pluralStringResource(R.plurals.home_zones, s.exclusionZones, s.exclusionZones),
        ).joinToString(" · ")
        TwoLines(title, detail, Modifier.weight(1f), body(15.sp, 600))
        Box(Modifier.size(10.dp).clip(CircleShape).background(if (s.on) Pc.Blue else Pc.Handle))
    }
}

@Composable
private fun LiveCard(m: LiveMeetupCard, onClick: () -> Unit) {
    val shape = RoundedCornerShape(22.dp)
    Column(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Pc.Ink)
            .clickable(onClick = onClick)
            .padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(Modifier.size(8.dp).clip(CircleShape).background(Pc.OrangeBright))
            Text(stringResource(R.string.home_live_meetup, m.partyName).uppercase(), style = body(13.sp, 600, Pc.OrangeOnInk))
        }
        Text(m.spotName, style = heading(24.sp).copy(color = Color.White))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.weight(1f)) { AvatarStack(m.people, 32.dp, 12, ring = Pc.Ink) }
            Text(
                stringResource(R.string.home_open),
                style = body(14.sp, 600, Pc.Ink),
                modifier = Modifier
                    .clip(RoundedCornerShape(999.dp))
                    .background(Pc.OrangeBright)
                    .padding(horizontal = 16.dp, vertical = 10.dp),
            )
        }
    }
}

/** The co-presence alert while the app is open: the notification's in-app state. */
@Composable
fun AlertBanner(
    alert: CoPresenceAlert,
    onOpen: () -> Unit,
    onHide: () -> Unit,
    modifier: Modifier = Modifier,
    background: Color = Pc.Card,
) {
    val w = words()
    Card(modifier, radius = 22.dp, background = background, border = if (background == Pc.Card) Pc.Line else null, padding = androidx.compose.foundation.layout.PaddingValues(14.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            IconBadge(PIcons.Signal, Color.White, Pc.Blue, size = 38.dp, shape = RoundedCornerShape(10.dp))
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Row(Modifier.fillMaxWidth()) {
                    Text(alert.partyName, style = body(14.sp, 700), modifier = Modifier.weight(1f))
                    Text(stringResource(R.string.alert_now), style = body(14.sp, color = Pc.Muted))
                }
                val names = w.names(alert.others)
                val text = stringResource(R.string.alert_body, names, alert.spotName)
                val at = text.indexOf(alert.spotName)
                Text(
                    buildAnnotatedString {
                        append(text)
                        if (at >= 0) addStyle(SpanStyle(fontWeight = FontWeight.Bold), at, at + alert.spotName.length)
                    },
                    style = body(15.sp, lineHeight = 1.4f),
                )
            }
        }
        Spacer(Modifier.height(12.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            AlertButton(stringResource(R.string.alert_open), Pc.Blue, Color.White, onOpen, Modifier.weight(1f).testTag(E2eTags.ALERT_OPEN))
            AlertButton(stringResource(R.string.alert_hide), Pc.ChipDark, Pc.Ink, onHide, Modifier.weight(1f))
        }
    }
}

@Composable
private fun AlertButton(text: String, bg: Color, fg: Color, onClick: () -> Unit, modifier: Modifier) {
    Box(
        modifier
            .height(48.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(bg)
            .clickable(role = Role.Button, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = body(15.sp, 600, fg)) }
}

@Composable
private fun PartyListRow(p: PartyRow, onClick: () -> Unit) {
    ListRow(onClick, Modifier.testTag(E2eTags.PARTY_ROW)) {
        Box(
            Modifier.size(44.dp).clip(RoundedCornerShape(14.dp)).background(p.colour.color()),
            contentAlignment = Alignment.Center,
        ) { Text(p.initials, style = heading(16.sp, 700).copy(color = Color.White)) }
        TwoLines(p.name, partyCounts(p.members, p.spots), Modifier.weight(1f))
        if (p.live) Tag(stringResource(R.string.tag_live), Pc.OrangeText, Pc.OrangeTint)
    }
}

/** The bottom bar on Home and Privacy: Parties, Meetups, Privacy. */
@Composable
fun MainTabs(selected: Route, nav: Navigator) {
    Column(Modifier.fillMaxWidth().background(Pc.Card)) {
        HorizontalDivider(color = Pc.Line)
        Row(Modifier.fillMaxWidth().navigationBarsPadding().height(66.dp)) {
            Tab(PIcons.People, stringResource(R.string.tab_parties), selected == Route.Home) { nav.reset(Route.Home) }
            Tab(PIcons.Pin, stringResource(R.string.tab_meetups), selected == Route.Meetup) { nav.go(Route.Meetup) }
            Tab(PIcons.Shield, stringResource(R.string.tab_privacy), selected == Route.Privacy) { nav.reset(Route.Privacy) }
        }
    }
}

@Composable
private fun androidx.compose.foundation.layout.RowScope.Tab(icon: ImageVector, label: String, on: Boolean, onClick: () -> Unit) {
    val color = if (on) Pc.Blue else Pc.Muted
    Column(
        Modifier
            .weight(1f)
            .fillMaxSize()
            .semantics { selected = on }
            .clickable(role = Role.Tab, onClick = onClick),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(4.dp, Alignment.CenterVertically),
    ) {
        Icon(icon, null, tint = color, modifier = Modifier.size(22.dp))
        Text(label, style = body(11.sp, 600, color))
    }
}
