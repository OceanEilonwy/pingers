package org.pingers.app.ui.map

import android.Manifest
import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.view.Surface
import android.view.WindowManager
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBars
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import kotlin.math.abs
import org.pingers.app.R
import org.pingers.app.data.Coordinate
import org.pingers.app.data.MapFrame
import org.pingers.app.data.MapPoint
import org.pingers.app.platform.hasLocationPermission
import org.pingers.app.platform.lastKnownCoordinate
import org.pingers.app.platform.requestCurrentCoordinate
import org.pingers.app.ui.components.MapButton
import org.pingers.app.ui.components.PIcons
import org.pingers.app.ui.theme.Pc

/*
 * Every map the user can move goes through InteractiveMap, so they all behave the same:
 * drag to pan, pinch to zoom, two fingers to turn, and the same buttons in the same place —
 * the compass ("Orient") and "Go to my location".
 *
 * The screen hands over its own frame (where its origin is, at what scale); the map adds what
 * the user did to it (zoom, bearing and, unless the screen keeps the centre itself, where it
 * was panned to) and hands the resulting frame back to the overlay and content, which must
 * place everything and read every touch through that frame.
 */

/** How the user has moved a map, on top of the screen's own frame. */
@Stable
class MapState(zoom: Float = 1f, bearing: Float = 0f) {
    /** Metres per unit, as a multiple of the screen's frame: 2 shows twice as much. */
    var zoom by mutableFloatStateOf(zoom)

    /** The compass direction pointing up, in degrees clockwise from north. */
    var bearing by mutableFloatStateOf(bearing)

    /** Where the map was panned to, for screens that do not keep the centre themselves. */
    var centre by mutableStateOf<Coordinate?>(null)

    /** The map turns with the phone ("Orient"). */
    var following by mutableStateOf(false)

    /** [frame] as the user has moved it. */
    fun applyTo(frame: MapFrame, ownCentre: Boolean): MapFrame = frame.copy(
        origin = if (ownCentre) frame.origin else centre ?: frame.origin,
        metresPerUnit = frame.metresPerUnit * zoom,
        bearing = bearing,
    )
}

/** A [MapState] that survives recomposition (not process death: a map starts north-up). */
@Composable
fun rememberMapState(zoom: Float = 1f): MapState = remember { MapState(zoom) }

/** Below a full-screen map's top bar: where the controls and the attribution sit. */
@Composable
fun belowTopBar(): Dp = WindowInsets.statusBars.asPaddingValues().calculateTopPadding() + 68.dp

/** Notices for a full-screen map: attribution top left, clear of the controls on the right. */
@Composable
fun fullScreenChrome(promptAt: Float = 0.3f) = MapChrome(
    attribution = Alignment.TopStart,
    attributionPadding = PaddingValues(top = belowTopBar(), start = 12.dp),
    promptAt = promptAt,
)

/**
 * A map the user can move. [frame] is the screen's; [overlay] and [content] get the frame as
 * the user has moved it.
 *
 * With [moveTo], the screen keeps the map's centre (the Spot editor's circle, the meet point
 * pin): panning and "Go to my location" call it with the coordinate that should sit at the
 * frame's origin. Without it, the map keeps its own centre.
 *
 * [onTap] gets the coordinate under a tap that was not a drag. [onFindMe] runs as well as
 * moving the map when "Go to my location" is pressed.
 */
@Composable
fun InteractiveMap(
    scene: MapScene,
    frame: MapFrame,
    modifier: Modifier = Modifier,
    view: MapState = rememberMapState(),
    chrome: MapChrome = fullScreenChrome(),
    controlsPadding: PaddingValues = PaddingValues(top = belowTopBar(), end = 12.dp),
    moveTo: ((Coordinate) -> Unit)? = null,
    onTap: ((Coordinate) -> Unit)? = null,
    onFindMe: () -> Unit = {},
    minZoom: Float = 1f / 8,
    maxZoom: Float = 64f,
    overlay: DrawScope.(MapFrame) -> Unit = {},
    content: @Composable BoxScope.(unit: Dp, frame: MapFrame) -> Unit = { _, _ -> },
) {
    val moved = view.applyTo(frame, ownCentre = moveTo != null)
    val current by rememberUpdatedState(moved)
    val currentMoveTo by rememberUpdatedState(moveTo)
    val currentOnTap by rememberUpdatedState(onTap)
    val density = LocalDensity.current
    val goTo: (Coordinate) -> Unit = { c -> currentMoveTo?.invoke(c) ?: run { view.centre = c } }
    val locate = rememberGoToMyLocation(moved.origin, goTo)
    FollowHeading(view)
    Box(modifier.fillMaxSize()) {
        MapBox(scene, frame = moved, chrome = chrome, fillHeight = true, overlay = { overlay(moved) }) { unit ->
            val unitPx = with(density) { unit.toPx() }
            Box(
                Modifier
                    .fillMaxSize()
                    .pointerInput(unitPx) {
                        detectTransformGestures { _, pan, zoom, rotation ->
                            if (zoom != 1f) view.zoom = (view.zoom / zoom).coerceIn(minZoom, maxZoom)
                            if (rotation != 0f) {
                                view.following = false
                                view.bearing = normalise(view.bearing - rotation)
                            }
                            if (pan != Offset.Zero) {
                                val f = current
                                goTo(f.toCoordinate(MapPoint(f.originAt.x - pan.x / unitPx, f.originAt.y - pan.y / unitPx)))
                            }
                        }
                    }
                    .pointerInput(unitPx) {
                        detectTapGestures { o -> currentOnTap?.invoke(current.toCoordinate(MapPoint(o.x / unitPx, o.y / unitPx))) }
                    },
            )
            content(unit, moved)
        }
        MapControls(view, { onFindMe(); locate() }, Modifier.align(Alignment.TopEnd).padding(controlsPadding))
    }
}

