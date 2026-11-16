package org.pingers.app.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.ui.E2eTags
import org.pingers.app.ui.components.ChevronIcon
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.theme.Grotesk
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import uniffi.pingers.HomeWakeupsView
import uniffi.pingers.NoticeLink
import uniffi.pingers.NoticeSeverity
import uniffi.pingers.NoticeView
import uniffi.pingers.PendingJoinView

/*
 * What sits above and under Home's sharing card (design canvas Notices, WakeupsHome,
 * WakeupsOffHome, HomePendingJoin): the one notice the core picked, the orange wake-ups fault,
 * the waiting join, and the grey "Wake-ups off" line. Their words are the core's.
 */

private class NoticeColours(val background: Color, val border: Color, val icon: Color, val title: Color, val body: Color)

private fun NoticeSeverity.colours() = when (this) {
    NoticeSeverity.STOPPED -> NoticeColours(Pc.RedTint, Pc.RedLine, Pc.Red, Pc.Red, Pc.RedBrown)
    NoticeSeverity.WARNING -> NoticeColours(Pc.OrangeTintSoft, Pc.OrangeLine, Pc.OrangeText, Pc.OrangeText, Pc.OrangeBrown)
    NoticeSeverity.INFO -> NoticeColours(Pc.Card, Pc.Line, Pc.BlueDark, Pc.Ink, Pc.Muted)
    NoticeSeverity.SLOW -> NoticeColours(Pc.Card, Pc.Line, Pc.Muted, Pc.Ink, Pc.Muted)
}

/**
 * The notice's icon. The view carries no kind, so it is told apart by colour and link, which
 * single out each of the six kinds on the canvas.
 */
private fun NoticeView.icon(): ImageVector = when (severity) {
    NoticeSeverity.STOPPED -> if (link is NoticeLink.Party) PIcons.ShieldAlert else PIcons.Clock
    NoticeSeverity.WARNING -> if (link is NoticeLink.Privacy) PIcons.Moon else PIcons.Servers
    NoticeSeverity.INFO -> PIcons.Pin
    NoticeSeverity.SLOW -> PIcons.Clock
}

@Composable
fun NoticeCard(notice: NoticeView, onLink: (NoticeLink) -> Unit) {
    val c = notice.severity.colours()
    val shape = RoundedCornerShape(18.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(c.background)
            .border(1.dp, c.border, shape)
            .semantics { if (notice.severity == NoticeSeverity.STOPPED) liveRegion = LiveRegionMode.Polite }
            .padding(horizontal = 16.dp, vertical = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(notice.icon(), null, tint = c.icon, modifier = Modifier.padding(top = 1.dp).size(22.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(notice.title, style = body(15.sp, 600, c.title))
            Text(notice.body, style = body(13.sp, color = c.body, lineHeight = 1.45f))
            val action = notice.action
            val link = notice.link
            if (action != null && link != null) {
                Box(
                    Modifier.heightIn(min = 32.dp).clickable(role = Role.Button) { onLink(link) }.testTag(E2eTags.NOTICE_ACTION),
                    contentAlignment = Alignment.CenterStart,
                ) { Text(action, style = body(14.sp, 600, if (c.title == Pc.Ink) Pc.Blue else c.title)) }
            }
        }
    }
}

/** The orange banner for a real wake-ups fault; "Fix" opens Wake-ups. */
@Composable
fun WakeupsFaultBanner(fault: HomeWakeupsView.Fault, onFix: () -> Unit) {
    val shape = RoundedCornerShape(18.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Pc.OrangeTintSoft)
            .border(1.dp, Pc.OrangeLine, shape)
            .clickable(onClick = onFix)
            .testTag(E2eTags.WAKEUPS_FIX)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.size(40.dp).clip(CircleShape).background(Pc.OrangeTint), contentAlignment = Alignment.Center) {
            Icon(PIcons.BellOff, null, tint = Pc.OrangeText, modifier = Modifier.size(20.dp))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(fault.title, style = body(15.sp, 600, Pc.OrangeText))
            Text(fault.detail, style = body(13.sp, color = Pc.OrangeBrown, lineHeight = 1.4f))
        }
        Text(fault.fix, style = body(14.sp, 600, Pc.OrangeText))
    }
}

/** The quiet grey line when wake-ups are off by choice: not a fault, so no banner. */
@Composable
fun WakeupsOffLine(text: String, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 32.dp)
            .clickable(onClick = onClick)
            .testTag(E2eTags.WAKEUPS_OFF_LINE)
            .padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(PIcons.BellOff, null, tint = Pc.Muted, modifier = Modifier.size(16.dp))
        Text(text, style = body(13.sp, color = Pc.Muted))
    }
}

/** "Waiting to be let in": reopens Asked to join with the whole code. */
@Composable
fun PendingJoinRow(pending: PendingJoinView, onClick: () -> Unit) {
    val shape = RoundedCornerShape(18.dp)
    Row(
        Modifier
            .fillMaxWidth()
            .clip(shape)
            .background(Pc.Card)
            .border(1.dp, Pc.Line, shape)
            .clickable(onClick = onClick)
            .clearAndSetSemantics {
                contentDescription = pending.label
                role = Role.Button
            }
            .testTag(E2eTags.HOME_PENDING_JOIN)
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.size(40.dp).clip(CircleShape).background(Pc.Chip), contentAlignment = Alignment.Center) {
            Icon(PIcons.Clock, null, tint = Pc.Ink, modifier = Modifier.size(20.dp))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(pending.title, style = body(15.sp, 600))
            Text(
                buildAnnotatedString {
                    append(pending.detail)
                    withStyle(SpanStyle(fontFamily = Grotesk, fontWeight = FontWeight.SemiBold, color = Pc.Ink, letterSpacing = 1.sp)) {
                        append(pending.codeStart)
                    }
                    append(pending.more)
                },
                style = body(13.sp, color = Pc.Muted),
            )
        }
        ChevronIcon()
    }
}
