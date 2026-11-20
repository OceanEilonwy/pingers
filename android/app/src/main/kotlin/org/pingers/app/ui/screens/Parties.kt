package org.pingers.app.ui.screens

import org.pingers.app.SecureScreen
import org.pingers.app.ui.map.LocalMaps
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import org.pingers.app.data.CreateOutcome
import org.pingers.app.ui.components.PField
import uniffi.pingers.ServerChoice
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
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import androidx.compose.ui.platform.testTag
import org.pingers.app.ui.E2eTags
import org.pingers.app.R
import org.pingers.app.Route
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
    // Every word is the core's (canvas CreateParty); nothing shows until it has answered.
    val text = state.text ?: return Waiting()
    Page(
        top = { TopBar(text.title, { nav.back() }, stringResource(R.string.cd_back)) },
        bottom = {
            state.error?.let { Text(it, style = body(14.sp, color = Pc.OrangeText)) }
            PillButton(text.create, {
                repo.createParty { outcome ->
                    when (outcome) {
                        CreateOutcome.Created -> {
                            nav.replace(Route.Party)
                            nav.go(Route.Invite)
                        }
                        CreateOutcome.CodeNeeded -> nav.go(Route.PartyCode)
                        CreateOutcome.CodeRefused, CreateOutcome.Failed -> {}
                    }
                }
            }, enabled = state.canCreate, modifier = Modifier.testTag(E2eTags.NEW_PARTY_CREATE))
        },
        spacing = 18,
    ) {
        Spacer(Modifier.height(0.dp))
        LabeledField(
            text.nameLabel, state.name, repo::setNewPartyName,
            background = Pc.Card, border = Pc.Blue, borderWidth = 1.5.dp, fontSize = 17,
        )
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(text.colourLabel, style = body(13.sp, 600, Pc.Muted))
            Row(Modifier.selectableGroup(), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                PartyColour.entries.forEachIndexed { i, c ->
                    ColourSwatch(c, text.colourNames.getOrElse(i) { "" }, c == state.colour) { repo.setNewPartyColour(c) }
                }
            }
        }
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(text.serverLabel, style = body(13.sp, 600, Pc.Muted), modifier = Modifier.semantics { heading() })
            Card(radius = 16.dp, modifier = Modifier.selectableGroup()) {
                ServerChoiceRow(text.builtIn, text.builtInDetail, state.serverChoice == ServerChoice.BUILT_IN, E2eTags.NEW_PARTY_BUILT_IN) {
                    repo.setServerChoice(ServerChoice.BUILT_IN)
                }
                RowDivider()
                ServerChoiceRow(text.another, text.anotherDetail, state.serverChoice == ServerChoice.ANOTHER, E2eTags.NEW_PARTY_ANOTHER) {
                    repo.setServerChoice(ServerChoice.ANOTHER)
                }
            }
            if (state.serverChoice == ServerChoice.ANOTHER) {
                PField(
                    state.server, repo::setServer, text.another, Modifier.testTag(E2eTags.NEW_PARTY_SERVER),
                    background = Pc.Card, height = 48.dp, placeholder = text.anotherPlaceholder,
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, keyboardType = KeyboardType.Uri),
                )
                // The first time this phone uses a server, its keys are trusted only when you say so.
                state.keysLine?.let { Text(it, style = body(13.sp, color = Pc.Muted, lineHeight = 1.4f)) }
                state.keysTrust?.let {
                    PillButton(
                        it, { repo.trustServerKeys() }, Modifier.testTag(E2eTags.NEW_PARTY_TRUST_KEYS),
                        background = Pc.Card, content = Pc.Blue, border = Pc.Handle,
                    )
                }
            }
        }
        ServerCard(text.advanced, text.advancedDetail, onClick = { nav.go(Route.Advanced) }, privacy = state.privacy)
        Text(text.later, style = body(14.sp, color = Pc.Muted, lineHeight = 1.5f))
    }
}

/** "The app's server" or "Another server", with its line and a radio button on the right. */
@Composable
private fun ServerChoiceRow(title: String, detail: String, selected: Boolean, tag: String, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick)
            .testTag(tag)
            .padding(horizontal = 14.dp, vertical = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        TwoLines(title, detail, Modifier.weight(1f), body(15.sp, 500), body(12.sp, color = Pc.Muted))
        RadioButton(selected = selected, onClick = null, colors = RadioButtonDefaults.colors(selectedColor = Pc.Blue))
    }
}

