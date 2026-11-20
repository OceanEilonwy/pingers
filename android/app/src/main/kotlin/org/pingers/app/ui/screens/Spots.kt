package org.pingers.app.ui.screens

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import org.pingers.app.platform.hasLocationPermission
import org.pingers.app.ui.components.IconAction
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
import androidx.compose.ui.geometry.Offset
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
import androidx.compose.ui.platform.testTag
import org.pingers.app.ui.E2eTags
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
import org.pingers.app.data.MapFrame
import org.pingers.app.maps.Place
import org.pingers.app.ui.map.LocalMaps
import org.pingers.app.ui.map.MapBox
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalFocusManager
import org.pingers.app.ui.map.InteractiveMap
import org.pingers.app.ui.map.Scenes
import org.pingers.app.ui.map.rememberGoToMyLocation
import org.pingers.app.ui.map.rememberMapState
import org.pingers.app.ui.map.hatched
import org.pingers.app.ui.map.spotCircle
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import org.pingers.app.ui.theme.heading
import org.pingers.app.words
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * The white sheet over a full-screen map. Tapping the handle, or dragging it down, folds the sheet
 * away to just the handle so the map is free; tapping or dragging up brings it back.
 */
@Composable
fun BoxScope.BottomSheet(height: Dp? = null, spacing: Int = 16, content: @Composable ColumnScope.() -> Unit) {
    var collapsed by rememberSaveable { mutableStateOf(false) }
    var dragged by remember { mutableFloatStateOf(0f) }
    val threshold = with(LocalDensity.current) { SHEET_DRAG.toPx() }
    val label = stringResource(if (collapsed) R.string.cd_sheet_expand else R.string.cd_sheet_collapse)
    Column(
        Modifier
            .align(Alignment.BottomCenter)
            .fillMaxWidth()
            .then(if (height != null && !collapsed) Modifier.heightIn(min = height) else Modifier)
            .shadow(12.dp, RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp))
            .clip(RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp))
            .background(Pc.Card)
            .navigationBarsPadding()
            .padding(start = 20.dp, end = 20.dp, bottom = 16.dp),
    ) {
        Box(
            Modifier
                .fillMaxWidth()
                .draggable(
                    rememberDraggableState { dragged += it },
                    Orientation.Vertical,
                    onDragStarted = { dragged = 0f },
                    onDragStopped = {
                        if (dragged > threshold) collapsed = true else if (dragged < -threshold) collapsed = false
                    },
                )
                .clickable(role = Role.Button, onClickLabel = label, interactionSource = null, indication = null) { collapsed = !collapsed }
                .semantics { contentDescription = label }
                .padding(top = 10.dp, bottom = spacing.dp),
            contentAlignment = Alignment.TopCenter,
        ) { SheetHandle() }
        AnimatedVisibility(!collapsed, enter = expandVertically(), exit = shrinkVertically()) {
            Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(spacing.dp), content = content)
        }
    }
}

// How far the handle must be dragged to fold or unfold the sheet.
private val SHEET_DRAG = 24.dp

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
    // Pinching scales the map about the Spot's centre: a zoom of 2 shows twice the metres per unit.
    val view = rememberMapState()
    val metresPerUnit = METRES_PER_UNIT * view.zoom
    val radius = state.radiusM / metresPerUnit
    val density = LocalDensity.current
    val current by rememberUpdatedState(state.radiusM)
    val currentMetresPerUnit by rememberUpdatedState(metresPerUnit)
    // The circle sits at SpotCentre; the frame puts the Spot's centre there, so the map moves under it.
    val frame = MapFrame(state.centre, SpotCentre, METRES_PER_UNIT)
    var searching by remember { mutableStateOf(false) }
    val locate = rememberGoToMyLocation(state.centre, repo::moveSpot)
    val context = LocalContext.current
    // A new Spot starts where the phone is now; asking for permission waits for the button.
    LaunchedEffect(Unit) { if (state.name.isEmpty() && hasLocationPermission(context)) locate() }
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        // Dragging the map moves the Spot's centre: the map slides, the circle stays put.
        InteractiveMap(Scenes.SpotEditor, frame, view = view, moveTo = repo::moveSpot, overlay = { f ->
            if (state.shape == ShapeKind.Circle) {
                spotCircle(SpotCentre, radius, dashed = false, strokeWidth = 2.5f, alpha = 0.14f)
                drawCircle(Pc.Blue, 6f, Offset(SpotCentre.x, SpotCentre.y))
            } else if (drawn.closed) {
                val path = polygon(drawn.points.map(f::toMap), close = true)
                drawPath(path, Pc.Blue.copy(alpha = 0.16f))
                drawPath(path, Pc.Blue, style = Stroke(2.5f, join = StrokeJoin.Round))
            }
        }) { unit, _ ->
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
                                val next = (current + amount.x / unitPx * currentMetresPerUnit).toInt()
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
            verticalAlignment = Alignment.Top,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            MapButton(PIcons.Close, stringResource(R.string.cd_close), { nav.back() })
            PlaceSearchButton({ searching = true }, Modifier.weight(1f))
        }
        BottomSheet {
            LabeledField(stringResource(if (state.forZone) R.string.zone_name else R.string.spot_name), state.name, repo::setSpotName, height = 52.dp)
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
            if (state.forZone) {
                LockNote(stringResource(R.string.zone_on_phone))
                PillButton(stringResource(R.string.zone_save), { repo.saveZone { nav.go(Route.ExclusionZones) } }, enabled = state.canSave)
            } else {
                LockNote(stringResource(R.string.spot_encrypted, state.partyName))
                PillButton(stringResource(R.string.spot_save), { repo.saveSpot(); nav.go(Route.Party) }, Modifier.testTag(E2eTags.SPOT_SAVE), enabled = state.canSave)
            }
        }
        if (searching) PlaceSearchScreen(onPick = { repo.moveSpot(it.at) }, onClose = { searching = false })
    }
}

