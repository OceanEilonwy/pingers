package org.pingers.app.ui.screens

import android.app.TimePickerDialog
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.RadioButton
import androidx.compose.material3.RadioButtonDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.Tint
import org.pingers.app.platform.AndroidPlatform
import org.pingers.app.ui.E2eTags
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.ChevronIcon
import org.pingers.app.ui.components.InitialsCircle
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.PField
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PSwitch
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SectionLabel
import org.pingers.app.ui.components.SwitchRow
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.components.color
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import uniffi.pingers.DeviceSettingsView
import uniffi.pingers.QuietHoursEditorView
import uniffi.pingers.TimeOfDay
import uniffi.pingers.TitledRow
import uniffi.pingers.WakeupsKind
import uniffi.pingers.WakeupsView
import uniffi.pingers.YourNameView
import uniffi.pingers.ZoneDays
import uniffi.pingers.ZoneTimes
import uniffi.pingers.ZoneTimesEditorView

/*
 * The device's own settings (design canvas section 4): Settings (the Home cog), Your name,
 * Quiet hours, Zone times and Wake-ups. Every word comes from the core's records (pingers-words
 * through pingers-ffi), none from strings.xml.
 */

// ---------------------------------------------------------------- Shared pieces

/** An empty page while the core has not answered yet. */
@Composable
internal fun Waiting() = Box(Modifier.fillMaxSize().background(Pc.Paper))

/** A section heading in the canvas's place: 20 above, 8 below, 24 from the edge. */
@Composable
internal fun Heading(text: String) = SectionLabel(text, Modifier.padding(start = 4.dp, top = 20.dp, bottom = 8.dp))

/** The grey note under a card. */
@Composable
internal fun Note(text: String, top: Dp = 12.dp, size: Int = 13) =
    Text(text, style = body(size.sp, color = Pc.Muted, lineHeight = 1.45f), modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = top))

/** A row with a title, an optional line under it and a chevron. */
@Composable
internal fun LinkRow(title: String, detail: String?, onClick: (() -> Unit)?, modifier: Modifier = Modifier, chevron: Boolean = onClick != null) {
    ListRow(onClick, modifier) {
        TwoLines(title, detail, Modifier.weight(1f), body(16.sp, 500), body(13.sp, color = Pc.Muted, lineHeight = 1.4f))
        if (chevron) ChevronIcon()
    }
}

/** The blue "How Pingers keeps you private" link. */
@Composable
internal fun SecurityLink(text: String, onClick: () -> Unit) {
    Row(
        Modifier
            .padding(top = 20.dp)
            .fillMaxWidth()
            .clip(RoundedCornerShape(18.dp))
            .background(Pc.BlueTint)
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(PIcons.Shield, null, tint = Pc.BlueDark, modifier = Modifier.size(20.dp))
        Text(text, style = body(15.sp, 600, Pc.BlueDark), modifier = Modifier.weight(1f))
        ChevronIcon(Pc.BlueDark)
    }
}

/** The main button, grey while it can't be pressed. */
@Composable
internal fun MainButton(text: String, enabled: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) = PillButton(
    text, onClick, modifier, enabled = enabled,
    background = if (enabled) Pc.Blue else Pc.Line, content = if (enabled) Color.White else Pc.Muted,
)

/** A red line with an alert icon: the core's refusal. */
@Composable
internal fun ErrorLine(text: String, modifier: Modifier = Modifier) {
    Row(modifier, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Icon(PIcons.AlertCircle, null, tint = Pc.Red, modifier = Modifier.padding(top = 1.dp).size(18.dp))
        Text(text, style = body(14.sp, color = Pc.Red, lineHeight = 1.45f))
    }
}

