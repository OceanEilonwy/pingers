package org.pingers.app.ui.screens

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
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
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.Route
import org.pingers.app.data.DrawAreaState
import org.pingers.app.data.DrawHint
import org.pingers.app.data.ExclusionZonesState
import org.pingers.app.data.MapPoint
import org.pingers.app.data.PingersRepository
import org.pingers.app.data.ShapeKind
import org.pingers.app.data.ShapeProblem
import org.pingers.app.data.SpotEditorState
import org.pingers.app.data.ZoneShape
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.ChevronIcon
import org.pingers.app.ui.components.LabeledField
import org.pingers.app.ui.components.ListRow
import org.pingers.app.ui.components.LockNote
import org.pingers.app.ui.components.MapButton
import org.pingers.app.ui.components.PField
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.components.PillButton
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.Segmented
import org.pingers.app.ui.components.SheetHandle
import org.pingers.app.ui.components.SwitchRow
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.components.TwoLines
import org.pingers.app.ui.map.MapBox
import org.pingers.app.ui.map.Scenes
import org.pingers.app.ui.map.hatched
import org.pingers.app.ui.map.spotCircle
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import org.pingers.app.words
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/** The white sheet over a full-screen map. */
@Composable
fun BoxScope.BottomSheet(height: Dp? = null, spacing: Int = 16, content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier
            .align(Alignment.BottomCenter)
            .fillMaxWidth()
            .then(if (height != null) Modifier.heightIn(min = height) else Modifier)
            .shadow(12.dp, RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp))
            .clip(RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp))
            .background(Pc.Card)
            .navigationBarsPadding()
            .padding(start = 20.dp, end = 20.dp, top = 10.dp, bottom = 16.dp),
        verticalArrangement = Arrangement.spacedBy(spacing.dp),
    ) {
        SheetHandle(Modifier.align(Alignment.CenterHorizontally))
        content()
    }
}

/** A dark map label ("300 m", "Home"). */
@Composable
fun MapLabel(text: String, modifier: Modifier = Modifier, radius: Dp = 8.dp) {
    Text(
        text,
        style = body(12.sp, 600, Color.White),
        modifier = modifier.clip(RoundedCornerShape(radius)).background(Pc.Ink).padding(horizontal = 8.dp, vertical = 4.dp),
    )
}

// ---------------------------------------------------------------- Spot editor

// The canvas draws a 300 m radius as 118 map units.
private const val METRES_PER_UNIT = 300f / 118f
private val SpotCentre = MapPoint(190f, 235f)

