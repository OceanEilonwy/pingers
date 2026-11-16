package org.pingers.app.ui.screens

import android.os.Build
import android.view.HapticFeedbackConstants
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CheckboxDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import org.pingers.app.SecureScreen
import org.pingers.app.Navigator
import androidx.compose.ui.platform.testTag
import org.pingers.app.ui.E2eTags
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.ChatItem
import org.pingers.app.data.ChatMessage
import org.pingers.app.data.ChatState
import org.pingers.app.data.CoPresenceAlert
import org.pingers.app.data.CompassSelection
import org.pingers.app.data.CompassState
import org.pingers.app.data.CompassTarget
import org.pingers.app.data.LocationRequestState
import org.pingers.app.data.MapPoint
import org.pingers.app.data.MeetPointMode
import org.pingers.app.data.MeetPointState
import org.pingers.app.data.MeetupEndedState
import org.pingers.app.data.MeetupMember
import org.pingers.app.data.MeetupState
import org.pingers.app.data.PeerLocation
import org.pingers.app.data.Person
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.ShareScope
import org.pingers.app.rememberHeading
import org.pingers.app.ui.components.Avatar
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.IconAction
import org.pingers.app.ui.components.LabeledField
import org.pingers.app.ui.components.LockNote
import org.pingers.app.ui.components.MapButton
import org.pingers.app.ui.components.PField
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.Segmented
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.map.MapBox
import org.pingers.app.ui.map.MapChrome
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.statusBars
import org.pingers.app.ui.map.MapCanvas
import org.pingers.app.ui.map.Scenes
import org.pingers.app.ui.theme.Inter
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import org.pingers.app.words

// ---------------------------------------------------------------- Co-presence alert

/**
 * The co-presence alert as it lands on the lock screen. The real notification is
 * posted at the same time (CoPresenceNotifier); this is its preview, as on the canvas.
 */