/** The canvas's segmented choice (days on Zone times, the server on Advanced). */
@Composable
internal fun Segments(
    options: List<String>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
    height: Dp = 40.dp,
    outer: Dp = 14.dp,
    inner: Dp = 10.dp,
    pad: Dp = 4.dp,
    fontSize: Int = 14,
) {
    Row(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(outer))
            .background(Pc.ChipDark)
            .selectableGroup()
            .padding(pad),
        horizontalArrangement = Arrangement.spacedBy(pad),
    ) {
        options.forEachIndexed { i, label ->
            val on = i == selected
            Box(
                Modifier
                    .weight(1f)
                    .height(height)
                    .then(if (on) Modifier.shadow(1.dp, RoundedCornerShape(inner)) else Modifier)
                    .clip(RoundedCornerShape(inner))
                    .background(if (on) Pc.Card else Color.Transparent)
                    .selectable(on, role = Role.RadioButton) { onSelect(i) },
                contentAlignment = Alignment.Center,
            ) { Text(label, style = body(fontSize.sp, 600, if (on) Pc.Ink else Pc.Body)) }
        }
    }
}

/** A big time on a card ("Starts 23:00"); tapping opens Android's 24-hour time picker. */
@Composable
internal fun RowScope.TimeCard(
    label: String,
    time: String,
    action: String,
    value: TimeOfDay,
    error: Boolean,
    note: String?,
    showNote: Boolean,
    onPick: (TimeOfDay) -> Unit,
) {
    val context = LocalContext.current
    val shape = RoundedCornerShape(18.dp)
    Column(
        Modifier
            .weight(1f)
            .clip(shape)
            .background(Pc.Card)
            .border(if (error) 2.dp else 1.dp, if (error) Pc.Red else Pc.Line, shape)
            .clickable(role = Role.Button) { pickTime(context, value, onPick) }
            .semantics(mergeDescendants = true) { contentDescription = action }
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(label, style = body(13.sp, 600, Pc.Muted))
        Text(time, style = heading(34.sp))
        if (showNote) Text(note ?: " ", style = body(12.sp, color = Pc.Muted))
    }
}

private fun pickTime(context: Context, value: TimeOfDay, onPick: (TimeOfDay) -> Unit) {
    TimePickerDialog(
        context,
        { _, hour, minute -> onPick(TimeOfDay(hour.toUByte(), minute.toUByte())) },
        value.hour.toInt(), value.minute.toInt(), true,
    ).show()
}

// ---------------------------------------------------------------- Settings (the Home cog)

@Composable
fun SettingsScreen(state: DeviceSettingsView?, nav: Navigator) {
    if (state == null) return Waiting()
    Page(
        top = { TopBar(state.title, { nav.back() }, stringResource(R.string.cd_back), subtitle = state.subtitle) },
        spacing = 0,
    ) {
        Card(
            Modifier.padding(top = 10.dp).clickable { nav.go(Route.YourName) }.testTag(E2eTags.SETTINGS_YOUR_NAME),
            padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
                InitialsCircle(state.youAvatar.orEmpty(), Tint.Butter.color(), 48.dp, 15)
                TwoLines(state.youTitle, state.youSubtitle, Modifier.weight(1f), body(17.sp, 600), body(13.sp, color = Pc.Muted))
                ChevronIcon()
            }
        }
        Heading(state.whenHeading)
        Card {
            LinkRow(state.quietHours, state.quietHoursSummary, { nav.go(Route.QuietHours) }, Modifier.testTag(E2eTags.SETTINGS_QUIET_HOURS))
            RowDivider()
            LinkRow(state.zones, state.zonesSummary, { nav.go(Route.ExclusionZones) })
        }
        Heading(state.phoneHeading)
        Card {
            LinkRow(state.wakeups, state.wakeupsSummary, { nav.go(Route.Wakeups) }, Modifier.testTag(E2eTags.SETTINGS_WAKEUPS))
            RowDivider()
            LinkRow(state.maps, state.mapsSummary, { nav.go(Route.MapRegions) })
        }
        SecurityLink(state.securityLink) { nav.go(Route.Security) }
        Note(state.footnote, top = 16.dp)
    }
}

// ---------------------------------------------------------------- Your name