@Composable
fun SpotEditorScreen(state: SpotEditorState, repo: PingersRepository, nav: Navigator) {
    val drawn by repo.drawArea.collectAsStateWithLifecycle()
    val radius = state.radiusM / METRES_PER_UNIT
    val density = LocalDensity.current
    val current by rememberUpdatedState(state.radiusM)
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        MapBox(Scenes.SpotEditor, overlay = {
            if (state.shape == ShapeKind.Circle) {
                spotCircle(SpotCentre, radius, dashed = false, strokeWidth = 2.5f, alpha = 0.14f)
                drawCircle(Pc.Blue, 6f, Offset(SpotCentre.x, SpotCentre.y))
            } else if (drawn.closed) {
                val path = polygon(drawn.points.map(drawn.mapFrame::toMap), close = true)
                drawPath(path, Pc.Blue.copy(alpha = 0.16f))
                drawPath(path, Pc.Blue, style = Stroke(2.5f, join = StrokeJoin.Round))
            }
        }) { unit ->
            if (state.shape == ShapeKind.Circle) {
                val handleLabel = stringResource(R.string.cd_radius_handle, state.radiusM)
                Box(
                    Modifier
                        .offset(unit * (SpotCentre.x + radius) - 24.dp, unit * SpotCentre.y - 24.dp)
                        .size(48.dp)
                        .semantics { contentDescription = handleLabel }
                        .pointerInput(Unit) {
                            val unitPx = with(density) { unit.toPx() }
                            detectDragGestures { change, amount ->
                                change.consume()
                                val next = (current + amount.x / unitPx * METRES_PER_UNIT).toInt()
                                repo.setSpotRadius(next.coerceIn(state.minRadiusM, state.maxRadiusM))
                            }
                        },
                    contentAlignment = Alignment.Center,
                ) {
                    Box(Modifier.size(unit * 22).clip(CircleShape).background(Color.White).border(3.dp, Pc.Blue, CircleShape))
                }
                MapLabel(
                    stringResource(R.string.metres, state.radiusM),
                    Modifier.offset(unit * (SpotCentre.x + radius / 2f - 12f), unit * (SpotCentre.y + 17f)),
                )
            }
        }
        Row(
            Modifier.statusBarsPadding().padding(start = 16.dp, end = 16.dp, top = 12.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            MapButton(PIcons.Close, stringResource(R.string.cd_close), { nav.back() })
            Spacer(Modifier.weight(1f))
            var query by remember { mutableStateOf("") }
            PField(
                query, { query = it }, stringResource(R.string.spot_search),
                background = Pc.Card, border = Color.Transparent, height = 48.dp, fontSize = 15,
                placeholder = stringResource(R.string.spot_search),
                shape = RoundedCornerShape(24.dp),
                leading = { Icon(PIcons.Search, null, tint = Pc.Muted, modifier = Modifier.size(18.dp)) },
                modifier = Modifier.width(250.dp).shadow(2.dp, RoundedCornerShape(24.dp)),
            )
        }
        BottomSheet {
            LabeledField(stringResource(R.string.spot_name), state.name, repo::setSpotName, height = 52.dp)
            Segmented(
                listOf(stringResource(R.string.spot_circle), stringResource(R.string.spot_draw_area)),
                if (state.shape == ShapeKind.Circle) 0 else 1,
                { i -> if (i == 0) repo.setSpotShape(ShapeKind.Circle) else nav.go(Route.DrawArea) },
            )
            if (state.shape == ShapeKind.Circle) {
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Row(Modifier.fillMaxWidth()) {
                        Text(stringResource(R.string.spot_radius), style = body(13.sp, 600, Pc.Muted), modifier = Modifier.weight(1f))
                        Text(stringResource(R.string.metres, state.radiusM), style = body(13.sp, 600))
                    }
                    val radiusLabel = stringResource(R.string.spot_radius)
                    Slider(
                        value = state.radiusM.toFloat(),
                        onValueChange = { repo.setSpotRadius(it.toInt()) },
                        valueRange = state.minRadiusM.toFloat()..state.maxRadiusM.toFloat(),
                        colors = SliderDefaults.colors(thumbColor = Pc.Blue, activeTrackColor = Pc.Blue, inactiveTrackColor = Pc.Line),
                        modifier = Modifier.semantics { contentDescription = radiusLabel },
                    )
                }
            } else {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    TwoLines(
                        stringResource(R.string.spot_drawn_area),
                        state.areaHectares?.let { stringResource(R.string.shape_area, org.pingers.app.Words.hectares(it)) },
                        Modifier.weight(1f), body(15.sp, 600),
                    )
                    Box(
                        Modifier.heightIn(min = 48.dp).clickable(role = Role.Button) { nav.go(Route.DrawArea) }.padding(horizontal = 8.dp),
                        contentAlignment = Alignment.Center,
                    ) { Text(stringResource(R.string.spot_edit_area), style = body(15.sp, 600, Pc.Blue)) }
                }
            }
            LockNote(stringResource(R.string.spot_encrypted, state.partyName))
            PillButton(stringResource(R.string.spot_save), { repo.saveSpot(); nav.go(Route.Party) }, enabled = state.canSave)
        }
    }
}

private fun polygon(points: List<MapPoint>, close: Boolean): Path = Path().apply {
    points.forEachIndexed { i, p -> if (i == 0) moveTo(p.x, p.y) else lineTo(p.x, p.y) }
    if (close) close()
}

@Composable
private fun shapeProblem(problem: ShapeProblem): String = stringResource(
    when (problem) {
        ShapeProblem.Crossing -> R.string.draw_problem_crossing
        ShapeProblem.NoArea -> R.string.draw_problem_no_area
        ShapeProblem.TooLarge -> R.string.draw_problem_too_large
    },
)

// ---------------------------------------------------------------- Draw area