@Composable
fun AlertScreen(alert: CoPresenceAlert?, repo: PingersRepository, nav: Navigator) {
    var shown by remember { mutableStateOf(alert) }
    if (alert != null) shown = alert
    val a = shown ?: return
    val now = remember { LocalDateTime.now() }
    val is24h = android.text.format.DateFormat.is24HourFormat(androidx.compose.ui.platform.LocalContext.current)
    Column(
        Modifier
            .fillMaxSize()
            .background(Pc.LockScreen)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(start = 12.dp, end = 12.dp, top = 48.dp, bottom = 24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(now.format(DateTimeFormatter.ofPattern("EEEE d MMMM")), style = body(18.sp, 500, Color(0xFFC9CEDA)))
        Text(
            now.format(DateTimeFormatter.ofPattern(if (is24h) "HH:mm" else "h:mm")),
            style = heading(84.sp, 600, (-2).sp).copy(color = Color.White, lineHeight = 84.sp),
        )
        Spacer(Modifier.weight(1f))
        AlertBanner(
            a,
            onOpen = { repo.dismissAlert(); nav.reset(Route.Meetup) },
            onHide = { repo.hideMeHere(); nav.reset(Route.Home) },
            background = Color.White.copy(alpha = 0.92f),
        )
        Row(
            Modifier
                .fillMaxWidth()
                .clip(RoundedCornerShape(18.dp))
                .background(Color.White.copy(alpha = 0.14f))
                .padding(horizontal = 14.dp, vertical = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Icon(PIcons.Lock, null, tint = Pc.Lavender, modifier = Modifier.padding(top = 1.dp).size(18.dp))
            Text(stringResource(R.string.alert_empty_push), style = body(13.sp, color = Color(0xFFE6E9F0), lineHeight = 1.45f))
        }
    }
}

// ---------------------------------------------------------------- Meetup

/** The meet point's teardrop pin, its tip at the bottom centre of the box. */
@Composable
fun MeetPin(modifier: Modifier = Modifier, shadow: Boolean = false) {
    Box(
        modifier
            .size(36.dp)
            .rotate(-45f)
            .then(if (shadow) Modifier.shadow(8.dp, RoundedCornerShape(18.dp, 18.dp, 18.dp, 4.dp)) else Modifier)
            .clip(RoundedCornerShape(18.dp, 18.dp, 18.dp, 4.dp))
            .background(Pc.Orange),
        contentAlignment = Alignment.Center,
    ) {
        Icon(PIcons.Flag, null, tint = Color.White, modifier = Modifier.size(18.dp).rotate(45f))
    }
}

@Composable
private fun MeDot(modifier: Modifier = Modifier) {
    Box(
        modifier
            .size(42.dp)
            .clip(CircleShape)
            .background(Pc.Blue.copy(alpha = 0.18f)),
        contentAlignment = Alignment.Center,
    ) {
        Box(Modifier.size(22.dp).clip(CircleShape).background(Color.White).padding(4.dp).clip(CircleShape).background(Pc.Blue))
    }
}

@Composable
private fun MapAvatar(person: Person, size: Dp, modifier: Modifier = Modifier) {
    Avatar(person, size, if (size > 36.dp) 13 else 11, modifier.shadow(3.dp, CircleShape), ring = Color.White, ringWidth = 3.dp)
}

@Composable
fun MeetupScreen(state: MeetupState, repo: PingersRepository, nav: Navigator) {
    SecureScreen()
    val w = words()
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        val frame = state.mapFrame
        val me = state.me?.let(frame::toMap)
        val meetAt = state.meetPoint?.at?.let(frame::toMap)
        MapBox(Scenes.Meetup, frame = frame, chrome = MapChrome(
            attribution = Alignment.TopEnd,
            attributionPadding = PaddingValues(top = WindowInsets.statusBars.asPaddingValues().calculateTopPadding() + 68.dp, end = 12.dp),
            promptAt = 0.3f,
        ), overlay = {
            if (me != null && meetAt != null) {
                drawLine(
                    Pc.Ink, Offset(me.x, me.y), Offset(meetAt.x, meetAt.y), strokeWidth = 2f,
                    pathEffect = PathEffect.dashPathEffect(floatArrayOf(4f, 5f)),
                )
            }
        }) { unit ->
            meetAt?.let { at -> MeetPin(Modifier.offset(unit * at.x - 18.dp, unit * at.y - 34.dp)) }
            state.members.forEach { m ->
                val loc = m.location
                if (loc is PeerLocation.Shared) {
                    val at = frame.toMap(loc.at)
                    MapAvatar(m.person, 40.dp, Modifier.offset(unit * at.x - 20.dp, unit * at.y - 20.dp))
                }
            }
            me?.let { at -> MeDot(Modifier.offset(unit * at.x - 21.dp, unit * at.y - 21.dp)) }
        }
        Row(
            Modifier.statusBarsPadding().padding(start = 16.dp, end = 16.dp, top = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            MapButton(PIcons.Back, stringResource(R.string.cd_back), { nav.back() })
            Row(
                Modifier.height(44.dp).shadow(2.dp, RoundedCornerShape(22.dp)).clip(RoundedCornerShape(22.dp)).background(Pc.Card).padding(horizontal = 16.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Box(Modifier.size(8.dp).clip(CircleShape).background(Pc.Orange))
                Text(stringResource(R.string.meetup_here, state.spotName, state.hereCount), style = body(15.sp, 600))
            }
        }
        BottomSheet(spacing = 14) {
            state.meetPoint?.let { mp ->
                Row(
                    Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(Pc.OrangeTintSoft).padding(start = 14.dp, end = 6.dp, top = 6.dp, bottom = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Icon(PIcons.Flag, null, tint = Pc.OrangeText, modifier = Modifier.size(22.dp))
                    TwoLines(
                        mp.label,
                        mp.distanceM?.let { stringResource(R.string.meetup_set_by, mp.setBy, it) }
                            ?: stringResource(R.string.meetup_set_by_no_distance, mp.setBy),
                        Modifier.weight(1f),
                        body(15.sp, 600), body(13.sp, color = Pc.OrangeBrown),
                    )
                    Box(
                        Modifier.size(48.dp).clip(CircleShape).clickable(role = Role.Button) {
                            repo.selectCompassTarget(CompassSelection.MeetPoint)
                            nav.go(Route.Compass)
                        },
                        contentAlignment = Alignment.Center,
                    ) { Text(stringResource(R.string.meetup_go), style = body(14.sp, 600, Pc.OrangeText)) }
                }
            }
            Column {
                state.members.forEach { m ->
                    val loc = m.location
                    MemberRow(m, if (loc is PeerLocation.Shared) loc.direction?.let { w.direction(it) } else null, repo, nav)
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Tile(PIcons.Chat, stringResource(R.string.meetup_chat, state.chatCount), Modifier.weight(1f).testTag(E2eTags.MEETUP_CHAT)) { nav.go(Route.Chat) }
                Tile(PIcons.Flag, stringResource(R.string.meetup_meet_point), Modifier.weight(1f)) { nav.go(Route.MeetPoint) }
                Tile(PIcons.Compass, stringResource(R.string.meetup_compass), Modifier.weight(1f).testTag(E2eTags.MEETUP_COMPASS)) { nav.go(Route.Compass) }
            }
            Footnote(state, nav)
        }
    }
}

@Composable
private fun MemberRow(m: MeetupMember, direction: String?, repo: PingersRepository, nav: Navigator) {
    Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Avatar(m.person, 40.dp, 13)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(m.person.name, style = body(15.sp, 600))
            when (val loc = m.location) {
                is PeerLocation.Shared -> {
                    Text(
                        loc.distanceM?.let { stringResource(R.string.meetup_sharing_with_you, it, direction.orEmpty()) }
                            ?: stringResource(R.string.meetup_sharing_no_distance),
                        style = body(13.sp, color = Pc.Muted),
                    )
                    // "Updated 40 s ago" (user decision), worded by pingers-words.
                    loc.updated?.let { Text(it, style = body(12.sp, color = Pc.Muted)) }
                }
                PeerLocation.Private, PeerLocation.Asked -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Icon(PIcons.Lock, null, tint = Pc.Muted, modifier = Modifier.size(13.dp))
                    Text(
                        stringResource(if (loc == PeerLocation.Asked) R.string.meetup_asked else R.string.meetup_private),
                        style = body(13.sp, color = Pc.Muted),
                    )
                }
            }
        }
        when (m.location) {
            is PeerLocation.Shared -> IconAction(
                PIcons.Compass, stringResource(R.string.cd_navigate_to, m.person.name),
                { repo.selectCompassTarget(CompassSelection.Member(m.person.id)); nav.go(Route.Compass) },
                background = Pc.Chip, iconSize = 20.dp,
            )
            PeerLocation.Private -> IconAction(
                PIcons.Eye, stringResource(R.string.cd_ask_location, m.person.name),
                { repo.askLocation(m.person.id) }, Modifier.testTag(E2eTags.MEETUP_ASK_LOCATION), background = Pc.Chip, iconSize = 20.dp,
            )
            PeerLocation.Asked -> Unit
        }
    }
}

@Composable
private fun Tile(icon: androidx.compose.ui.graphics.vector.ImageVector, label: String, modifier: Modifier, onClick: () -> Unit) {
    Column(
        modifier
            .height(68.dp)
            .clip(RoundedCornerShape(16.dp))
            .background(Pc.Chip)
            .clickable(role = Role.Button, onClick = onClick),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterVertically),
    ) {
        Icon(icon, null, tint = Pc.Ink, modifier = Modifier.size(22.dp))
        Text(label, style = body(13.sp, 600))
    }
}

@Composable
private fun Footnote(state: MeetupState, nav: Navigator) {
    val w = words()
    val shared = if (state.sharedWith.isEmpty()) {
        stringResource(R.string.meetup_shared_nobody)
    } else {
        stringResource(R.string.meetup_shared_with, w.names(state.sharedWith))
    }
    val manage = stringResource(R.string.meetup_manage)
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Center, verticalAlignment = Alignment.CenterVertically) {
        Text(
            pluralStringResource(R.plurals.meetup_only_here, state.hereCount, state.hereCount) + " " + shared,
            style = body(12.sp, color = Pc.Muted, lineHeight = 1.45f),
            textAlign = TextAlign.Center,
            modifier = Modifier.weight(1f, fill = false),
        )
        Box(
            Modifier.padding(start = 12.dp).heightIn(min = 48.dp).clickable(role = Role.Button) { nav.go(Route.Privacy) },
            contentAlignment = Alignment.Center,
        ) { Text(manage, style = body(12.sp, 600, Pc.Blue)) }
    }
}