@Composable
fun YourNameScreen(state: YourNameView?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Waiting()
    var name by rememberSaveable(state.name) { mutableStateOf(state.name.orEmpty()) }
    Page(
        top = { TopBar(state.title, { nav.back() }, state.back) },
        bottom = {
            MainButton(state.save, name.isNotBlank(), { repo.saveDisplayName(name) { nav.back() } }, Modifier.testTag(E2eTags.YOUR_NAME_SAVE))
        },
        spacing = 0,
    ) {
        Column(Modifier.fillMaxWidth().padding(top = 18.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(Modifier.size(84.dp).clip(CircleShape).background(Tint.Butter.color()), contentAlignment = Alignment.Center) {
                Text(state.avatar.orEmpty(), style = heading(28.sp))
            }
            Text(state.appearsAs, style = body(13.sp, color = Pc.Muted))
        }
        Card(Modifier.padding(top = 18.dp), padding = PaddingValues(16.dp)) {
            Text(state.label, style = body(13.sp, 600, Pc.Muted))
            Spacer(Modifier.height(10.dp))
            PField(
                name, { name = it }, state.label, Modifier.testTag(E2eTags.YOUR_NAME_FIELD),
                background = Pc.Card, border = Pc.Blue, borderWidth = 2.dp, height = 48.dp, fontSize = 17,
                placeholder = state.placeholder, shape = RoundedCornerShape(12.dp),
            )
            Spacer(Modifier.height(10.dp))
            Text(state.help, style = body(13.sp, color = Pc.Muted, lineHeight = 1.45f))
        }
        state.partiesHeading?.let { Heading(it) }
        if (state.parties.isNotEmpty()) {
            Card {
                state.parties.forEachIndexed { i, p ->
                    if (i > 0) RowDivider()
                    Row(
                        Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Box(Modifier.size(36.dp).clip(RoundedCornerShape(12.dp)).background(p.colour.toUi().color()), contentAlignment = Alignment.Center) {
                            Text(p.initials, style = heading(13.sp, 700).copy(color = Color.White))
                        }
                        Text(p.name, style = body(15.sp, 500), modifier = Modifier.weight(1f))
                    }
                }
            }
        }
        Note(state.note)
    }
}

private fun uniffi.pingers.PartyColour.toUi() = when (this) {
    uniffi.pingers.PartyColour.BLUE -> org.pingers.app.data.PartyColour.Blue
    uniffi.pingers.PartyColour.ORANGE -> org.pingers.app.data.PartyColour.Orange
    uniffi.pingers.PartyColour.INK -> org.pingers.app.data.PartyColour.Ink
    uniffi.pingers.PartyColour.TEAL -> org.pingers.app.data.PartyColour.Teal
    uniffi.pingers.PartyColour.PLUM -> org.pingers.app.data.PartyColour.Plum
}

// ---------------------------------------------------------------- Quiet hours

@Composable
fun QuietHoursScreen(state: QuietHoursEditorView?, repo: PingersRepository, nav: Navigator) {
    LaunchedEffect(Unit) { repo.openQuietHours() }
    if (state == null) return Waiting()
    val error = state.error
    Page(
        top = { TopBar(state.title, { nav.back() }, stringResource(R.string.cd_back)) },
        bottom = {
            MainButton(state.save, state.canSave, { repo.saveQuietHours { nav.back() } }, Modifier.testTag(E2eTags.QUIET_HOURS_SAVE))
        },
        spacing = 0,
    ) {
        Text(state.intro, style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f), modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = 4.dp, bottom = 14.dp))
        Card {
            SwitchRow(state.switchLabel, null, state.on, { repo.setQuietHoursDraft(it, state.from, state.to) }, Modifier.testTag(E2eTags.QUIET_HOURS_SWITCH))
        }
        Row(Modifier.padding(top = 14.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            TimeCard(state.starts, state.fromText, state.startsAction, state.from, error != null, null, error == null) {
                repo.setQuietHoursDraft(state.on, it, state.to)
            }
            TimeCard(state.ends, state.toText, state.endsAction, state.to, error != null, state.toNote, error == null) {
                repo.setQuietHoursDraft(state.on, state.from, it)
            }
        }
        error?.let { ErrorLine(it, Modifier.padding(start = 4.dp, end = 4.dp, top = 10.dp)) }
        Card(Modifier.padding(top = 14.dp), padding = PaddingValues(16.dp)) {
            Text(state.length, style = body(13.sp, 600, Pc.Muted))
            Spacer(Modifier.height(8.dp))
            DayBar(state, Modifier.semantics { contentDescription = state.barLabel })
        }
        if (error == null) Note(state.footnote, top = 14.dp)
    }
}

