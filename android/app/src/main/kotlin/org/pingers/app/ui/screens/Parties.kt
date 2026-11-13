package org.pingers.app.ui.screens

import android.content.Intent
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
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
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.AcceptInviteState
import org.pingers.app.data.AdvancedState
import org.pingers.app.data.Fence
import org.pingers.app.data.Invitation
import org.pingers.app.data.InviteState
import org.pingers.app.data.NewPartyState
import org.pingers.app.data.PartyColour
import org.pingers.app.data.PartyState
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.PrivacyLevel
import org.pingers.app.data.PushRelayChoice
import org.pingers.app.data.Reachability
import org.pingers.app.data.SpotRow
import org.pingers.app.ui.components.Avatar
import org.pingers.app.ui.components.AvatarStack
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.IconAction
import org.pingers.app.ui.components.IconBadge
import org.pingers.app.ui.components.LabeledField
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PSwitch
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SectionLabel
import org.pingers.app.ui.components.ServerCard
import org.pingers.app.ui.components.Tag
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.components.color
import org.pingers.app.ui.map.MapBox
import org.pingers.app.ui.map.Scenes
import org.pingers.app.ui.map.spotArea
import org.pingers.app.ui.map.spotCircle
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import org.pingers.app.words

/** "6 members · 3 spots". */
@Composable
fun partyCounts(members: Int, spots: Int): String =
    pluralStringResource(R.plurals.members, members, members) + " · " + pluralStringResource(R.plurals.spots, spots, spots)

@Composable
fun privacyLabel(p: PrivacyLevel?): String = stringResource(
    when (p) {
        PrivacyLevel.Full -> R.string.privacy_full
        PrivacyLevel.Reduced -> R.string.privacy_reduced
        null -> R.string.privacy_checking
    },
)

/** A page with a header, a scrolling body and a pinned bottom action. */
@Composable
fun Page(
    top: @Composable () -> Unit,
    bottom: (@Composable ColumnScope.() -> Unit)? = null,
    background: Color = Pc.Paper,
    bodyPadding: PaddingValues = PaddingValues(horizontal = 20.dp),
    spacing: Int = 14,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(Modifier.fillMaxSize().background(background)) {
        top()
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).padding(bodyPadding),
            verticalArrangement = Arrangement.spacedBy(spacing.dp),
        ) {
            content()
            if (bottom == null) Spacer(Modifier.navigationBarsPadding().height(20.dp))
        }
        if (bottom != null) {
            Column(
                Modifier.navigationBarsPadding().padding(start = 20.dp, end = 20.dp, top = 12.dp, bottom = 16.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
                content = bottom,
            )
        }
    }
}

// ---------------------------------------------------------------- New party

@Composable
fun NewPartyScreen(state: NewPartyState, repo: PingersRepository, nav: Navigator) {
    Page(
        top = { TopBar(stringResource(R.string.new_party_title), { nav.back() }, stringResource(R.string.cd_back)) },
        bottom = {
            state.error?.let { Text(it, style = body(14.sp, color = Pc.OrangeText)) }
            PillButton(stringResource(R.string.new_party_create), {
                repo.createParty()
                nav.replace(Route.Party)
                nav.go(Route.Invite)
            }, enabled = state.canCreate)
        },
        spacing = 18,
    ) {
        Spacer(Modifier.height(0.dp))
        LabeledField(
            stringResource(R.string.new_party_name), state.name, repo::setNewPartyName,
            background = Pc.Card, border = Pc.Blue, borderWidth = 1.5.dp, fontSize = 17,
        )
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.new_party_colour), style = body(13.sp, 600, Pc.Muted))
            Row(Modifier.selectableGroup(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                PartyColour.entries.forEach { c ->
                    ColourSwatch(c, c == state.colour) { repo.setNewPartyColour(c) }
                }
            }
        }
        ServerCard(
            stringResource(R.string.advanced_title),
            stringResource(R.string.server_and_privacy, state.server, privacyLabel(state.privacy)),
            onClick = { nav.go(Route.Advanced) },
        )
        Text(stringResource(R.string.new_party_later), style = body(14.sp, color = Pc.Muted, lineHeight = 1.5f))
    }
}