@Composable
private fun ColourSwatch(c: PartyColour, name: String, selected: Boolean, onClick: () -> Unit) {
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
    // Every word is the core's (canvas PartyAdvanced); nothing shows until it has answered.
    val text = state.text ?: return Waiting()
    val another = state.serverChoice == ServerChoice.ANOTHER
    Page(
        top = { TopBar(text.title, { nav.back() }, stringResource(R.string.cd_back_new_party)) },
        bottom = { PillButton(text.done, { nav.go(Route.NewParty) }, background = Pc.Ink) },
        spacing = 10,
    ) {
        val reduced = state.privacy == PrivacyLevel.Reduced
        val shape = RoundedCornerShape(16.dp)
        Row(
            Modifier
                .padding(top = 8.dp, bottom = 2.dp)
                .fillMaxWidth()
                .clip(shape)
                .background(if (reduced) Pc.OrangeTintSoft else Pc.BlueTint)
                .then(if (reduced) Modifier.border(1.dp, Pc.OrangeLine, shape) else Modifier)
                .padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(if (reduced) PIcons.ShieldAlert else PIcons.ShieldCheck, null, tint = if (reduced) Pc.OrangeText else Pc.BlueDark, modifier = Modifier.size(22.dp))
            TwoLines(
                text.privacy, text.privacyDetail,
                titleStyle = body(15.sp, 600, if (reduced) Pc.OrangeText else Pc.BlueDark),
                subtitleStyle = body(13.sp, color = if (reduced) Pc.OrangeBrown else Pc.BlueInk, lineHeight = 1.4f),
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(text.serverLabel, style = body(13.sp, 600, Pc.Muted))
            Segments(
                text.serverChoices, if (another) 1 else 0,
                { repo.setServerChoice(if (it == 1) ServerChoice.ANOTHER else ServerChoice.BUILT_IN) },
                Modifier.semantics { contentDescription = text.serverChoiceLabel },
                height = 34.dp, outer = 12.dp, inner = 9.dp, pad = 3.dp, fontSize = 13,
            )
            PField(
                if (another) state.server else state.builtIn, repo::setServer, text.serverLabel,
                background = Pc.Card, height = 48.dp, enabled = another,
                trailing = { ReachabilityMark(state.serverCheck, text.serverCheck) },
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, keyboardType = KeyboardType.Uri),
            )
            Text(text.serverHelp, style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f))
        }
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(text.relayLabel, style = body(13.sp, 600, Pc.Muted))
            PField(
                state.relay, repo::setRelay, text.relayLabel, background = Pc.Card, height = 48.dp,
                trailing = { ReachabilityMark(state.relayCheck, text.relayCheck) },
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false, keyboardType = KeyboardType.Uri),
            )
            Text(text.relayHelp, style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f))
        }
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(text.pushLabel, style = body(13.sp, 600, Pc.Muted), modifier = Modifier.semantics { heading() })
            Card(radius = 14.dp, modifier = Modifier.selectableGroup()) {
                RadioRow(text.pushOfficial, state.pushRelay == PushRelayChoice.Official) {
                    repo.setPushRelay(PushRelayChoice.Official)
                }
                RowDivider()
                RadioRow(text.pushCustom, state.pushRelay == PushRelayChoice.Custom) {
                    repo.setPushRelay(PushRelayChoice.Custom)
                }
            }
            Text(text.pushHelp, style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f))
        }
        LocalMaps.current?.let { maps -> MapsAdvancedSection(maps) { nav.go(Route.MapRegions) } }
        Card(radius = 14.dp) {
            Row(
                Modifier
                    .clickable(role = Role.Switch) { repo.setPostQuantum(!state.postQuantum) }
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                TwoLines(
                    text.postQuantum, text.postQuantumHelp, Modifier.weight(1f),
                    body(15.sp, 500), body(12.sp, color = Pc.Muted, lineHeight = 1.4f),
                )
                PSwitch(state.postQuantum, null)
            }
        }
    }
}

/** A field's check: a spinner, the green tick, or a cross; [label] is the core's words for it. */
@Composable
private fun ReachabilityMark(r: Reachability, label: String) {
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
    SecureScreen()
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
                }, Modifier.testTag(E2eTags.INVITE_SHARE), height = 50.dp, icon = PIcons.Share, enabled = state.link != null)
            }
        }
        SectionLabel(stringResource(R.string.invite_invitations), Modifier.padding(start = 4.dp, top = 4.dp))
        Card {
            state.invitations.forEachIndexed { i, inv ->
                if (i > 0) RowDivider()
                InvitationRow(inv, repo, nav)
            }
        }
    }
}

@Composable
private fun InvitationRow(inv: Invitation, repo: PingersRepository, nav: Navigator) {
    ListRow(null, padding = PaddingValues(horizontal = 16.dp, vertical = 12.dp)) {
        when (inv) {
            is Invitation.Opened -> {
                Avatar(inv.person, 40.dp, 13)
                TwoLines(inv.person.name, stringResource(R.string.invite_opened, inv.code), Modifier.weight(1f), body(15.sp, 600))
                Box(
                    Modifier
                        .height(48.dp)
                        .testTag(E2eTags.INVITE_VERIFY)
                        .clickable(role = Role.Button) {
                            repo.openJoinRequest(inv.id)
                            nav.go(Route.JoinRequest)
                        },
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

// ---------------------------------------------------------------- Party

@Composable
fun PartyScreen(state: PartyState, repo: PingersRepository, nav: Navigator) {
    Page(
        top = {
            TopBar(state.name, { nav.back() }, stringResource(R.string.cd_back)) {
                IconAction(PIcons.PersonAdd, stringResource(R.string.cd_invite_people), { nav.go(Route.Invite) }, Modifier.testTag(E2eTags.PARTY_INVITE))
                IconAction(PIcons.Settings, stringResource(R.string.cd_party_settings), {
                    repo.openPartySettings(state.id)
                    nav.go(Route.PartySettings)
                })
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
            MapBox(Scenes.PartyCard, frame = state.mapFrame, overlay = {
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
                    .testTag(E2eTags.PARTY_ADD_SPOT)
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
        // The server and its privacy, in the core's words; opens Party settings.
        ServerCard(
            state.settingsRow,
            state.settingsDetail,
            onClick = {
                repo.openPartySettings(state.id)
                nav.go(Route.PartySettings)
            },
            modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 14.dp).testTag(E2eTags.PARTY_SETTINGS_OPEN),
            privacy = state.privacy,
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
