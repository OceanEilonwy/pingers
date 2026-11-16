package org.pingers.app.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.heightIn
import androidx.compose.ui.semantics.Role
import org.pingers.app.data.Tint
import org.pingers.app.ui.components.InitialsCircle
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.AskedToJoinState
import org.pingers.app.data.JoinRequestState
import org.pingers.app.data.LetInState
import org.pingers.app.data.OpenInviteState
import org.pingers.app.data.PingersRepository
import org.pingers.app.platform.Notifications
import org.pingers.app.ui.E2eTags
import org.pingers.app.ui.components.AvatarStack
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.PField
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.components.color
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading

/*
 * Join flow A (design canvas JoinA1 to JoinA3 and InviteApprove). Every word on these screens
 * comes from the core (pingers-words through pingers-ffi's views), none from strings.xml.
 */

// ---------------------------------------------------------------- Open invite (JoinA1)

@Composable
fun OpenInviteScreen(state: OpenInviteState, repo: PingersRepository, nav: Navigator) {
    var name by rememberSaveable { mutableStateOf("") }
    val joinAs = state.joinAs
    JoinPage(
        bottom = {
            state.error?.let { Text(it, style = body(14.sp, color = Pc.OrangeText)) }
            PillButton(
                state.ask, { repo.askToJoin(name) { nav.replace(Route.AskedToJoin) } },
                Modifier.testTag(E2eTags.JOIN_ASK), enabled = joinAs != null || name.isNotBlank(),
            )
            SecondaryButton(state.notNow, Pc.Ink) { if (!nav.back()) nav.reset(Route.Home) }
        },
    ) {
        JoinHeader(TileShape, Pc.Line, state.title, state.subtitle) {
            Icon(PIcons.Mail, null, tint = Pc.Muted, modifier = Modifier.size(34.dp))
        }
        Card(radius = 20.dp, padding = PaddingValues(18.dp)) {
            Text(state.whatJoiningMeans, style = body(15.sp, 600), modifier = Modifier.semantics { heading() })
            state.points.forEachIndexed { i, point ->
                Spacer(Modifier.height(14.dp))
                JoinPoint(PointIcons.getOrElse(i) { PIcons.Lock }, Pc.Blue) {
                    Text(point, style = body(14.sp, lineHeight = 1.45f))
                }
            }
        }
        if (joinAs != null) {
            // "You'll join as Ben · Change": the name set in Settings; Change opens Your name.
            Card(radius = 20.dp, padding = PaddingValues(horizontal = 16.dp, vertical = 12.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    InitialsCircle(joinAs.avatar, Tint.Butter.color(), 40.dp, 14)
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
                        Text(joinAs.title, style = body(15.sp, 600))
                        Text(joinAs.subtitle, style = body(12.sp, color = Pc.Muted))
                    }
                    Box(
                        Modifier.heightIn(min = 44.dp).clickable(role = Role.Button) { nav.go(Route.YourName) }.testTag(E2eTags.JOIN_CHANGE_NAME),
                        contentAlignment = Alignment.Center,
                    ) { Text(joinAs.change, style = body(14.sp, 600, Pc.Blue)) }
                }
            }
        } else {
            // No name yet: what is typed here becomes Your name.
            Card(radius = 20.dp, padding = PaddingValues(horizontal = 18.dp, vertical = 16.dp)) {
                Text(state.nameLabel, style = body(13.sp, 600, Pc.Muted))
                Spacer(Modifier.height(10.dp))
                PField(
                    name, { name = it }, state.nameLabel, Modifier.testTag(E2eTags.JOIN_NAME),
                    shape = RoundedCornerShape(12.dp),
                )
                Spacer(Modifier.height(8.dp))
                Text(state.nameHelp, style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f))
            }
        }
    }
}

/** The points' icons, in the order pingers-words lists them: Spots, then encryption. */
private val PointIcons = listOf(PIcons.Pin, PIcons.Lock, PIcons.Moon)

// ---------------------------------------------------------------- Asked to join (JoinA2)

@Composable
fun AskedToJoinScreen(state: AskedToJoinState?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Blank()
    JoinPage(
        bottom = {
            PillButton(state.done, { nav.reset(Route.Home) }, background = Pc.Ink)
            SecondaryButton(state.withdraw, Pc.Red) {
                repo.withdrawJoin()
                nav.reset(Route.Home)
            }
        },
    ) {
        JoinHeader(CircleShape, Pc.BlueTint, state.title, state.subtitle) {
            Icon(PIcons.Clock, null, tint = Pc.Blue, modifier = Modifier.size(34.dp))
        }
        CodePanel(state.readCode, state.code, state.warning, 28, PaddingValues(horizontal = 18.dp, vertical = 16.dp), E2eTags.JOIN_CODE)
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Icon(PIcons.Bell, null, tint = Pc.Muted, modifier = Modifier.size(20.dp))
                Text(state.leaveNote, style = body(14.sp, lineHeight = 1.45f))
            }
        }
    }
}

// ---------------------------------------------------------------- You're in (JoinA3)