/** The "Search a place" pill over the map; tapping it opens [PlaceSearchScreen]. */
@Composable
internal fun PlaceSearchButton(onClick: () -> Unit, modifier: Modifier = Modifier) {
    val label = stringResource(R.string.spot_search)
    Row(
        modifier
            .height(48.dp)
            .shadow(2.dp, RoundedCornerShape(24.dp))
            .clip(RoundedCornerShape(24.dp))
            .background(Pc.Card)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = label }
            .padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(PIcons.Search, null, tint = Pc.Muted, modifier = Modifier.size(18.dp))
        Text(label, style = body(15.sp, color = Pc.Muted), maxLines = 1)
    }
}

/**
 * "Search a place", full screen: matches place names in the downloaded packs' indexes, on
 * this phone. Picking one moves the map there and closes the search; Back closes it too.
 */
@Composable
internal fun PlaceSearchScreen(onPick: (Place) -> Unit, onClose: () -> Unit) {
    val maps = LocalMaps.current
    val focus = LocalFocusManager.current
    val focusRequester = remember { FocusRequester() }
    var query by remember { mutableStateOf("") }
    var results by remember { mutableStateOf<List<Place>>(emptyList()) }
    val installed = maps?.installed?.collectAsStateWithLifecycle()?.value.orEmpty()
    LaunchedEffect(query, installed) { results = if (maps == null || query.isBlank()) emptyList() else maps.search(query) }
    LaunchedEffect(Unit) { focusRequester.requestFocus() }
    BackHandler(onBack = onClose)
    Column(
        Modifier
            .fillMaxSize()
            .background(Pc.Paper)
            // Touches stay here, off the map underneath.
            .pointerInput(Unit) { detectTapGestures { focus.clearFocus() } }
            .statusBarsPadding()
            .imePadding(),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 16.dp, top = 12.dp, bottom = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            MapButton(PIcons.Back, stringResource(R.string.cd_back), onClose)
            PField(
                query, { query = it }, stringResource(R.string.spot_search),
                background = Pc.Card, border = Color.Transparent, height = 48.dp, fontSize = 15,
                placeholder = stringResource(R.string.spot_search),
                shape = RoundedCornerShape(24.dp),
                leading = { Icon(PIcons.Search, null, tint = Pc.Muted, modifier = Modifier.size(18.dp)) },
                trailing = if (query.isEmpty()) null else {
                    { IconAction(PIcons.Close, stringResource(R.string.cd_clear_search), { query = "" }, iconSize = 16.dp) }
                },
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Words, imeAction = ImeAction.Search),
                modifier = Modifier.weight(1f).shadow(2.dp, RoundedCornerShape(24.dp)).focusRequester(focusRequester),
            )
        }
        LazyColumn(
            Modifier.fillMaxWidth().weight(1f),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 16.dp),
        ) {
            val notice = when {
                installed.isEmpty() -> R.string.maps_search_no_maps
                query.isNotBlank() && results.isEmpty() -> R.string.maps_search_none
                else -> null
            }
            if (notice == null && query.isBlank()) {
                // Nothing typed yet: say what the box is for, instead of a blank page.
                item {
                    Column(
                        Modifier.fillMaxWidth().padding(top = 72.dp, start = 24.dp, end = 24.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Box(Modifier.size(64.dp).clip(CircleShape).background(Pc.Chip), contentAlignment = Alignment.Center) {
                            Icon(PIcons.Search, null, tint = Pc.Muted, modifier = Modifier.size(28.dp))
                        }
                        Spacer(Modifier.height(4.dp))
                        Text(stringResource(R.string.maps_search_empty_title), style = body(16.sp, 600), textAlign = TextAlign.Center)
                        Text(
                            stringResource(R.string.maps_search_empty_detail),
                            style = body(14.sp, color = Pc.Muted, lineHeight = 1.4f),
                            textAlign = TextAlign.Center,
                        )
                    }
                }
            }
            if (notice != null) {
                item {
                    Text(
                        stringResource(notice),
                        style = body(14.sp, color = Pc.Muted, lineHeight = 1.4f),
                        modifier = Modifier.padding(horizontal = 8.dp, vertical = 12.dp),
                    )
                }
            }
            itemsIndexed(results) { i, place ->
                if (i > 0) RowDivider()
                val label = stringResource(R.string.cd_map_search_result, place.name)
                TwoLines(
                    place.name,
                    stringResource(
                        when (place.kind) {
                            org.pingers.app.maps.PlaceKind.Country -> R.string.place_country
                            org.pingers.app.maps.PlaceKind.Region -> R.string.place_region
                            org.pingers.app.maps.PlaceKind.City -> R.string.place_city
                            org.pingers.app.maps.PlaceKind.Suburb -> R.string.place_suburb
                            org.pingers.app.maps.PlaceKind.Street -> R.string.place_street
                            org.pingers.app.maps.PlaceKind.Poi -> R.string.place_poi
                        },
                    ),
                    Modifier.fillMaxWidth()
                        .clickable(role = Role.Button) {
                            focus.clearFocus()
                            onPick(place)
                            onClose()
                        }
                        .semantics(mergeDescendants = true) { contentDescription = label }
                        .heightIn(min = 56.dp).padding(horizontal = 8.dp, vertical = 10.dp),
                    titleStyle = body(16.sp, 500),
                    subtitleStyle = body(13.sp, color = Pc.Muted),
                )
            }
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
    Box(Modifier.fillMaxSize().background(Pc.MapLand)) {
        InteractiveMap(
            Scenes.DrawArea,
            frame,
            onTap = repo::drawTapMap,
            overlay = { f ->
                val points = state.points.map(f::toMap)
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
        ) { unit, f ->
            val unitPx = with(density) { unit.toPx() }
            state.midpoints.forEachIndexed { i, c ->
                val m = f.toMap(c)
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
            state.points.map(f::toMap).forEachIndexed { i, p ->
                Vertex(i, p, state, f, unit, unitPx, repo)
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
private fun Vertex(i: Int, p: MapPoint, state: DrawAreaState, frame: MapFrame, unit: Dp, unitPx: Float, repo: PingersRepository) {
    val first = i == 0 && state.closable
    val selected = state.selected == i
    val size = if (first) 30 else 24
    val point by rememberUpdatedState(p)
    val currentFrame by rememberUpdatedState(frame)
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
                    repo.drawDragPoint(i, currentFrame.toCoordinate(MapPoint(point.x + amount.x / unitPx, point.y + amount.y / unitPx)))
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
            MapBox(Scenes.Exclusion, frame = state.mapFrame, overlay = {
                // Circles from their centre and radius, drawn areas through their own vertices.
                state.zones.forEach { z ->
                    when (val shape = z.shape) {
                        is ZoneShape.Circle -> {
                            val at = state.mapFrame.toMap(z.labelAt)
                            hatched(Path().apply { addOval(androidx.compose.ui.geometry.Rect(Offset(at.x, at.y), state.mapFrame.units(shape.radiusM.toDouble()))) })
                        }
                        is ZoneShape.Area -> hatched(polygon(shape.points.map(state.mapFrame::toMap), close = true))
                    }
                }
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
                ListRow({ repo.openZoneTimes(z.id); nav.go(Route.ZoneTimes) }, Modifier.testTag(org.pingers.app.ui.E2eTags.ZONE_ROW)) {
                    val shape = when (val s = z.shape) {
                        is ZoneShape.Circle -> stringResource(R.string.zone_circle, s.radiusM)
                        is ZoneShape.Area -> stringResource(R.string.zone_area)
                    }
                    TwoLines(z.name, listOf(shape, z.schedule ?: stringResource(R.string.zone_all_parties)).joinToString(" · "), Modifier.weight(1f))
                    ChevronIcon()
                }
                RowDivider()
            }
            ListRow({ repo.newZone(); nav.go(Route.SpotEditor) }, padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
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