/** The 24-hour bar: the quiet stretches in ink, the hours under it; a red mark when the times are equal. */
@Composable
private fun DayBar(state: QuietHoursEditorView, modifier: Modifier = Modifier) {
    Column(modifier) {
        Canvas(Modifier.fillMaxWidth().height(18.dp)) {
            val track = Path().apply {
                addRoundRect(RoundRect(0f, 2.dp.toPx(), size.width, 16.dp.toPx(), CornerRadius(7.dp.toPx())))
            }
            drawPath(track, Pc.Chip)
            clipPath(track) {
                state.bar.forEach { span ->
                    val x = size.width * span.startMinute.toFloat() / MINUTES_A_DAY
                    val w = size.width * span.lengthMinutes.toFloat() / MINUTES_A_DAY
                    drawRect(Pc.Ink, Offset(x, 0f), Size(w, size.height))
                }
            }
            if (state.error != null) {
                val x = size.width * (state.from.hour.toInt() * 60 + state.from.minute.toInt()) / MINUTES_A_DAY
                drawRect(Pc.Red, Offset(x - 1.dp.toPx(), 0f), Size(2.dp.toPx(), 18.dp.toPx()))
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            listOf("00", "06", "12", "18", "24").forEach { Text(it, style = body(10.sp, color = Pc.Muted)) }
        }
    }
}

private const val MINUTES_A_DAY = 24 * 60

// ---------------------------------------------------------------- Zone times

@Composable
fun ZoneTimesScreen(state: ZoneTimesEditorView?, repo: PingersRepository, nav: Navigator) {
    if (state == null) return Waiting()
    val window = state.times as? ZoneTimes.Window
    Page(
        top = { TopBar(state.title, { nav.back() }, stringResource(R.string.cd_back), subtitle = state.subtitle) },
        bottom = {
            MainButton(state.save, state.canSave, { repo.saveZoneTimes { nav.back() } }, Modifier.testTag(E2eTags.ZONE_TIMES_SAVE))
            Box(
                Modifier
                    .fillMaxWidth()
                    .height(48.dp)
                    .clip(RoundedCornerShape(24.dp))
                    .clickable(role = Role.Button) { repo.deleteZone { nav.back() } }
                    .testTag(E2eTags.ZONE_DELETE),
                contentAlignment = Alignment.Center,
            ) { Text(state.delete, style = body(16.sp, 600, Pc.Red)) }
        },
        spacing = 0,
    ) {
        SectionLabel(state.whenHeading, Modifier.padding(start = 4.dp, top = 18.dp, bottom = 8.dp))
        Card(Modifier.selectableGroup().semantics { contentDescription = state.whenLabel }, padding = PaddingValues(horizontal = 16.dp, vertical = 4.dp)) {
            ChoiceRow(state.always, state.isAlways) { repo.setZoneTimesDraft(ZoneTimes.Always) }
            RowDivider()
            ChoiceRow(state.certainTimes, !state.isAlways) {
                if (state.isAlways) repo.setZoneTimesDraft(DEFAULT_WINDOW)
            }
        }
        if (window != null) {
            Segments(
                state.days, ZoneDays.entries.indexOf(window.days),
                { repo.setZoneTimesDraft(window.copy(days = ZoneDays.entries[it])) },
                Modifier.padding(top = 14.dp).semantics { contentDescription = state.daysLabel },
            )
            Row(Modifier.padding(top = 12.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                TimeCard(state.from, state.fromTime.orEmpty(), state.fromAction.orEmpty(), window.from, state.error != null, null, false) {
                    repo.setZoneTimesDraft(window.copy(from = it))
                }
                TimeCard(state.until, state.untilTime.orEmpty(), state.untilAction.orEmpty(), window.to, state.error != null, null, false) {
                    repo.setZoneTimesDraft(window.copy(to = it))
                }
            }
            state.error?.let { ErrorLine(it, Modifier.padding(start = 4.dp, end = 4.dp, top = 10.dp)) }
        }
        Card(Modifier.padding(top = 14.dp), padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
            Text(
                buildAnnotatedString {
                    withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(state.summaryLead) }
                    append(state.summaryRest)
                },
                style = body(14.sp, lineHeight = 1.45f),
            )
        }
        Note(state.footnote)
    }
}

/** "Only at certain times" starts from every day, 08:00 to 18:00, until changed. */
private val DEFAULT_WINDOW = ZoneTimes.Window(ZoneDays.EVERY_DAY, TimeOfDay(8u, 0u), TimeOfDay(18u, 0u))

@Composable
internal fun ChoiceRow(title: String, selected: Boolean, subtitle: String? = null, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 52.dp).selectable(selected, role = Role.RadioButton, onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TwoLines(title, subtitle, Modifier.weight(1f), body(16.sp, 500), body(12.sp, color = Pc.Muted))
        RadioButton(selected, onClick = null, colors = RadioButtonDefaults.colors(selectedColor = Pc.Blue))
    }
}

