package org.pingers.app.ui.components

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/**
 * The canvas's line icons, from its SVG paths (24-unit, round caps). They are drawn
 * black and tinted by Icon, so one vector serves every colour.
 */
object PIcons {
    private fun circle(cx: Float, cy: Float, r: Float) =
        "M${cx - r} ${cy}a$r $r 0 1 0 ${2 * r} 0a$r $r 0 1 0 ${-2 * r} 0z"

    private fun rect(x: Float, y: Float, w: Float, h: Float, r: Float) =
        "M${x + r} ${y}h${w - 2 * r}a$r $r 0 0 1 $r ${r}v${h - 2 * r}a$r $r 0 0 1 ${-r} ${r}" +
            "h${-(w - 2 * r)}a$r $r 0 0 1 ${-r} ${-r}v${-(h - 2 * r)}a$r $r 0 0 1 $r ${-r}z"

    private fun icon(name: String, width: Float, vararg paths: String): ImageVector =
        ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f).apply {
            paths.forEach {
                addPath(
                    pathData = addPathNodes(it),
                    stroke = SolidColor(Color.Black),
                    strokeLineWidth = width,
                    strokeLineCap = StrokeCap.Round,
                    strokeLineJoin = StrokeJoin.Round,
                )
            }
        }.build()

    private const val SHIELD = "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"
    private const val PIN = "M12 21s-7-6.2-7-11.5A7 7 0 0 1 19 9.5C19 14.8 12 21 12 21z"
    private val LOCK = arrayOf(rect(4f, 10f, 16f, 11f, 2f), "M8 10V7a4 4 0 0 1 8 0v3")

    val Settings = icon(
        "settings", 2f, circle(12f, 12f, 3f),
        "M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z",
    )
    val Shield = icon("shield", 2f, SHIELD)
    val ShieldCheck = icon("shield-check", 2.2f, SHIELD, "M9 12l2 2 4-4")
    val People = icon(
        "people", 2f, circle(9f, 8f, 3.5f), "M2.5 20c.8-3.5 3.4-5.5 6.5-5.5s5.7 2 6.5 5.5",
        circle(17f, 9f, 2.5f), "M17 14c2.4 0 4 1.6 4.5 4.5",
    )
    val PersonAdd = icon("person-add", 2f, circle(10f, 8f, 3.5f), "M3.5 20c.8-3.5 3.4-5.5 6.5-5.5s5.7 2 6.5 5.5M19 8v6M16 11h6")
    val Pin = icon("pin", 2f, PIN, circle(12f, 9.5f, 2.5f))
    val Plus = icon("plus", 2.4f, "M12 5v14M5 12h14")
    val Back = icon("back", 2.2f, "M15 5l-7 7 7 7")
    val Chevron = icon("chevron", 2.2f, "M9 6l6 6-6 6")
    val Close = icon("close", 2.2f, "M6 6l12 12M18 6L6 18")
    val Lock = icon("lock", 2.2f, *LOCK)
    val Moon = icon("moon", 2f, "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z")
    val Search = icon("search", 2.2f, circle(11f, 11f, 7f), "M20 20l-4-4")
    val Flag = icon("flag", 2.2f, "M5 21V4M5 4h11l-2 4 2 4H5")
    val Compass = icon("compass", 2.2f, circle(12f, 12f, 9f), "M15.5 8.5l-2 5-5 2 2-5z")
    val Eye = icon("eye", 2.2f, "M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12z", circle(12f, 12f, 3f))
    val Chat = icon("chat", 2f, "M21 12a8 8 0 0 1-11.6 7.1L4 20.5l1.4-4.9A8 8 0 1 1 21 12z")
    val Link = icon(
        "link", 2.2f, "M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7",
        "M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7",
    )
    val Share = icon("share", 2.2f, "M12 3v13M7 8l5-5 5 5M5 14v5a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-5")
    val Undo = icon("undo", 2.2f, "M9 14L4 9l5-5", "M4 9h10a6 6 0 0 1 0 12h-3")
    val Check = icon("check", 2.2f, "M20 6L9 17l-5-5")
    val Servers = icon(
        "servers", 2f, rect(3f, 4f, 18f, 7f, 2f), rect(3f, 13f, 18f, 7f, 2f), "M7 7.5h.01M7 16.5h.01",
    )
    val Bell = icon("bell", 2.2f, "M18 8a6 6 0 0 0-12 0c0 7-3 9-3 9h18s-3-2-3-9", "M13.7 21a2 2 0 0 1-3.4 0")
    val Globe = icon("globe", 2.2f, circle(12f, 12f, 9f), "M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18")
    val Trash = icon("trash", 2f, "M3 6h18M8 6V4h8v2M6 6l1 14h10l1-14")
    val Signal = icon(
        "signal", 2.2f, circle(12f, 12f, 2.5f),
        "M7.8 7.8a6 6 0 0 0 0 8.4M16.2 7.8a6 6 0 0 1 0 8.4M4.9 4.9a10 10 0 0 0 0 14.2M19.1 4.9a10 10 0 0 1 0 14.2",
    )
    val Send = icon("send", 2.2f, "M12 19V5M5 12l7-7 7 7")

    /** The filled green tick in a circle ("reachable", "joined"). */
    val CheckCircle: ImageVector = ImageVector.Builder("check-circle", 24.dp, 24.dp, 24f, 24f).apply {
        addPath(addPathNodes(circle(12f, 12f, 11f)), fill = SolidColor(Color(0xFF1F8A4C)))
        addPath(
            addPathNodes("M7 12.5l3.2 3.2L17 9"),
            stroke = SolidColor(Color.White),
            strokeLineWidth = 2.4f,
            strokeLineCap = StrokeCap.Round,
            strokeLineJoin = StrokeJoin.Round,
        )
    }.build()
}