// ---------------------------------------------------------------- Chat

@Composable
fun ChatScreen(state: ChatState, repo: PingersRepository, nav: Navigator) {
    val w = words()
    var draft by remember { mutableStateOf("") }
    val scroll = rememberScrollState()
    LaunchedEffect(state.items) { scroll.animateScrollTo(scroll.maxValue) }
    Column(Modifier.fillMaxSize().background(Pc.Paper)) {
        Column(Modifier.background(Pc.Card)) {
            org.pingers.app.ui.components.TopBar(
                state.spotName, { nav.back() }, stringResource(R.string.cd_back_meetup),
                subtitle = stringResource(R.string.chat_subtitle, w.names(state.participants + stringResource(R.string.you_lower))),
                titleSize = 19,
            )
            org.pingers.app.ui.components.RowDivider(Modifier.background(Pc.Line))
        }
        Column(
            Modifier.weight(1f).verticalScroll(scroll).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            state.items.forEach { item ->
                when (item) {
                    is ChatItem.Joined -> Text(
                        stringResource(R.string.chat_joined, item.name, item.time),
                        style = body(12.sp, color = Pc.Muted),
                        modifier = Modifier.align(Alignment.CenterHorizontally).clip(RoundedCornerShape(999.dp)).background(Pc.SystemPill).padding(horizontal = 12.dp, vertical = 4.dp),
                    )
                    is ChatItem.Message -> MessageBlock(item.message, state.replyingTo?.id == item.message.id, repo)
                    is ChatItem.MeetPointSet -> Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Avatar(item.author, 32.dp, 11)
                        Row(
                            Modifier
                                .clip(RoundedCornerShape(4.dp, 18.dp, 18.dp, 18.dp))
                                .background(Pc.OrangeTintSoft)
                                .border(1.dp, Color(0xFFFFD9C7), RoundedCornerShape(4.dp, 18.dp, 18.dp, 18.dp))
                                .padding(horizontal = 14.dp, vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            Icon(PIcons.Flag, null, tint = Pc.OrangeText, modifier = Modifier.size(16.dp))
                            Text(stringResource(R.string.chat_meet_point_set, item.label), style = body(14.sp))
                        }
                    }
                }
            }
        }
        Column(
            Modifier.background(Pc.Card).navigationBarsPadding().padding(start = 12.dp, end = 12.dp, top = 6.dp, bottom = 10.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            state.replyingTo?.let { parent ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        stringResource(R.string.chat_replying, parent.author.name),
                        style = body(12.sp, color = Pc.Muted),
                        modifier = Modifier.weight(1f).padding(start = 8.dp),
                    )
                    IconAction(PIcons.Close, stringResource(R.string.cd_leave_thread), { repo.replyInThread(null) }, tint = Pc.Muted, iconSize = 14.dp, size = 40.dp)
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                PField(
                    draft, { draft = it }, stringResource(R.string.chat_message),
                    background = Pc.Chip, border = Color.Transparent, height = 48.dp, fontSize = 15,
                    placeholder = stringResource(R.string.chat_message), shape = RoundedCornerShape(24.dp),
                    modifier = Modifier.weight(1f),
                )
                IconAction(
                    PIcons.Send, stringResource(R.string.cd_send), { repo.sendChat(draft); draft = "" }, Modifier.testTag(E2eTags.CHAT_SEND),
                    tint = Color.White, background = Pc.Blue, iconSize = 20.dp,
                )
            }
        }
    }
}