// ---------------------------------------------------------------- Wake-ups

/**
 * Wake-ups: one screen whose layout follows [WakeupsView.kind] (canvas WakeupsOn, Wakeups,
 * WakeupsDelayed). The phone's facts are read again each time the app comes back, since the fix
 * for a fault happens in another app or in Android's settings.
 */
@Composable
fun WakeupsScreen(state: WakeupsView?, repo: PingersRepository, nav: Navigator) {
    val context = LocalContext.current
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { repo.refreshPhone() }
    if (state == null) return Waiting()
    var picking by remember { mutableStateOf(false) }
    val noWake: @Composable () -> Unit = {
        SwitchRow(
            state.noWakePushes.title, state.noWakePushes.detail, state.noWakePushesOn,
            { setNoWakePushes(context, repo, it); repo.refreshPhone() }, Modifier.testTag(E2eTags.WAKEUPS_NO_PUSHES),
        )
    }
    val pushService: @Composable () -> Unit = {
        LinkRow(state.pushService.title, state.pushService.detail, if (state.pushServiceChoosable) ({ picking = true }) else null)
    }
    val keepRunning: @Composable () -> Unit = {
        SwitchRow(state.keepRunning.title, state.keepRunning.detail, state.keepRunningOn, {
            AndroidPlatform.get(context).modes.keepRunning = it
            repo.refreshPhone()
        }, Modifier.testTag(E2eTags.WAKEUPS_KEEP_RUNNING))
    }
    Page(top = { TopBar(state.title, { nav.back() }, stringResource(R.string.cd_back)) }, spacing = 0) {
        when (state.kind) {
            WakeupsKind.OFF, WakeupsKind.WORKING -> {
                state.intro?.let {
                    Text(it, style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f), modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = 4.dp, bottom = 14.dp))
                }
                Card(padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        if (state.kind == WakeupsKind.WORKING) {
                            Icon(PIcons.CheckCircle, null, tint = Color.Unspecified, modifier = Modifier.size(24.dp))
                        } else {
                            Icon(PIcons.BellOff, null, tint = Pc.Muted, modifier = Modifier.size(24.dp))
                        }
                        TwoLines(state.statusTitle.orEmpty(), state.statusDetail, titleStyle = body(15.sp, 600), subtitleStyle = body(13.sp, color = Pc.Muted))
                    }
                }
                Heading(state.settingsHeading)
                Card {
                    noWake()
                    RowDivider()
                    pushService()
                    RowDivider()
                    keepRunning()
                }
                state.footnote?.let { Note(it) }
            }
            WakeupsKind.NOTHING_CAN_WAKE -> {
                FaultCard(state)
                state.fixHeading?.let { Heading(it) }
                Card(padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
                    Step("1", Pc.Ink, Color.White) {
                        Text(
                            buildAnnotatedString {
                                append(state.installBefore.orEmpty())
                                withStyle(SpanStyle(fontWeight = FontWeight.Bold)) { append(state.installApp.orEmpty()) }
                                append(state.installAfter.orEmpty())
                            },
                            style = body(15.sp, lineHeight = 1.4f),
                        )
                        state.installAction?.let { SmallPill(it, Modifier.testTag(E2eTags.WAKEUPS_GET_PUSH_APP)) { getNtfy(context) } }
                    }
                    RowDivider()
                    Step("2", Pc.Chip, Pc.Ink) { Text(state.comeBack.orEmpty(), style = body(15.sp, lineHeight = 1.4f)) }
                }
                Heading(state.settingsHeading)
                Card {
                    pushService()
                    RowDivider()
                    keepRunning()
                    RowDivider()
                    noWake()
                }
            }
            WakeupsKind.DELAYED -> {
                FaultCard(state)
                state.tryHeading?.let { Heading(it) }
                Card {
                    state.battery?.let { battery ->
                        ListRow(null) {
                            TwoLines(battery.title, battery.detail, Modifier.weight(1f), body(16.sp, 500), body(13.sp, color = Pc.Muted, lineHeight = 1.4f))
                            state.batteryAction?.let { SmallPill(it, Modifier.testTag(E2eTags.WAKEUPS_BATTERY)) { openBatterySettings(context) } }
                        }
                        RowDivider()
                    }
                    keepRunning()
                    state.notifications?.let { row ->
                        RowDivider()
                        NotificationsRow(row, state.notificationsAllowed) { openNotificationSettings(context) }
                    }
                }
                Heading(state.settingsHeading)
                Card {
                    pushService()
                    RowDivider()
                    noWake()
                }
            }
        }
    }
    if (picking) PushServicePicker(onDismiss = { picking = false }, onPicked = { picking = false; repo.refreshPhone() })
}

