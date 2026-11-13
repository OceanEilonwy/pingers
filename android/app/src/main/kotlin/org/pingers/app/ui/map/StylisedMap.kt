package org.pingers.app.ui.map

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.unit.Dp
import org.pingers.app.data.MapPoint
import org.pingers.app.ui.theme.Pc

/*
 * The stylised placeholder map from the design canvas: land, water, a park and
 * white roads, drawn from the canvas's own SVG shapes. MapLibre replaces it later;
 * everything placed on a map uses the scene's units (MapPoint), so the overlay code
 * changes only in how units become screen positions.
 */

sealed interface MapShape {
    data class Fill(val d: String, val color: Color) : MapShape
    data class Block(val x: Float, val y: Float, val w: Float, val h: Float, val r: Float, val color: Color = Pc.MapPark) : MapShape
    data class Road(val d: String, val width: Float, val color: Color = Color.White) : MapShape
}

class MapScene(val width: Float, val height: Float, val shapes: List<MapShape>) {
    internal val paths: Map<String, Path> by lazy {
        shapes.mapNotNull {
            when (it) {
                is MapShape.Fill -> it.d
                is MapShape.Road -> it.d
                is MapShape.Block -> null
            }
        }.associateWith { PathParser().parsePathString(it).toPath() }
    }
}

private fun water(d: String) = MapShape.Fill(d, Pc.MapWater)
private fun path(d: String) = MapShape.Road(d, 5f, Pc.MapPath)

object Scenes {
    val PartyCard = MapScene(
        350f, 230f,
        listOf(
            water("M0 170 C70 150 130 200 210 185 S320 150 350 165 L350 230 L0 230Z"),
            MapShape.Block(20f, 20f, 110f, 70f, 12f),
            MapShape.Road("M0 110 L350 85", 12f),
            MapShape.Road("M180 0 L160 230", 10f),
            MapShape.Road("M0 40 L350 55", 6f),
            MapShape.Road("M80 0 L100 230", 5f),
            MapShape.Road("M280 0 L300 230", 5f),
        ),
    )

    val SpotEditor = MapScene(
        390f, 480f,
        listOf(
            water("M0 330 C80 300 150 370 240 345 S360 300 390 320 L390 480 L0 480Z"),
            MapShape.Block(70f, 150f, 230f, 150f, 40f),
            MapShape.Road("M0 120 L390 100", 14f),
            MapShape.Road("M330 0 L350 480", 10f),
            MapShape.Road("M40 0 L30 480", 7f),
            MapShape.Road("M0 40 L390 55", 6f),
        ),
    )

    val DrawArea = MapScene(
        390f, 600f,
        listOf(
            water("M0 420 C80 390 150 460 240 435 S360 390 390 410 L390 600 L0 600Z"),
            MapShape.Block(70f, 150f, 250f, 210f, 40f),
            MapShape.Road("M0 120 L390 100", 14f),
            MapShape.Road("M340 0 L360 600", 10f),
            MapShape.Road("M40 0 L30 600", 7f),
            MapShape.Road("M0 40 L390 55", 6f),
            path("M90 300 C150 250 230 340 310 270"),
        ),
    )

    val Exclusion = MapScene(
        350f, 250f,
        listOf(
            MapShape.Block(200f, 140f, 130f, 90f, 14f),
            MapShape.Road("M0 95 L350 120", 12f),
            MapShape.Road("M150 0 L170 250", 10f),
            MapShape.Road("M0 200 L350 185", 6f),
            MapShape.Road("M270 0 L260 250", 5f),
        ),
    )

    private val meetupBase = listOf(
        water("M0 340 C80 310 150 380 240 355 S360 310 390 330 L390 460 L0 460Z"),
        MapShape.Block(50f, 130f, 280f, 190f, 44f),
        MapShape.Road("M0 110 L390 92", 14f),
        MapShape.Road("M350 0 L365 460", 10f),
        MapShape.Road("M20 0 L14 460", 7f),
    )

