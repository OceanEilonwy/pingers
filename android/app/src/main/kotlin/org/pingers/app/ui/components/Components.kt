package org.pingers.app.ui.components

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
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.data.PartyColour
import org.pingers.app.data.Person
import org.pingers.app.data.PrivacyLevel
import org.pingers.app.data.Tint
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.SectionStyle
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading

fun Tint.color(): Color = when (this) {
    Tint.Peach -> Color(0xFFFFD9C7)
    Tint.Sky -> Color(0xFFCFE0FF)
    Tint.Sage -> Color(0xFFDDEFD3)
    Tint.Butter -> Color(0xFFFFE7B8)
    Tint.Lilac -> Color(0xFFEADFF2)
    Tint.Mint -> Color(0xFFD6EEEC)
    Tint.Stone -> Pc.Chip
}

fun PartyColour.color(): Color = when (this) {
    PartyColour.Blue -> Pc.Blue
    PartyColour.Orange -> Pc.Orange
    PartyColour.Ink -> Pc.Ink
    PartyColour.Teal -> Pc.Teal
    PartyColour.Plum -> Pc.Plum
}

/** A round initials avatar. */
@Composable
fun Avatar(
    person: Person,
    size: Dp,
    fontSize: Int,
    modifier: Modifier = Modifier,
    ring: Color? = null,
    ringWidth: Dp = 2.dp,
) {
    InitialsCircle(person.initials, person.tint.color(), size, fontSize, modifier, ring, ringWidth)
}

@Composable
fun InitialsCircle(
    text: String,
    background: Color,
    size: Dp,
    fontSize: Int,
    modifier: Modifier = Modifier,
    ring: Color? = null,
    ringWidth: Dp = 2.dp,
) {
    Box(
        modifier
            .size(size)
            .then(if (ring != null) Modifier.border(ringWidth, ring, CircleShape) else Modifier)
            .padding(if (ring != null) ringWidth else 0.dp)
            .clip(CircleShape)
            .background(background),
        contentAlignment = Alignment.Center,
    ) {
        Text(text, style = body(fontSize.sp, 600))
    }
}

/** Overlapping avatars, as on the meetup card and invite. */
@Composable
fun AvatarStack(people: List<Person>, size: Dp, fontSize: Int, ring: Color, extra: String? = null) {
    Row(horizontalArrangement = Arrangement.spacedBy((-8).dp)) {
        people.forEach { p -> Avatar(p, size, fontSize, ring = ring) }
        if (extra != null) InitialsCircle(extra, Tint.Lilac.color(), size, fontSize, ring = ring)
    }
}

/** The page header: back button and title, as on every pushed screen. */
@Composable
fun TopBar(
    title: String,
    onBack: () -> Unit,
    backLabel: String,
    modifier: Modifier = Modifier,
    subtitle: String? = null,
    titleSize: Int = 22,
    titleColor: Color = Pc.Ink,
    actions: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier
            .fillMaxWidth()
            .statusBarsPadding()
            .padding(start = 10.dp, end = 10.dp, top = 12.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        IconAction(PIcons.Back, backLabel, onBack)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(1.dp)) {
            Text(title, style = heading(titleSize.sp).copy(color = titleColor), modifier = Modifier.semantics { heading() })
            if (subtitle != null) Text(subtitle, style = body(12.sp, color = Pc.Muted))
        }
        actions()
    }
}

/** A 48 dp icon-only button; [label] is read by TalkBack. */
@Composable
fun IconAction(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tint: Color = Pc.Ink,
    background: Color = Color.Transparent,
    border: Color? = null,
    iconSize: Dp = 22.dp,
    size: Dp = 48.dp,
) {
    Box(
        modifier
            .size(size)
            .clip(CircleShape)
            .background(background)
            .then(if (border != null) Modifier.border(1.dp, border, CircleShape) else Modifier)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(iconSize))
    }
}

/** A white floating icon button over a map. */
@Composable
fun MapButton(icon: ImageVector, label: String, onClick: () -> Unit, modifier: Modifier = Modifier, tint: Color = Pc.Ink) {
    IconAction(icon, label, onClick, modifier.shadow(2.dp, CircleShape), tint = tint, background = Pc.Card, iconSize = 20.dp)
}

/** The white rounded card the canvas lists sit in. */
@Composable
fun Card(
    modifier: Modifier = Modifier,
    radius: Dp = 18.dp,
    background: Color = Pc.Card,
    border: Color? = Pc.Line,
    padding: PaddingValues = PaddingValues(0.dp),
    content: @Composable ColumnScope.() -> Unit,
) {
    val shape = RoundedCornerShape(radius)
    Column(
        modifier
            .fillMaxWidth()
            .clip(shape)
            .background(background)
            .then(if (border != null) Modifier.border(1.dp, border, shape) else Modifier)
            .padding(padding),
        content = content,
    )
}