@Composable
private fun FaultCard(state: WakeupsView) {
    val shape = RoundedCornerShape(18.dp)
    Column(
        Modifier
            .padding(top = 8.dp)
            .fillMaxWidth()
            .clip(shape)
            .background(Pc.OrangeTintSoft)
            .border(1.dp, Pc.OrangeLine, shape)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(state.faultTitle.orEmpty(), style = body(16.sp, 600, Pc.OrangeText), modifier = Modifier.semantics { heading() })
        Text(state.faultBody.orEmpty(), style = body(14.sp, color = Pc.OrangeBrown, lineHeight = 1.45f))
    }
}

@Composable
private fun Step(number: String, circle: Color, digit: Color, content: @Composable ColumnScope.() -> Unit) {
    Row(Modifier.fillMaxWidth().padding(vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Box(Modifier.size(26.dp).clip(CircleShape).background(circle), contentAlignment = Alignment.Center) {
            Text(number, style = body(13.sp, 600, digit))
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp), content = content)
    }
}

/** The small blue button of "Get ntfy" and "Change". */
@Composable
internal fun SmallPill(text: String, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .height(40.dp)
            .clip(RoundedCornerShape(20.dp))
            .background(Pc.Blue)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = body(14.sp, 600, Color.White)) }
}

@Composable
private fun NotificationsRow(row: TitledRow, allowed: Boolean, onOpen: () -> Unit) {
    ListRow(if (allowed) null else onOpen, Modifier.testTag(E2eTags.WAKEUPS_NOTIFICATIONS)) {
        TwoLines(row.title, row.detail, Modifier.weight(1f), body(16.sp, 500), body(13.sp, color = Pc.Muted, lineHeight = 1.4f))
        if (allowed) Icon(PIcons.CheckCircle, null, tint = Color.Unspecified, modifier = Modifier.size(22.dp)) else ChevronIcon()
    }
}

private fun start(context: Context, intent: Intent): Boolean = try {
    context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    true
} catch (_: android.content.ActivityNotFoundException) {
    false
}

/** "Get ntfy": the store's page for it, or F-Droid's in a browser when there is no store. */
private fun getNtfy(context: Context) {
    if (!start(context, Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=io.heckel.ntfy")))) {
        start(context, Intent(Intent.ACTION_VIEW, Uri.parse("https://f-droid.org/packages/io.heckel.ntfy/")))
    }
}

private fun openBatterySettings(context: Context) {
    start(context, Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
}

private fun openNotificationSettings(context: Context) {
    start(context, Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName))
}