@Composable
fun DrawAreaScreen(state: DrawAreaState, repo: PingersRepository, nav: Navigator) {
    val density = LocalDensity.current
    val w = words()
    val frame = state.mapFrame
    val points = state.points.map(frame::toMap)
    val current by rememberUpdatedState(state)
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        MapBox(
            Scenes.DrawArea,
            overlay = {
                if (points.isNotEmpty()) {
                    val path = polygon(points, state.closed)
                    if (state.closed) {
                        drawPath(path, Pc.Blue.copy(alpha = 0.16f))
                        drawPath(path, Pc.Blue, style = Stroke(2.5f, join = StrokeJoin.Round))
                    } else {
                        drawPath(path, Pc.Blue, style = Stroke(2.5f, join = StrokeJoin.Round, pathEffect = PathEffect.dashPathEffect(floatArrayOf(7f, 5f))))
                    }
                }
            },
        ) { unit ->
            val unitPx = with(density) { unit.toPx() }
            Box(
                Modifier.fillMaxSize().pointerInput(Unit) {
                    detectTapGestures { o -> repo.drawTapMap(current.mapFrame.toCoordinate(MapPoint(o.x / unitPx, o.y / unitPx))) }
                },
            )
            state.midpoints.forEachIndexed { i, c ->
                val m = frame.toMap(c)
                val label = stringResource(R.string.cd_add_point_between, i + 1, (i + 1) % state.points.size + 1)
                Box(
                    Modifier
                        .offset(unit * m.x - 24.dp, unit * m.y - 24.dp)
                        .size(48.dp)
                        .clip(CircleShape)
                        .clickable(role = Role.Button) { repo.drawInsertMidpoint(i) }
                        .semantics { contentDescription = label },
                    contentAlignment = Alignment.Center,
                ) {
                    Box(
                        Modifier.size(unit * 22).clip(CircleShape).background(Color.White.copy(alpha = 0.85f)).border(2.dp, Pc.Blue, CircleShape),
                        contentAlignment = Alignment.Center,
                    ) { Text("+", style = body(14.sp, 600, Pc.Blue)) }
                }
            }
            points.forEachIndexed { i, p ->
                Vertex(i, p, state, unit, unitPx, repo)
            }
        }
        Row(
            Modifier.statusBarsPadding().padding(start = 16.dp, end = 16.dp, top = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            MapButton(PIcons.Back, stringResource(R.string.cd_back_spot_editor), { nav.back() })
            Box(
                Modifier.heightIn(min = 44.dp).shadow(2.dp, RoundedCornerShape(22.dp)).clip(RoundedCornerShape(22.dp)).background(Pc.Card).padding(horizontal = 16.dp),
                contentAlignment = Alignment.Center,
            ) { Text(stringResource(R.string.draw_title, state.spotName), style = body(14.sp, 600)) }
        }
        BottomSheet(spacing = 14) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom) {
                Text(
                    pluralStringResource(R.plurals.draw_points, state.points.size, state.points.size),
                    style = heading(20.sp),
                    modifier = Modifier.weight(1f),
                )
                Text(state.areaM2?.let { w.area(it) } ?: stringResource(R.string.draw_open), style = body(13.sp, color = Pc.Muted))
            }
            Text(
                when (val h = state.hint) {
                    DrawHint.FirstPoint -> stringResource(R.string.draw_hint_first)
                    DrawHint.KeepTapping -> stringResource(R.string.draw_hint_keep)
                    DrawHint.CloseShape -> stringResource(R.string.draw_hint_close)
                    is DrawHint.Selected -> stringResource(R.string.draw_hint_selected, h.number)
                    is DrawHint.Fix -> shapeProblem(h.problem)
                    DrawHint.Adjust -> stringResource(R.string.draw_hint_adjust)
                },
                style = body(14.sp, color = Pc.Body, lineHeight = 1.45f),
                modifier = Modifier.heightIn(min = 40.dp),
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                SmallButton(stringResource(R.string.draw_undo), repo::drawUndo, Modifier.weight(1f), enabled = state.canUndo, icon = true)
                if (state.canDelete) {
                    SmallButton(
                        stringResource(R.string.draw_delete), repo::drawDeleteSelected, Modifier.weight(1f),
                        bg = Pc.OrangeTintSoft, border = Pc.OrangeBorder, fg = Pc.OrangeText,
                    )
                }
                SmallButton(stringResource(R.string.draw_clear), repo::drawClear, Modifier.weight(1f))
            }
            if (state.closed) {
                PillButton(stringResource(R.string.draw_use), { repo.useDrawnArea(); nav.go(Route.SpotEditor) }, enabled = state.canUse)
            } else {
                PillButton(stringResource(R.string.draw_close_first), {}, background = Pc.Line, content = Pc.Muted, enabled = false)
            }
        }
    }
}