@Composable
fun RowDivider(modifier: Modifier = Modifier) = HorizontalDivider(modifier, thickness = 1.dp, color = Pc.Divider)

@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(text.uppercase(), style = SectionStyle, modifier = modifier.semantics { heading() })
}

/** Title and grey subtitle, the two-line text of most rows. */
@Composable
fun TwoLines(
    title: String,
    subtitle: String?,
    modifier: Modifier = Modifier,
    titleStyle: TextStyle = body(16.sp, 600),
    subtitleStyle: TextStyle = body(13.sp, color = Pc.Muted, lineHeight = 1.4f),
) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(title, style = titleStyle)
        if (subtitle != null) Text(subtitle, style = subtitleStyle)
    }
}

/** A tappable row inside a Card. */
@Composable
fun ListRow(
    onClick: (() -> Unit)?,
    modifier: Modifier = Modifier,
    padding: PaddingValues = PaddingValues(horizontal = 16.dp, vertical = 14.dp),
    content: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .then(if (onClick != null) Modifier.clickable(onClick = onClick) else Modifier)
            .padding(padding),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        content = content,
    )
}

@Composable
fun ChevronIcon(tint: Color = Pc.Muted) {
    Icon(PIcons.Chevron, contentDescription = null, tint = tint, modifier = Modifier.size(18.dp))
}

/** A row with a label and a switch; the whole row toggles. */
@Composable
fun SwitchRow(
    title: String,
    subtitle: String?,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
    titleWeight: Int = 500,
    titleSize: Int = 16,
) {
    Row(
        modifier
            .fillMaxWidth()
            .heightIn(min = 52.dp)
            .clickable(role = Role.Switch) { onChange(!checked) }
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        TwoLines(title, subtitle, Modifier.weight(1f), titleStyle = body(titleSize.sp, titleWeight))
        PSwitch(checked, null)
    }
}

@Composable
fun PSwitch(checked: Boolean, onChange: ((Boolean) -> Unit)?) {
    Switch(
        checked = checked,
        onCheckedChange = onChange,
        colors = SwitchDefaults.colors(
            checkedTrackColor = Pc.Blue,
            checkedThumbColor = Color.White,
            uncheckedTrackColor = Pc.Line,
            uncheckedThumbColor = Color.White,
            uncheckedBorderColor = Pc.Handle,
        ),
    )
}

/** The canvas's pill button. */
@Composable
fun PillButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    background: Color = Pc.Blue,
    content: Color = Color.White,
    border: Color? = null,
    height: Dp = 52.dp,
    enabled: Boolean = true,
    fontSize: Int = 16,
    icon: ImageVector? = null,
) {
    val shape = RoundedCornerShape(height / 2)
    Row(
        modifier
            .fillMaxWidth()
            .height(height)
            .clip(shape)
            .background(background)
            .then(if (border != null) Modifier.border(1.dp, border, shape) else Modifier)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon != null) {
            Icon(icon, contentDescription = null, tint = content, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(8.dp))
        }
        Text(text, style = body(fontSize.sp, 600, content))
    }
}

/** A small status pill ("Live", "Self-hosted"). */
@Composable
fun Tag(text: String, color: Color, background: Color, modifier: Modifier = Modifier, icon: ImageVector? = null) {
    Row(
        modifier
            .clip(RoundedCornerShape(999.dp))
            .background(background)
            .padding(start = if (icon != null) 7.dp else 10.dp, end = 10.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (icon != null) Icon(icon, null, tint = color, modifier = Modifier.size(13.dp))
        Text(text, style = body(12.sp, 600, color))
    }
}

/** The grabber at the top of a bottom sheet. */
@Composable
fun SheetHandle(modifier: Modifier = Modifier) {
    Box(
        modifier
            .size(40.dp, 5.dp)
            .clip(RoundedCornerShape(3.dp))
            .background(Pc.Handle),
    )
}

/** The canvas's two- or three-way segmented control. */
@Composable
fun Segmented(
    options: List<String>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
    fontSize: Int = 14,
) {
    Row(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(Pc.Chip)
            .padding(3.dp),
        horizontalArrangement = Arrangement.spacedBy(3.dp),
    ) {
        options.forEachIndexed { i, label ->
            val on = i == selected
            Box(
                Modifier
                    .weight(1f)
                    .height(42.dp)
                    .then(if (on) Modifier.shadow(1.dp, RoundedCornerShape(10.dp)) else Modifier)
                    .clip(RoundedCornerShape(10.dp))
                    .background(if (on) Pc.Card else Color.Transparent)
                    .semantics { this.selected = on }
                    .clickable(role = Role.Tab) { onSelect(i) },
                contentAlignment = Alignment.Center,
            ) {
                Text(label, style = body(fontSize.sp, 600, if (on) Pc.Ink else Pc.Muted), textAlign = TextAlign.Center)
            }
        }
    }
}

/** A labelled single-line field in the canvas's style. */
@Composable
fun LabeledField(
    label: String,
    value: String,
    onChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    background: Color = Pc.Paper,
    border: Color = Pc.Line,
    borderWidth: Dp = 1.dp,
    height: Dp = 52.dp,
    fontSize: Int = 16,
    placeholder: String? = null,
    trailing: (@Composable () -> Unit)? = null,
    helper: String? = null,
) {
    Column(modifier, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(label, style = body(13.sp, 600, Pc.Muted))
        PField(value, onChange, label, Modifier, background, border, borderWidth, height, fontSize, placeholder, trailing)
        if (helper != null) Text(helper, style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f))
    }
}