@Composable
private fun ColourSwatch(c: PartyColour, selected: Boolean, onClick: () -> Unit) {
    val name = stringResource(
        when (c) {
            PartyColour.Blue -> R.string.colour_blue
            PartyColour.Orange -> R.string.colour_orange
            PartyColour.Ink -> R.string.colour_ink
            PartyColour.Teal -> R.string.colour_teal
            PartyColour.Plum -> R.string.colour_plum
        },
    )
    Box(
        Modifier
            .size(48.dp)
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick)
            .semantics { contentDescription = name },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier
                .size(46.dp)
                .then(if (selected) Modifier.border(2.dp, c.color(), CircleShape).padding(2.dp).border(3.dp, Color.White, CircleShape) else Modifier)
                .clip(CircleShape)
                .background(c.color()),
        )
    }
}

// ---------------------------------------------------------------- Advanced

@Composable
fun AdvancedScreen(state: AdvancedState, repo: PingersRepository, nav: Navigator) {
    // Check the server and relay each time the screen opens, as the canvas does.
    LaunchedEffect(Unit) { repo.openAdvanced() }
    Page(
        top = { TopBar(stringResource(R.string.advanced_title), { nav.back() }, stringResource(R.string.cd_back_new_party)) },
        bottom = { PillButton(stringResource(R.string.done), { nav.go(Route.NewParty) }, background = Pc.Ink) },
    ) {
        Row(
            Modifier
                .padding(top = 8.dp)
                .fillMaxWidth()
                .clip(RoundedCornerShape(16.dp))
                .background(Pc.BlueTint)
                .padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(PIcons.ShieldCheck, null, tint = Pc.BlueDark, modifier = Modifier.size(22.dp))
            TwoLines(
                privacyLabel(state.privacy), stringResource(R.string.advanced_full_detail),
                titleStyle = body(15.sp, 600, Pc.BlueDark),
                subtitleStyle = body(13.sp, color = Pc.BlueInk, lineHeight = 1.4f),
            )
        }
        Spacer(Modifier.height(2.dp))
        LabeledField(
            stringResource(R.string.advanced_server), state.server, repo::setServer,
            background = Pc.Card, height = 52.dp,
            trailing = { ReachabilityMark(state.serverCheck, R.string.server_reachable, R.string.server_checking) },
            helper = stringResource(R.string.advanced_server_help),
        )
        LabeledField(
            stringResource(R.string.advanced_relay), state.relay, repo::setRelay,
            background = Pc.Card, height = 52.dp,
            trailing = { ReachabilityMark(state.relayCheck, R.string.relay_reachable, R.string.relay_checking) },
            helper = stringResource(R.string.advanced_relay_help),
        )
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(stringResource(R.string.advanced_push), style = body(13.sp, 600, Pc.Muted), modifier = Modifier.semantics { heading() })
            Card(radius = 14.dp, modifier = Modifier.selectableGroup()) {
                RadioRow(stringResource(R.string.advanced_push_official), state.pushRelay == PushRelayChoice.Official) {
                    repo.setPushRelay(PushRelayChoice.Official)
                }
                RowDivider()
                RadioRow(stringResource(R.string.advanced_push_custom), state.pushRelay == PushRelayChoice.Custom) {
                    repo.setPushRelay(PushRelayChoice.Custom)
                }
            }
            Text(stringResource(R.string.advanced_push_help), style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f))
        }
        Card(radius = 14.dp) {
            Row(
                Modifier
                    .clickable(role = Role.Switch) { repo.setPostQuantum(!state.postQuantum) }
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                TwoLines(
                    stringResource(R.string.advanced_pq), stringResource(R.string.advanced_pq_help), Modifier.weight(1f),
                    body(15.sp, 500), body(12.sp, color = Pc.Muted, lineHeight = 1.4f),
                )
                PSwitch(state.postQuantum, null)
            }
        }
    }
}