@Composable
private fun Vertex(i: Int, p: MapPoint, state: DrawAreaState, unit: Dp, unitPx: Float, repo: PingersRepository) {
    val first = i == 0 && state.closable
    val selected = state.selected == i
    val size = if (first) 30 else 24
    val point by rememberUpdatedState(p)
    val label = if (first) stringResource(R.string.cd_point_close) else stringResource(R.string.cd_point, i + 1)
    Box(
        Modifier
            .offset(unit * p.x - 24.dp, unit * p.y - 24.dp)
            .size(48.dp)
            .semantics {
                contentDescription = label
                this.selected = selected
            }
            .pointerInput(i) {
                detectDragGestures(onDragEnd = repo::drawDragEnd, onDragCancel = repo::drawDragEnd) { change, amount ->
                    change.consume()
                    repo.drawDragPoint(i, state.mapFrame.toCoordinate(MapPoint(point.x + amount.x / unitPx, point.y + amount.y / unitPx)))
                }
            }
            .clickable(role = Role.Button, interactionSource = null, indication = null) { repo.drawTapPoint(i) },
        contentAlignment = Alignment.Center,
    ) {
        Box(
            Modifier
                .size(unit * size)
                .shadow(2.dp, CircleShape)
                .clip(CircleShape)
                .background(if (selected) Pc.Blue else Color.White)
                .border(3.dp, if (first) Pc.Orange else Pc.Blue, CircleShape),
        )
    }
}

@Composable
private fun SmallButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier,
    enabled: Boolean = true,
    bg: Color = Pc.Card,
    border: Color = Pc.Handle,
    fg: Color = Pc.Ink,
    icon: Boolean = false,
) {
    val color = if (enabled) fg else Pc.Muted.copy(alpha = 0.6f)
    Row(
        modifier
            .height(48.dp)
            .clip(RoundedCornerShape(24.dp))
            .background(bg)
            .border(1.dp, border, RoundedCornerShape(24.dp))
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (icon) {
            Icon(PIcons.Undo, null, tint = color, modifier = Modifier.size(16.dp))
            Spacer(Modifier.width(6.dp))
        }
        Text(text, style = body(14.sp, 600, color), textAlign = TextAlign.Center)
    }
}

// ---------------------------------------------------------------- Exclusion zones

@Composable
fun ExclusionZonesScreen(state: ExclusionZonesState, repo: PingersRepository, nav: Navigator) {
    val context = LocalContext.current
    val soon = stringResource(R.string.zones_edit_soon)
    val notYet = { Toast.makeText(context, soon, Toast.LENGTH_SHORT).show() }
    Page(
        top = { TopBar(stringResource(R.string.zones_title), { nav.back() }, stringResource(R.string.cd_back)) },
        spacing = 0,
    ) {
        Text(
            stringResource(R.string.zones_intro),
            style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f),
            modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = 4.dp, bottom = 14.dp),
        )
        Box(Modifier.clip(RoundedCornerShape(22.dp)).border(1.dp, Pc.Line, RoundedCornerShape(22.dp))) {
            MapBox(Scenes.Exclusion, overlay = {
                // Placeholder geometry for the two example zones, as drawn on the canvas.
                hatched(Path().apply { addOval(androidx.compose.ui.geometry.Rect(Offset(80f, 60f), 42f)) })
                hatched(Path().apply { addRoundRect(RoundRect(215f, 25f, 315f, 95f, CornerRadius(8f))) })
                state.me?.let(state.mapFrame::toMap)?.let { me ->
                    drawCircle(Color.White, 8.5f, Offset(me.x, me.y))
                    drawCircle(Pc.Blue, 7f, Offset(me.x, me.y))
                }
            }) { unit ->
                state.zones.forEach { z ->
                    val at = state.mapFrame.toMap(z.labelAt)
                    MapLabel(z.name, Modifier.offset(unit * at.x, unit * at.y))
                }
            }
        }
        Spacer(Modifier.height(16.dp))
        Card {
            state.zones.forEach { z ->
                ListRow(notYet) {
                    val shape = when (val s = z.shape) {
                        is ZoneShape.Circle -> stringResource(R.string.zone_circle, s.radiusM)
                        ZoneShape.Area -> stringResource(R.string.zone_area)
                    }
                    TwoLines(z.name, listOf(shape, z.schedule ?: stringResource(R.string.zone_all_parties)).joinToString(" · "), Modifier.weight(1f))
                    ChevronIcon()
                }
                RowDivider()
            }
            ListRow(notYet, padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
                Icon(PIcons.Plus, null, tint = Pc.Blue, modifier = Modifier.size(18.dp))
                Text(stringResource(R.string.zones_add), style = body(16.sp, 600, Pc.Blue))
            }
        }
        Spacer(Modifier.height(14.dp))
        Card {
            SwitchRow(
                stringResource(R.string.zones_hide_trail), stringResource(R.string.zones_hide_trail_help),
                state.hideTrail, repo::setHideTrail, titleWeight = 600, titleSize = 15,
            )
        }
    }
}