@Composable
fun LetInScreen(state: LetInState?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Blank()
    val context = LocalContext.current
    JoinPage(
        bottom = {
            PillButton(state.open, {
                Notifications.cancelLetIn(context)
                repo.openJoinedParty()
                nav.reset(Route.Party)
            }, Modifier.testTag(E2eTags.JOINED_OPEN))
            SecondaryButton(state.leave, Pc.Ink) {
                Notifications.cancelLetIn(context)
                repo.leaveJoinedParty()
                nav.reset(Route.Home)
            }
        },
    ) {
        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Box(
                Modifier.size(76.dp).clip(TileShape).background(state.colour.color()),
                contentAlignment = Alignment.Center,
            ) { Text(state.partyInitials, style = heading(30.sp, 700).copy(color = Color.White)) }
            Text(state.letInBy, style = body(15.sp, 600, Pc.GreenText))
            Text(state.partyName, style = heading(30.sp, 700), textAlign = TextAlign.Center, modifier = Modifier.semantics { heading() })
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AvatarStack(state.preview, 30.dp, 11, ring = Pc.Paper, extra = state.more)
                Text(state.counts, style = body(14.sp, color = Pc.Muted))
            }
        }
        Card(radius = 20.dp, padding = PaddingValues(horizontal = 18.dp, vertical = 16.dp)) {
            JoinPoint(PIcons.CheckCircle, Color.Unspecified) { Text(state.checked, style = body(14.sp, lineHeight = 1.45f)) }
        }
    }
}

// ---------------------------------------------------------------- Ben asked to join (InviteApprove)

@Composable
fun JoinRequestScreen(state: JoinRequestState?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Blank()
    Page(
        top = {
            TopBar(
                state.title, { nav.back() },
                stringResource(R.string.cd_back_to, stringResource(R.string.invite_title)), subtitle = state.subtitle,
            )
        },
        bottom = {
            PillButton(state.letIn, { repo.letIn(); nav.back() }, Modifier.testTag(E2eTags.APPROVE_LET_IN))
            SecondaryButton(state.turnAway, Pc.Red) { repo.turnAway(); nav.back() }
        },
        spacing = 16,
    ) {
        Spacer(Modifier.height(0.dp))
        CodePanel(state.ask, state.code, state.warning, 30, PaddingValues(18.dp), E2eTags.APPROVE_CODE)
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
            Text(state.whenHeading, style = body(14.sp, 600), modifier = Modifier.semantics { heading() })
            Spacer(Modifier.height(6.dp))
            Text(state.whenBody, style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f))
        }
    }
}

// ---------------------------------------------------------------- Shared pieces

private val TileShape = RoundedCornerShape(24.dp)

/** The joiner's screens: no top bar, the canvas's 24 dp margins, actions at the foot. */
@Composable
private fun JoinPage(bottom: @Composable ColumnScope.() -> Unit, content: @Composable ColumnScope.() -> Unit) {
    Page(
        top = { Spacer(Modifier.statusBarsPadding().height(28.dp)) },
        bottom = bottom,
        bodyPadding = PaddingValues(horizontal = 24.dp),
        spacing = 22,
        content = content,
    )
}

/** While a screen's state is gone (decided, withdrawn, dismissed) and the route moves on. */
@Composable
private fun Blank() = Box(Modifier.fillMaxSize().background(Pc.Paper))

/** The centred icon tile, title and subtitle at the top of Open invite and Asked to join. */
@Composable
private fun JoinHeader(shape: Shape, background: Color, title: String, subtitle: String, icon: @Composable () -> Unit) {
    Column(
        Modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Box(Modifier.size(76.dp).clip(shape).background(background), contentAlignment = Alignment.Center) { icon() }
        Text(title, style = heading(28.sp, 700), textAlign = TextAlign.Center, modifier = Modifier.semantics { heading() })
        Text(subtitle, style = body(15.sp, color = Pc.Muted, lineHeight = 1.45f), textAlign = TextAlign.Center)
    }
}

/**
 * The blue panel with the big join code, as both phones show it. The 16 digits go on two lines
 * of two groups ("4821 0937" over "5512 6604"), so they stay large enough to read aloud on a
 * narrow phone.
 */
@Composable
private fun CodePanel(label: String, code: String, warning: String, codeSize: Int, padding: PaddingValues, tag: String) {
    Column(
        Modifier.fillMaxWidth().clip(RoundedCornerShape(20.dp)).background(Pc.BlueTint).padding(padding),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(label, style = body(13.sp, 600, Pc.BlueDark))
        val lines = code.split(' ').chunked(2).joinToString("\n") { it.joinToString(" ") }
        Text(lines, style = heading(codeSize.sp, 600, 2.sp), modifier = Modifier.testTag(tag))
        Text(warning, style = body(13.sp, color = Pc.BlueDark, lineHeight = 1.4f))
    }
}

@Composable
private fun JoinPoint(icon: ImageVector, tint: Color, text: @Composable () -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Icon(icon, null, tint = tint, modifier = Modifier.padding(top = 1.dp).size(20.dp))
        text()
    }
}

/** The plain text button under the main one ("Not now", "Withdraw request", "Don't let in"). */
@Composable
private fun SecondaryButton(text: String, color: Color, onClick: () -> Unit) =
    PillButton(text, onClick, background = Color.Transparent, content = color, height = 48.dp)