@Composable
private fun ReachabilityMark(r: Reachability, ok: Int, checking: Int) {
    val label = stringResource(if (r == Reachability.Reachable) ok else checking)
    Box(
        Modifier.size(24.dp).semantics {
            contentDescription = label
            liveRegion = LiveRegionMode.Polite
        },
        contentAlignment = Alignment.Center,
    ) {
        when (r) {
            Reachability.Checking -> CircularProgressIndicator(
                modifier = Modifier.size(20.dp), color = Pc.Blue, trackColor = Pc.Line, strokeWidth = 3.dp,
            )
            Reachability.Reachable -> Icon(PIcons.CheckCircle, null, tint = Color.Unspecified, modifier = Modifier.size(22.dp))
            Reachability.Unreachable -> Icon(PIcons.Close, null, tint = Pc.OrangeText, modifier = Modifier.size(20.dp))
        }
    }
}

@Composable
fun RadioRow(text: String, selected: Boolean, fontSize: Int = 15, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected = selected, onClick = null, colors = RadioButtonDefaults.colors(selectedColor = Pc.Blue), modifier = Modifier.padding(horizontal = 10.dp))
        Text(text, style = body(fontSize.sp))
    }
}

// ---------------------------------------------------------------- Invite people

private val QrCode: ImageVector = ImageVector.Builder("qr", 160.dp, 160.dp, 21f, 21f).apply {
    addPath(addPathNodes("M0 0h7v7H0zM14 0h7v7h-7zM0 14h7v7H0z"), fill = SolidColor(Pc.Ink))
    addPath(addPathNodes("M1 1h5v5H1zM15 1h5v5h-5zM1 15h5v5H1z"), fill = SolidColor(Color.White))
    addPath(
        addPathNodes(
            "M2 2h3v3H2zM16 2h3v3h-3zM2 16h3v3H2zM8 0h1v2H8zM10 1h2v1h-2zM9 3h3v1H9zM8 5h1v3H8zM11 5h2v2h-2zM0 8h2v1H0zM3 8h3v1H3zM9 9h3v1H9zM13 8h2v2h-2zM16 8h3v1h-3zM19 10h2v2h-2zM1 10h2v2H1zM5 10h2v1H5zM8 11h1v3H8zM10 12h3v1h-3zM14 11h2v2h-2zM17 12h1v3h-1zM9 15h2v2H9zM12 14h2v1h-2zM15 15h2v2h-2zM19 14h2v1h-2zM8 18h3v1H8zM12 17h1v4h-1zM14 18h3v1h-3zM18 17h2v2h-2zM15 20h2v1h-2zM19 20h2v1h-2z",
        ),
        fill = SolidColor(Pc.Ink),
    )
}.build()

@Composable
fun InviteScreen(state: InviteState, repo: PingersRepository, nav: Navigator) {
    val context = LocalContext.current
    val shareTitle = stringResource(R.string.invite_share_title, state.partyName)
    Page(
        top = {
            TopBar(
                stringResource(R.string.invite_title), { nav.go(Route.Party) },
                stringResource(R.string.cd_back_to, state.partyName), subtitle = state.partyName,
            )
        },
        bottom = { PillButton(stringResource(R.string.done), { nav.go(Route.Party) }, background = Pc.Ink) },
        spacing = 16,
    ) {
        Card(radius = 22.dp, padding = PaddingValues(20.dp)) {
            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp)) {
                Image(QrCode, stringResource(R.string.cd_invite_qr), Modifier.size(160.dp))
                Text(
                    state.expiresInHours?.let { stringResource(R.string.invite_scan, it) } ?: stringResource(R.string.invite_making),
                    style = body(13.sp, color = Pc.Muted, lineHeight = 1.45f),
                    textAlign = TextAlign.Center,
                )
                PillButton(stringResource(R.string.invite_share), {
                    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, state.link)
                    context.startActivity(Intent.createChooser(send, shareTitle))
                }, height = 50.dp, icon = PIcons.Share, enabled = state.link != null)
            }
        }
        SectionLabel(stringResource(R.string.invite_invitations), Modifier.padding(start = 4.dp, top = 4.dp))
        Card {
            state.invitations.forEachIndexed { i, inv ->
                if (i > 0) RowDivider()
                InvitationRow(inv, repo)
            }
        }
    }
}