    val Meetup = MapScene(390f, 460f, meetupBase + path("M90 250 C150 200 230 290 310 220"))

    val LocationRequest = MapScene(390f, 460f, meetupBase)

    val MeetPoint = MapScene(
        390f, 480f,
        listOf(
            water("M0 360 C80 330 150 400 240 375 S360 330 390 350 L390 480 L0 480Z"),
            MapShape.Block(50f, 150f, 280f, 190f, 44f),
            MapShape.Road("M0 130 L390 112", 14f),
            MapShape.Road("M350 0 L365 480", 10f),
            path("M90 270 C150 220 230 310 310 240"),
        ),
    )
}

/** Draws [scene] scaled to the canvas width, shifted by [pan] scene units, then [overlay] in scene units. */
@Composable
fun MapCanvas(
    scene: MapScene,
    modifier: Modifier = Modifier,
    pan: MapPoint = MapPoint(0f, 0f),
    overlay: DrawScope.() -> Unit = {},
) {
    Canvas(modifier) {
        drawRect(Pc.MapLand)
        val s = size.width / scene.width
        scale(s, s, pivot = Offset.Zero) {
            translate(pan.x, pan.y) {
                scene.shapes.forEach { shape ->
                    when (shape) {
                        is MapShape.Fill -> drawPath(scene.paths.getValue(shape.d), shape.color)
                        is MapShape.Block -> drawRoundRect(
                            shape.color, Offset(shape.x, shape.y), Size(shape.w, shape.h), CornerRadius(shape.r),
                        )
                        is MapShape.Road -> drawPath(
                            scene.paths.getValue(shape.d), shape.color, style = Stroke(shape.width, cap = StrokeCap.Butt),
                        )
                    }
                }
                overlay()
            }
        }
    }
}

/**
 * A map at its scene's aspect ratio with composable overlays: [content] gets the
 * size of one scene unit, so a pin at (x, y) is `Modifier.offset(unit * x, unit * y)`.
 */
@Composable
fun MapBox(
    scene: MapScene,
    modifier: Modifier = Modifier,
    pan: MapPoint = MapPoint(0f, 0f),
    overlay: DrawScope.() -> Unit = {},
    content: @Composable BoxScope.(unit: Dp) -> Unit = {},
) {
    BoxWithConstraints(modifier.fillMaxWidth().aspectRatio(scene.width / scene.height)) {
        val unit = maxWidth / scene.width
        MapCanvas(scene, Modifier.fillMaxSize(), pan, overlay)
        Box(Modifier.fillMaxSize().offset(unit * pan.x, unit * pan.y)) { content(unit) }
    }
}

// ---------------------------------------------------------------- Overlay drawing, in scene units

fun DrawScope.spotCircle(center: MapPoint, radius: Float, dashed: Boolean, small: Boolean = false, strokeWidth: Float = 2f, alpha: Float = 0.12f) {
    val c = Offset(center.x, center.y)
    drawCircle(Pc.Blue.copy(alpha = alpha), radius, c)
    val effect = if (!dashed) null else if (small) PathEffect.dashPathEffect(floatArrayOf(4f, 4f)) else PathEffect.dashPathEffect(floatArrayOf(6f, 5f))
    drawCircle(Pc.Blue, radius, c, style = Stroke(strokeWidth, pathEffect = effect))
}

/** A hatched exclusion zone: [outline] is the zone's shape in scene units. */
fun DrawScope.hatched(outline: Path) {
    val b = outline.getBounds()
    clipPath(outline) {
        drawRect(Pc.Ink.copy(alpha = 0.06f), b.topLeft, b.size)
        val step = 8f * 1.4142f
        var x = b.left - b.height
        while (x < b.right) {
            drawLine(Pc.Ink.copy(alpha = 0.35f), Offset(x, b.bottom), Offset(x + b.height, b.top), strokeWidth = 2f)
            x += step
        }
    }
    drawPath(outline, Pc.Ink, style = Stroke(2f))
}
