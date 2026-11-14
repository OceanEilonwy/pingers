package org.pingers.app.ui.theme

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import org.pingers.app.R

/** The design canvas's palette (docs/design-canvas), by role. */
object Pc {
    val Blue = Color(0xFF2F5BFF)
    val BlueDark = Color(0xFF1E40C8)
    val BlueTint = Color(0xFFE8EDFF)
    val BlueTintSoft = Color(0xFFF4F6FF)
    val BlueInk = Color(0xFF2B3A7A)
    val Orange = Color(0xFFE8590C)
    val OrangeBright = Color(0xFFFF7A3D)
    val OrangeOnInk = Color(0xFFFFB38A)
    val OrangeText = Color(0xFFB23C00)
    val OrangeTint = Color(0xFFFFEDE3)
    val OrangeTintSoft = Color(0xFFFFF3EC)
    val OrangeBorder = Color(0xFFF0C4B0)
    val OrangeBrown = Color(0xFF6B3A1F)
    val Ink = Color(0xFF15171C)
    val InkRaised = Color(0xFF262A33)
    val InkCircle = Color(0xFF2A2E38)
    val InkSubtle = Color(0xFFB9BDC7)
    val Lavender = Color(0xFFC9D4FF)
    val Paper = Color(0xFFF6F5F1)
    val Card = Color(0xFFFFFFFF)
    val Line = Color(0xFFE3E1DA)
    val Divider = Color(0xFFEEECE6)
    val Handle = Color(0xFFD6D3CB)
    val Muted = Color(0xFF5B606B)
    val Body = Color(0xFF3A3F49)
    val Chip = Color(0xFFF0EFEA)
    val ChipDark = Color(0xFFE9E7E1)
    val SystemPill = Color(0xFFEDEBE5)
    val Green = Color(0xFF1F8A4C)
    val GreenText = Color(0xFF1F7A45)
    /** Withdraw request, Don't let in. */
    val Red = Color(0xFFB42318)
    val Teal = Color(0xFF0F7B74)
    val Plum = Color(0xFF7A3E9D)
    val LockScreen = Color(0xFF1C2230)

    // The stylised placeholder map.
    val MapLand = Color(0xFFECEAE3)
    val MapWater = Color(0xFFCFE0EC)
    val MapPark = Color(0xFFDCE8D2)
    val MapPath = Color(0xFFF4F1E8)
}

private fun grotesk(weight: Int) = Font(
    R.font.space_grotesk,
    weight = FontWeight(weight),
    variationSettings = FontVariation.Settings(FontVariation.weight(weight)),
)

private fun inter(weight: Int) = Font(
    R.font.inter,
    weight = FontWeight(weight),
    variationSettings = FontVariation.Settings(FontVariation.weight(weight)),
)

/** Headings. */
val Grotesk = FontFamily(grotesk(500), grotesk(600), grotesk(700))

/** Body. */
val Inter = FontFamily(inter(400), inter(500), inter(600), inter(700))

fun heading(size: TextUnit, weight: Int = 600, spacing: TextUnit = TextUnit.Unspecified) = TextStyle(
    fontFamily = Grotesk,
    fontWeight = FontWeight(weight),
    fontSize = size,
    letterSpacing = spacing,
    color = Pc.Ink,
)

fun body(size: TextUnit, weight: Int = 400, color: Color = Pc.Ink, lineHeight: Float = 0f) = TextStyle(
    fontFamily = Inter,
    fontWeight = FontWeight(weight),
    fontSize = size,
    lineHeight = if (lineHeight > 0f) lineHeight.em else TextUnit.Unspecified,
    color = color,
)

/** The small upper-case section labels ("YOUR PARTIES"). */
val SectionStyle = TextStyle(
    fontFamily = Inter,
    fontWeight = FontWeight.SemiBold,
    fontSize = 13.sp,
    letterSpacing = 0.4.sp,
    color = Pc.Muted,
)

@Composable
fun PingersTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = lightColorScheme(
            primary = Pc.Blue,
            onPrimary = Color.White,
            secondary = Pc.Orange,
            onSecondary = Color.White,
            background = Pc.Paper,
            onBackground = Pc.Ink,
            surface = Pc.Card,
            onSurface = Pc.Ink,
            onSurfaceVariant = Pc.Muted,
            outline = Pc.Handle,
            outlineVariant = Pc.Line,
            error = Pc.OrangeText,
        ),
        typography = MaterialTheme.typography.let { t ->
            t.copy(
                bodyLarge = t.bodyLarge.copy(fontFamily = Inter),
                bodyMedium = t.bodyMedium.copy(fontFamily = Inter),
                bodySmall = t.bodySmall.copy(fontFamily = Inter),
                labelLarge = t.labelLarge.copy(fontFamily = Inter),
                labelMedium = t.labelMedium.copy(fontFamily = Inter),
                titleMedium = t.titleMedium.copy(fontFamily = Inter),
                titleLarge = t.titleLarge.copy(fontFamily = Grotesk),
            )
        },
        content = content,
    )
}