@Composable
private fun InvitationRow(inv: Invitation, repo: PingersRepository) {
    ListRow(null, padding = PaddingValues(horizontal = 16.dp, vertical = 12.dp)) {
        when (inv) {
            is Invitation.Opened -> {
                Avatar(inv.person, 40.dp, 13)
                TwoLines(inv.person.name, stringResource(R.string.invite_opened, inv.code), Modifier.weight(1f), body(15.sp, 600))
                Box(
                    Modifier
                        .height(48.dp)
                        .clickable(role = Role.Button) { repo.verifyInvite(inv.id) },
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        stringResource(R.string.invite_verify),
                        style = body(14.sp, 600, Pc.Blue),
                        modifier = Modifier
                            .border(1.dp, Pc.Blue, RoundedCornerShape(18.dp))
                            .padding(horizontal = 14.dp, vertical = 8.dp),
                    )
                }
            }
            is Invitation.Pending -> {
                IconBadge(PIcons.Link, Pc.Muted, Pc.Chip, iconSize = 18.dp)
                TwoLines(
                    stringResource(R.string.invite_link_shared, inv.sharedHoursAgo),
                    stringResource(R.string.invite_not_opened, inv.expiresInHours),
                    Modifier.weight(1f), body(15.sp, 600),
                )
                IconAction(PIcons.Close, stringResource(R.string.cd_cancel_invite), { repo.cancelInvite(inv.id) }, background = Pc.Chip, iconSize = 16.dp)
            }
            is Invitation.Joined -> {
                Avatar(inv.person, 40.dp, 13)
                TwoLines(
                    inv.person.name, stringResource(R.string.invite_joined), Modifier.weight(1f), body(15.sp, 600),
                    body(13.sp, color = Pc.GreenText),
                )
                Icon(PIcons.CheckCircle, null, tint = Color.Unspecified, modifier = Modifier.size(22.dp))
            }
        }
    }
}

// ---------------------------------------------------------------- Accept invite

@Composable
fun AcceptInviteScreen(state: AcceptInviteState, repo: PingersRepository, nav: Navigator) {
    Page(
        top = { Spacer(Modifier.statusBarsPadding().height(28.dp)) },
        bottom = {
            PillButton(stringResource(R.string.accept_join), { repo.acceptInvite(); nav.reset(Route.Party) })
            PillButton(
                stringResource(R.string.accept_decline), { repo.declineInvite(); nav.reset(Route.Home) },
                background = Color.Transparent, content = Pc.Ink, height = 48.dp,
            )
        },
        bodyPadding = PaddingValues(horizontal = 24.dp),
        spacing = 22,
    ) {
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Box(
                Modifier.size(76.dp).clip(RoundedCornerShape(24.dp)).background(state.colour.color()),
                contentAlignment = Alignment.Center,
            ) { Text(state.partyInitials, style = heading(30.sp, 700).copy(color = Color.White)) }
            Text(stringResource(R.string.accept_invited_by, state.inviter), style = body(15.sp, color = Pc.Muted))
            Text(state.partyName, style = heading(30.sp, 700), modifier = Modifier.semantics { heading() })
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AvatarStack(state.preview, 30.dp, 11, ring = Pc.Paper, extra = "+${state.moreMembers}")
                Text(
                    partyCounts(state.members, state.spots),
                    style = body(14.sp, color = Pc.Muted),
                    modifier = Modifier.padding(start = 0.dp),
                )
            }
        }
        Card(radius = 20.dp, padding = PaddingValues(18.dp)) {
            Text(stringResource(R.string.accept_what_joining), style = body(15.sp, 600), modifier = Modifier.semantics { heading() })
            Spacer(Modifier.height(14.dp))
            val spot = stringResource(R.string.word_spot)
            val first = stringResource(R.string.accept_point_spot, spot)
            val at = first.indexOf(spot)
            JoinPoint(PIcons.Pin) {
                Text(
                    buildAnnotatedString {
                        append(first)
                        if (at >= 0) addStyle(SpanStyle(fontWeight = FontWeight.Bold), at, at + spot.length)
                    },
                    style = body(14.sp, lineHeight = 1.45f),
                )
            }
            Spacer(Modifier.height(14.dp))
            JoinPoint(PIcons.Lock) { Text(stringResource(R.string.accept_point_e2e), style = body(14.sp, lineHeight = 1.45f)) }
            Spacer(Modifier.height(14.dp))
            JoinPoint(PIcons.Moon) { Text(stringResource(R.string.accept_point_focus), style = body(14.sp, lineHeight = 1.45f)) }
        }
        Column(
            Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(Pc.BlueTint).padding(horizontal = 18.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(stringResource(R.string.accept_check_code, state.inviter), style = body(13.sp, 600, Pc.BlueDark))
            Text(state.safetyCode, style = heading(28.sp, 600, 2.sp))
        }
    }
}