/** The compass ("Orient") and "Go to my location", one under the other. */
@Composable
private fun MapControls(view: MapState, findMe: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val noCompass = stringResource(R.string.map_no_compass)
    val label = stringResource(
        when {
            view.following || view.bearing != 0f -> R.string.cd_map_north_up
            else -> R.string.cd_map_follow_heading
        },
    )
    Column(modifier, verticalArrangement = Arrangement.spacedBy(10.dp)) {
        MapButton(
            PIcons.Compass, label,
            {
                when {
                    view.following || view.bearing != 0f -> {
                        view.following = false
                        view.bearing = 0f
                    }
                    hasCompass(context) -> view.following = true
                    else -> Toast.makeText(context, noCompass, Toast.LENGTH_SHORT).show()
                }
            },
            // The icon's needle points north-east; turned so it points to north on the map.
            Modifier.rotate(-view.bearing - 45f),
            tint = if (view.following) Pc.Blue else Pc.Ink,
        )
        MapButton(PIcons.Locate, stringResource(R.string.cd_my_location), findMe)
    }
}

private fun normalise(degrees: Float): Float = ((degrees % 360f) + 360f) % 360f

private fun hasCompass(context: Context): Boolean =
    (context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager)?.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR) != null

/** While [view] is following, turns it with the phone: the direction the top of the phone faces is up. */
@Composable
private fun FollowHeading(view: MapState) {
    val context = LocalContext.current
    val following = view.following
    DisposableEffect(following) {
        val sensors = context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager
        val sensor = sensors?.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR)
        if (!following || sensors == null || sensor == null) return@DisposableEffect onDispose {}
        val matrix = FloatArray(9)
        val remapped = FloatArray(9)
        val angles = FloatArray(3)
        val listener = object : SensorEventListener {
            override fun onSensorChanged(event: SensorEvent) {
                SensorManager.getRotationMatrixFromVector(matrix, event.values)
                val (x, y) = when (displayRotation(context)) {
                    Surface.ROTATION_90 -> SensorManager.AXIS_Y to SensorManager.AXIS_MINUS_X
                    Surface.ROTATION_180 -> SensorManager.AXIS_MINUS_X to SensorManager.AXIS_MINUS_Y
                    Surface.ROTATION_270 -> SensorManager.AXIS_MINUS_Y to SensorManager.AXIS_X
                    else -> SensorManager.AXIS_X to SensorManager.AXIS_Y
                }
                SensorManager.remapCoordinateSystem(matrix, x, y, remapped)
                SensorManager.getOrientation(remapped, angles)
                val heading = normalise(Math.toDegrees(angles[0].toDouble()).toFloat())
                // Ease towards the new heading the short way round, so the map does not jitter.
                val step = ((heading - view.bearing + 540f) % 360f) - 180f
                if (abs(step) > 0.5f) view.bearing = normalise(view.bearing + step * 0.3f)
            }

            override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit
        }
        sensors.registerListener(listener, sensor, SensorManager.SENSOR_DELAY_UI)
        onDispose { sensors.unregisterListener(listener) }
    }
}

@Suppress("DEPRECATION")
private fun displayRotation(context: Context): Int =
    (context.getSystemService(Context.WINDOW_SERVICE) as? WindowManager)?.defaultDisplay?.rotation ?: Surface.ROTATION_0

/**
 * "Go to my location": moves the map to where the phone last was at once, then to a fresh fix
 * when one comes, unless the map was moved meanwhile. Asks for location permission first if
 * the app has none. The location stays on the phone.
 */
@Composable
fun rememberGoToMyLocation(centre: Coordinate, moveTo: (Coordinate) -> Unit): () -> Unit {
    val context = LocalContext.current
    val currentCentre by rememberUpdatedState(centre)
    val currentMoveTo by rememberUpdatedState(moveTo)
    val notFound = stringResource(R.string.map_location_not_found)
    val denied = stringResource(R.string.map_location_denied)
    val pending = remember { mutableStateOf<(() -> Unit)?>(null) }
    DisposableEffect(Unit) { onDispose { pending.value?.invoke() } }
    val go = {
        pending.value?.invoke()
        val last = lastKnownCoordinate(context)
        last?.let(currentMoveTo)
        pending.value = requestCurrentCoordinate(context) { fresh ->
            pending.value = null
            val untouched = last == null || currentCentre.near(last)
            when {
                fresh != null && untouched -> currentMoveTo(fresh)
                fresh == null && last == null -> Toast.makeText(context, notFound, Toast.LENGTH_SHORT).show()
            }
        }
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { granted ->
        if (granted.values.any { it }) go() else Toast.makeText(context, denied, Toast.LENGTH_SHORT).show()
    }
    return {
        if (hasLocationPermission(context)) {
            go()
        } else {
            permission.launch(arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION))
        }
    }
}

/** Within about a metre: the map has not been moved since it went to this coordinate. */
private fun Coordinate.near(other: Coordinate): Boolean =
    abs(latitude - other.latitude) < 1e-5 && abs(longitude - other.longitude) < 1e-5