@Composable
private fun MessageBlock(m: ChatMessage, inThread: Boolean, repo: PingersRepository) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (m.author.isMe) {
            MyBubble(m.body, 15, 18)
        } else {
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Avatar(m.author, 32.dp, 11)
                Column(Modifier.widthIn(max = 280.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    TheirBubble(m.body, 15, 18)
                    if (m.replies.isNotEmpty()) {
                        Row(
                            Modifier
                                .heightIn(min = 48.dp)
                                .clickable(role = Role.Button) { repo.replyInThread(if (inThread) null else m.id) }
                                .padding(end = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(6.dp),
                        ) {
                            Row {
                                m.replies.take(2).forEachIndexed { i, r ->
                                    Avatar(r.author, 18.dp, 1, Modifier.offset(x = (-6 * i).dp), ring = Pc.Paper)
                                }
                            }
                            Text(
                                androidx.compose.ui.res.pluralStringResource(R.plurals.chat_replies, m.replies.size, m.replies.size),
                                style = body(13.sp, 600, Pc.Blue),
                            )
                        }
                    }
                }
            }
        }
        if (m.replies.isNotEmpty()) {
            Column(
                Modifier
                    .padding(start = 42.dp)
                    .drawBehind { drawLine(Pc.Handle, Offset(0f, 0f), Offset(0f, size.height), strokeWidth = 2.dp.toPx()) }
                    .padding(start = 12.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                m.replies.forEach { r ->
                    if (r.author.isMe) {
                        MyBubble(r.body, 14, 16)
                    } else {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Avatar(r.author, 26.dp, 10)
                            TheirBubble(r.body, 14, 16)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun TheirBubble(text: String, size: Int, r: Int) {
    val shape = RoundedCornerShape(4.dp, r.dp, r.dp, r.dp)
    Text(
        text,
        style = body(size.sp, lineHeight = 1.4f),
        modifier = Modifier.clip(shape).background(Pc.Card).border(1.dp, Pc.Line, shape)
            .padding(horizontal = if (size > 14) 14.dp else 12.dp, vertical = if (size > 14) 10.dp else 8.dp),
    )
}

@Composable
private fun MyBubble(text: String, size: Int, r: Int) {
    Box(Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterEnd) {
        Text(
            text,
            style = body(size.sp, color = Color.White, lineHeight = 1.4f),
            modifier = Modifier.widthIn(max = 270.dp).clip(RoundedCornerShape(r.dp, 4.dp, r.dp, r.dp)).background(Pc.Blue)
                .padding(horizontal = if (size > 14) 14.dp else 12.dp, vertical = if (size > 14) 10.dp else 8.dp),
        )
    }
}

// ---------------------------------------------------------------- Meet point

// Where the pin sits on screen, in map units; dragging moves the map under it.
private val PinAt = MapPoint(195f, 256f)

@Composable
fun MeetPointScreen(state: MeetPointState, repo: PingersRepository, nav: Navigator) {
    SecureScreen()
    val density = LocalDensity.current
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            val unit = maxWidth / Scenes.MeetPoint.width
            val unitPx = with(density) { unit.toPx() }
            val frame = state.mapFrame
            val centre = frame.toMap(state.point ?: state.pin)
            val pan = MapPoint(PinAt.x - centre.x, PinAt.y - centre.y)
            val hint = stringResource(R.string.mp_drag)
            val current by rememberUpdatedState(state)
            MapBox(
                Scenes.MeetPoint,
                Modifier.semantics { contentDescription = hint }.pointerInput(Unit) {
                    detectDragGestures { change, amount ->
                        change.consume()
                        val f = current.mapFrame
                        val pin = f.toMap(current.pin)
                        repo.moveMeetPoint(f.toCoordinate(MapPoint(pin.x - amount.x / unitPx, pin.y - amount.y / unitPx)))
                    }
                },
                pan = pan,
                frame = frame,
                chrome = MapChrome(
                    attribution = Alignment.TopEnd,
                    attributionPadding = PaddingValues(top = WindowInsets.statusBars.asPaddingValues().calculateTopPadding() + 68.dp, end = 12.dp),
                    promptAt = 0.25f,
                ),
            ) { u ->
                state.members.forEach { (p, c) ->
                    val at = frame.toMap(c)
                    MapAvatar(p, 34.dp, Modifier.offset(u * at.x - 17.dp, u * at.y - 17.dp))
                }
            }
            Canvas(Modifier.fillMaxWidth().height(unit * Scenes.MeetPoint.height)) {
                drawCircle(Pc.Orange.copy(alpha = 0.14f), 28f * unitPx, Offset(PinAt.x * unitPx, (PinAt.y + 6f) * unitPx))
            }
            MeetPin(Modifier.offset(unit * PinAt.x - 18.dp, unit * PinAt.y - 34.dp), shadow = true)
            MapLabel(hint, Modifier.offset(unit * 120f, unit * 165f), radius = 8.dp)
        }
        Box(Modifier.statusBarsPadding().padding(start = 16.dp, top = 12.dp)) {
            MapButton(PIcons.Close, stringResource(R.string.cd_close), { nav.back() })
        }
        BottomSheet(spacing = 14) {
            Text(stringResource(R.string.mp_title), style = heading(22.sp), modifier = Modifier.semantics { heading() })
            Segmented(
                listOf(stringResource(R.string.mp_pin), stringResource(R.string.mp_at_me), stringResource(R.string.mp_midpoint)),
                state.mode.ordinal,
                { repo.setMeetPointMode(MeetPointMode.entries[it]) },
                fontSize = 13,
            )
            LabeledField(stringResource(R.string.mp_label), state.label, repo::setMeetPointLabel, height = 52.dp)
            Row(horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                state.distances.forEach { d ->
                    Text(
                        if (d.distanceM != null) stringResource(R.string.mp_distance, d.name, d.distanceM) else stringResource(R.string.mp_private, d.name),
                        style = body(13.sp, color = Pc.Muted),
                    )
                }
            }
            Spacer(Modifier.height(4.dp))
            PillButton(
                stringResource(R.string.mp_share), { repo.shareMeetPoint(); nav.go(Route.Meetup) },
                background = Pc.Orange, enabled = state.canShare,
            )
        }
    }
}

// ---------------------------------------------------------------- Compass

@Composable
fun CompassScreen(state: CompassState, repo: PingersRepository, nav: Navigator) {
    val w = words()
    val heading by rememberHeading()
    val reading = state.reading
    val relative = (((reading?.bearingDeg ?: heading) - heading) % 360f + 360f) % 360f
    val aligned = reading != null && (relative < 10f || relative > 350f)
    val view = LocalView.current
    LaunchedEffect(aligned) {
        if (aligned) {
            view.performHapticFeedback(
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) HapticFeedbackConstants.CONFIRM else HapticFeedbackConstants.VIRTUAL_KEY,
            )
        }
    }
    val dir = reading?.let { w.direction(it.direction) }.orEmpty()
    val measurer = rememberTextMeasurer()
    val north = stringResource(R.string.compass_n)
    Column(
        Modifier
            .fillMaxSize()
            .background(Pc.Ink)
            .statusBarsPadding()
            .navigationBarsPadding()
            .padding(start = 20.dp, end = 20.dp, top = 12.dp, bottom = 20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(18.dp),
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            IconAction(PIcons.Close, stringResource(R.string.cd_close_compass), { nav.back() }, tint = Color.White, background = Pc.InkRaised, iconSize = 20.dp)
            Spacer(Modifier.weight(1f))
            Text(
                state.gpsAccuracyM?.let { stringResource(R.string.compass_gps, it) } ?: stringResource(R.string.compass_no_gps),
                style = body(13.sp, color = Pc.InkSubtle),
                modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(Pc.InkRaised).padding(horizontal = 12.dp, vertical = 8.dp),
            )
        }
        Row(Modifier.fillMaxWidth().selectableGroup(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            state.targets.forEach { t -> TargetChip(t, t.selection == state.selected) { repo.selectCompassTarget(t.selection) } }
        }
        val dialLabel = reading?.let { stringResource(R.string.cd_compass_arrow, dir, it.bearingDeg.toInt(), state.targetLabel) }
            ?: stringResource(R.string.compass_waiting, state.targetLabel)
        Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
            Canvas(Modifier.size(320.dp).testTag(E2eTags.COMPASS_DIAL).semantics { contentDescription = dialLabel }) {
                val s = size.width / 320f
                val c = Offset(size.width / 2, size.height / 2)
                drawCircle(Color(0xFF1E2129), 150f * s, c)
                drawCircle(Color(0xFF2E323C), 150f * s, c, style = Stroke(2f * s))
                rotate(-heading, c) {
                    for (i in 0 until 8) {
                        rotate(i * 45f, c) {
                            drawLine(Color(0xFF4A4F5B), Offset(c.x, c.y - 142f * s), Offset(c.x, c.y - 128f * s), strokeWidth = 2f * s)
                        }
                    }
                    val label = measurer.measure(north, TextStyle(fontFamily = Inter, fontSize = 14.sp, color = Pc.InkSubtle, fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold))
                    drawText(label, topLeft = Offset(c.x - label.size.width / 2f, c.y - 108f * s - label.size.height / 2f))
                }
                if (reading != null) rotate(relative, c) {
                    drawArc(
                        Pc.Orange.copy(alpha = 0.25f), startAngle = -90f - 11f, sweepAngle = 22f, useCenter = false,
                        topLeft = Offset(c.x - 104f * s, c.y - 104f * s), size = Size(208f * s, 208f * s), style = Stroke(14f * s),
                    )
                    val arrow = Path().apply {
                        moveTo(c.x, c.y - 90f * s)
                        lineTo(c.x + 36f * s, c.y + 18f * s)
                        lineTo(c.x, c.y - 2f * s)
                        lineTo(c.x - 36f * s, c.y + 18f * s)
                        close()
                    }
                    drawPath(arrow, Pc.Orange)
                }
                drawCircle(Color.White, 6f * s, c)
            }
        }
        Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (reading != null) {
                Text(stringResource(R.string.metres, reading.distanceM), style = heading(64.sp).copy(color = Color.White, lineHeight = 64.sp))
                // "Updated 40 s ago" / "Updated just now" (user decision), worded by pingers-words.
                reading.updated?.let { Text(it, style = body(13.sp, color = Pc.InkSubtle)) }
                Text(stringResource(R.string.compass_walk, reading.walkMinutes, dir), style = body(15.sp, color = Pc.InkSubtle))
            } else {
                Text(stringResource(R.string.compass_waiting, state.targetLabel), style = body(15.sp, color = Pc.InkSubtle))
            }
        }
        Text(
            stringResource(R.string.compass_taps) + "\n" + stringResource(R.string.compass_watchers, w.names(state.watchers)),
            style = body(13.sp, color = Pc.InkSubtle, lineHeight = 1.45f),
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun TargetChip(t: CompassTarget, selected: Boolean, onClick: () -> Unit) {
    val locked = t is CompassTarget.Member && t.locked
    val label = when (t) {
        is CompassTarget.Place -> t.label
        is CompassTarget.Member -> t.name
    }
    val cd = if (locked) stringResource(R.string.cd_target_locked, label) else label
    val bg = if (selected) Pc.Orange else Color.Transparent
    Row(
        Modifier
            .height(48.dp)
            .semantics {
                contentDescription = cd
                this.selected = selected
            }
            .selectable(selected, role = Role.RadioButton, onClick = onClick)
            .padding(vertical = 4.dp)
            .clip(RoundedCornerShape(20.dp))
            .background(bg)
            .drawBehind {
                if (!selected) {
                    drawRoundRect(
                        if (locked) Pc.Muted else Color(0xFF3A3F49),
                        cornerRadius = CornerRadius(20.dp.toPx()),
                        style = Stroke(1.dp.toPx(), pathEffect = if (locked) PathEffect.dashPathEffect(floatArrayOf(8f, 6f)) else null),
                    )
                }
            }
            .padding(horizontal = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (t is CompassTarget.Place) Icon(PIcons.Flag, null, tint = Color.White, modifier = Modifier.size(15.dp))
        if (locked) Icon(PIcons.Lock, null, tint = Pc.InkSubtle, modifier = Modifier.size(14.dp))
        Text(label, style = body(14.sp, 600, if (locked) Pc.InkSubtle else Color.White))
    }
}

// ---------------------------------------------------------------- Location request

@Composable
fun LocationRequestScreen(state: LocationRequestState, repo: PingersRepository, nav: Navigator) {
    val name = state.from.name
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        MapCanvas(Scenes.LocationRequest, Modifier.fillMaxWidth().height(500.dp))
        Box(Modifier.fillMaxSize().background(Pc.Ink.copy(alpha = 0.45f)))
        BottomSheet(spacing = 16) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                Avatar(state.from, 52.dp, 16)
                Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    Text(
                        stringResource(R.string.lr_title, name),
                        style = heading(21.sp).copy(lineHeight = 25.sp),
                        modifier = Modifier.semantics { heading() },
                    )
                    Text(stringResource(R.string.lr_already, name, state.spotName), style = body(13.sp, color = Pc.Muted))
                }
            }
            Column(Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(stringResource(R.string.lr_share_for), style = body(13.sp, 600, Pc.Muted), modifier = Modifier.padding(bottom = 0.dp))
                ScopeOption(stringResource(R.string.lr_this_meetup), state.scope == ShareScope.ThisMeetup) { repo.setShareScope(ShareScope.ThisMeetup) }
                ScopeOption(stringResource(R.string.lr_15_min), state.scope == ShareScope.FifteenMinutes) { repo.setShareScope(ShareScope.FifteenMinutes) }
            }
            Column {
                CheckRow(stringResource(R.string.lr_ask_back, name), state.askBack, repo::setAskBack)
                CheckRow(stringResource(R.string.lr_always, name, state.partyName), state.alwaysAllow, repo::setAlwaysAllow)
            }
            LockNote(stringResource(R.string.lr_note, name))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                PillButton(
                    stringResource(R.string.lr_not_now), { repo.answerLocationRequest(false); nav.go(Route.Meetup) },
                    Modifier.weight(1f), background = Pc.Chip, content = Pc.Ink,
                )
                PillButton(stringResource(R.string.lr_allow), { repo.answerLocationRequest(true); nav.go(Route.Meetup) }, Modifier.weight(1f).testTag(E2eTags.LOCATION_ALLOW))
            }
            Text(
                stringResource(R.string.lr_silent, name),
                style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f),
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

@Composable
private fun ScopeOption(text: String, selected: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(14.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clip(shape)
            .background(if (selected) Pc.BlueTintSoft else Color.Transparent)
            .border(if (selected) 1.5.dp else 1.dp, if (selected) Pc.Blue else Pc.Line, shape)
            .selectable(selected, role = Role.RadioButton, onClick = onClick)
            .padding(horizontal = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        RadioButton(selected, onClick = null, colors = RadioButtonDefaults.colors(selectedColor = Pc.Blue), modifier = Modifier.padding(horizontal = 10.dp))
        Text(text, style = body(15.sp, 500))
    }
}

@Composable
private fun CheckRow(text: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 48.dp).toggleable(checked, role = Role.Checkbox, onValueChange = onChange),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Checkbox(checked, onCheckedChange = null, colors = CheckboxDefaults.colors(checkedColor = Pc.Blue), modifier = Modifier.padding(end = 6.dp))
        Text(text, style = body(15.sp))
    }
}

// ---------------------------------------------------------------- Meetup ended

@Composable
fun MeetupEndedScreen(state: MeetupEndedState, nav: Navigator) {
    Page(
        top = { Spacer(Modifier.statusBarsPadding().height(76.dp)) },
        bottom = { PillButton(stringResource(R.string.done), { nav.reset(Route.Home) }, Modifier.testTag(E2eTags.ENDED_DONE)) },
        bodyPadding = PaddingValues(horizontal = 24.dp),
        spacing = 24,
    ) {
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Box(
                Modifier.size(72.dp).clip(CircleShape).background(Pc.Card).border(1.dp, Pc.Line, CircleShape),
                contentAlignment = Alignment.Center,
            ) { Icon(PIcons.Check, null, tint = Pc.Ink, modifier = Modifier.size(32.dp)) }
            Text(stringResource(R.string.ended_title), style = heading(30.sp, 700), modifier = Modifier.semantics { heading() })
            Text(stringResource(R.string.ended_everyone_left, state.spotName), style = body(15.sp, color = Pc.Muted))
        }
        Card(radius = 20.dp, padding = PaddingValues(18.dp)) {
            Row(Modifier.fillMaxWidth()) {
                Stat(stringResource(R.string.ended_duration, state.durationMinutes / 60, state.durationMinutes % 60), stringResource(R.string.ended_range, state.from, state.to))
                Stat(state.people.toString(), stringResource(R.string.ended_people))
                Stat(state.messages.toString(), stringResource(R.string.ended_messages))
            }
        }
        Row(
            Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(Pc.Ink).padding(horizontal = 18.dp, vertical = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Icon(PIcons.Trash, null, tint = Pc.Lavender, modifier = Modifier.padding(top = 2.dp).size(20.dp))
            TwoLines(
                stringResource(R.string.ended_erased), stringResource(R.string.ended_erased_detail),
                titleStyle = body(15.sp, 600, Color.White), subtitleStyle = body(13.sp, color = Pc.InkSubtle, lineHeight = 1.45f),
            )
        }
    }
}

@Composable
private fun androidx.compose.foundation.layout.RowScope.Stat(value: String, label: String) {
    Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(value, style = heading(24.sp))
        Text(label, style = body(12.sp, color = Pc.Muted))
    }
}