@Composable
private fun JoinPoint(icon: ImageVector, text: @Composable () -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Icon(icon, null, tint = Pc.Blue, modifier = Modifier.padding(top = 1.dp).size(20.dp))
        text()
    }
}

// ---------------------------------------------------------------- Party

@Composable
fun PartyScreen(state: PartyState, repo: PingersRepository, nav: Navigator) {
    Page(
        top = {
            TopBar(state.name, { nav.back() }, stringResource(R.string.cd_back)) {
                IconAction(PIcons.PersonAdd, stringResource(R.string.cd_invite_people), { nav.go(Route.Invite) })
            }
        },
        bodyPadding = PaddingValues(0.dp),
        spacing = 0,
    ) {
        Row(
            Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 20.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            state.members.forEach { p ->
                Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Avatar(p, 48.dp, 14)
                    Text(p.name, style = body(12.sp, color = Pc.Muted))
                }
            }
        }
        Box(
            Modifier
                .padding(start = 20.dp, end = 20.dp, top = 16.dp)
                .clip(RoundedCornerShape(22.dp))
                .border(1.dp, Pc.Line, RoundedCornerShape(22.dp)),
        ) {
            MapBox(Scenes.PartyCard, overlay = {
                val frame = state.mapFrame
                state.spots.forEach { spot ->
                    when (val fence = spot.fence) {
                        is Fence.Circle -> {
                            val radius = frame.units(fence.radiusM)
                            spotCircle(frame.toMap(fence.center), radius, dashed = true, small = radius < 20f)
                        }
                        is Fence.Polygon -> spotArea(fence.vertices.map(frame::toMap))
                    }
                }
            })
            Row(
                Modifier
                    .align(Alignment.BottomStart)
                    .padding(12.dp)
                    .clip(RoundedCornerShape(999.dp))
                    .background(Pc.Card)
                    .padding(horizontal = 12.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Icon(PIcons.Lock, null, tint = Pc.Ink, modifier = Modifier.size(14.dp))
                Text(stringResource(R.string.party_spots_private), style = body(12.sp, 600))
            }
        }
        Row(Modifier.fillMaxWidth().padding(start = 20.dp, end = 12.dp, top = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            SectionLabel(stringResource(R.string.party_spots), Modifier.weight(1f))
            Box(
                Modifier
                    .heightIn(min = 48.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .clickable(role = Role.Button) { repo.editSpot(null); nav.go(Route.SpotEditor) }
                    .padding(horizontal = 8.dp),
                contentAlignment = Alignment.Center,
            ) { Text(stringResource(R.string.party_add_spot), style = body(15.sp, 600, Pc.Blue)) }
        }
        Card(Modifier.padding(horizontal = 20.dp)) {
            state.spots.forEachIndexed { i, s ->
                if (i > 0) RowDivider()
                SpotListRow(s) {
                    if (s.inMeetup) {
                        nav.go(Route.Meetup)
                    } else {
                        repo.editSpot(s.id)
                        nav.go(Route.SpotEditor)
                    }
                }
            }
        }
        Text(
            stringResource(R.string.party_meetups_note),
            style = body(13.sp, color = Pc.Muted, lineHeight = 1.45f),
            modifier = Modifier.padding(start = 24.dp, end = 24.dp, top = 14.dp),
        )
        ServerCard(
            stringResource(R.string.server_and_privacy, state.server, privacyLabel(state.privacy)),
            stringResource(R.string.party_server_subtitle),
            onClick = { nav.go(Route.ServerView) },
            modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 14.dp),
        )
    }
}

@Composable
private fun SpotListRow(s: SpotRow, onClick: () -> Unit) {
    val w = words()
    ListRow(onClick) {
        TwoLines(s.name, w.shape(s.shape), Modifier.weight(1f))
        if (s.inMeetup) Tag(stringResource(R.string.party_in_meetup), Pc.OrangeText, Pc.OrangeTint)
    }
}