@Composable
fun PField(
    value: String,
    onChange: (String) -> Unit,
    description: String,
    modifier: Modifier = Modifier,
    background: Color = Pc.Paper,
    border: Color = Pc.Line,
    borderWidth: Dp = 1.dp,
    height: Dp = 52.dp,
    fontSize: Int = 16,
    placeholder: String? = null,
    trailing: (@Composable () -> Unit)? = null,
    shape: Shape = RoundedCornerShape(14.dp),
    leading: (@Composable () -> Unit)? = null,
    textStyle: TextStyle = body(fontSize.sp),
    keyboardOptions: KeyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
    enabled: Boolean = true,
) {
    TextField(
        value = value,
        onValueChange = onChange,
        singleLine = true,
        enabled = enabled,
        textStyle = textStyle,
        placeholder = placeholder?.let { { Text(it, style = textStyle.copy(color = Pc.Muted)) } },
        trailingIcon = trailing,
        leadingIcon = leading,
        keyboardOptions = keyboardOptions,
        shape = shape,
        colors = TextFieldDefaults.colors(
            focusedContainerColor = background,
            unfocusedContainerColor = background,
            disabledContainerColor = background,
            focusedIndicatorColor = Color.Transparent,
            unfocusedIndicatorColor = Color.Transparent,
            disabledIndicatorColor = Color.Transparent,
            disabledTextColor = Pc.Muted,
            cursorColor = Pc.Blue,
        ),
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = height)
            .border(borderWidth, border, shape)
            .semantics { contentDescription = description },
    )
}

/** The pale panel with a lock that explains what stays encrypted. */
@Composable
fun LockNote(text: String, modifier: Modifier = Modifier, background: Color = Pc.Paper) {
    Row(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(background)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(PIcons.Lock, null, tint = Pc.Blue, modifier = Modifier.padding(top = 1.dp).size(18.dp))
        Text(text, style = body(13.sp, color = Pc.Body, lineHeight = 1.45f))
    }
}

/** A circular icon badge, as at the start of info rows. */
@Composable
fun IconBadge(icon: ImageVector, tint: Color, background: Color, size: Dp = 40.dp, shape: Shape = CircleShape, iconSize: Dp = 20.dp) {
    Box(Modifier.size(size).clip(shape).background(background), contentAlignment = Alignment.Center) {
        Icon(icon, null, tint = tint, modifier = Modifier.size(iconSize))
    }
}

/** "pingers.flat7.org · Full privacy": opens the server's privacy details. */
@Composable
fun ServerCard(
    title: String,
    subtitle: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    privacy: PrivacyLevel? = PrivacyLevel.Full,
) {
    val shape = RoundedCornerShape(16.dp)
    Row(
        modifier
            .fillMaxWidth()
            .heightIn(min = 52.dp)
            .clip(shape)
            .background(Pc.Card)
            .border(1.dp, Pc.Line, shape)
            .clickable(onClick = onClick)
            .padding(horizontal = 14.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        if (privacy == PrivacyLevel.Reduced) {
            Icon(PIcons.ShieldAlert, null, tint = Pc.OrangeText, modifier = Modifier.size(20.dp))
        } else {
            Icon(PIcons.ShieldCheck, null, tint = Pc.BlueDark, modifier = Modifier.size(20.dp))
        }
        TwoLines(title, subtitle, Modifier.weight(1f), body(15.sp, 600), body(12.sp, color = Pc.Muted))
        ChevronIcon()
    }
}
