package org.pingers.app.ui.map

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.ln
import org.maplibre.android.MapLibre
import org.maplibre.android.camera.CameraPosition
import org.maplibre.android.camera.CameraUpdateFactory
import org.maplibre.android.geometry.LatLng
import org.maplibre.android.maps.MapLibreMap
import org.maplibre.android.maps.MapLibreMapOptions
import org.maplibre.android.maps.MapView
import org.maplibre.android.maps.Style
import org.pingers.app.data.Coordinate
import org.pingers.app.data.MapFrame
import org.pingers.app.data.MapPoint
import org.pingers.app.maps.Maps

/*
 * The real map: MapLibre Native drawing one installed region pack (PMTiles, through the
 * `pmtiles://file://` source) with the Protomaps basemap style, glyphs and sprites bundled in
 * assets/maps. Nothing is fetched from the network: the style names only local files.
 *
 * Screens keep drawing their overlays in map units through a MapFrame; the camera is worked
 * out from the same frame (where the scene's centre is and how many metres a unit is), so a
 * Spot circle drawn in units lies exactly over the streets MapLibre draws.
 */

/** The maps on this phone, or null in previews. */
val LocalMaps = staticCompositionLocalOf<Maps?> { null }

/** Opens the region picker for the area around a coordinate ("Download map for this area"). */
val LocalOpenMapRegions = staticCompositionLocalOf<(Coordinate?) -> Unit> { {} }

/** Where MapLibre looks, how close, and which compass direction is up. */
data class Camera(val target: Coordinate, val zoom: Double, val bearing: Double = 0.0)

/**
 * The camera that puts [frame]'s units on screen at [unitDp] dp each, with the scene's centre
 * (shifted by [pan] units, as MapBox shifts its overlays) in the middle of the view.
 *
 * MapLibre's zoom z shows the world 512 dp wide, so at latitude φ a dp is
 * C·cos φ / (512·2^z) metres, C being the equator's length.
 */
fun cameraFor(frame: MapFrame, sceneWidth: Float, sceneHeight: Float, unitDp: Float, pan: MapPoint = MapPoint(0f, 0f)): Camera {
    val target = frame.toCoordinate(MapPoint(sceneWidth / 2 - pan.x, sceneHeight / 2 - pan.y))
    val metresPerDp = frame.metresPerUnit / unitDp
    val zoom = ln(EQUATOR_M * cos(target.latitude * PI / 180) / (512 * metresPerDp)) / ln(2.0)
    return Camera(target, zoom, frame.bearing.toDouble())
}

/** The equator's length as MapFrame measures it (111,320 m a degree). */
private const val EQUATOR_M = 111_320.0 * 360

private object StyleTemplate {
    @Volatile private var json: String? = null

    fun load(context: Context): String = json ?: context.assets.open("maps/style.json").bufferedReader().use { it.readText() }.also { json = it }
}

/**
 * MapLibre showing the pack at [packUrl] from [camera]. Gestures are off: the screen above
 * handles touches in map units and moves the camera through its frame.
 */
@Composable
fun PackMap(packUrl: String, camera: Camera, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val holder = remember { MapHolder() }
    val view = remember {
        MapLibre.getInstance(context.applicationContext)
        val options = MapLibreMapOptions.createFromAttributes(context)
            .textureMode(true)
            .attributionEnabled(false)
            .logoEnabled(false)
            .compassEnabled(false)
        MapView(context, options).apply {
            onCreate(null)
            getMapAsync { map ->
                map.uiSettings.setAllGesturesEnabled(false)
                holder.map = map
                holder.apply(context)
            }
        }
    }
    holder.wanted = packUrl to camera
    holder.apply(context)
    DisposableEffect(lifecycle, view) {
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_START -> view.onStart()
                Lifecycle.Event.ON_RESUME -> view.onResume()
                Lifecycle.Event.ON_PAUSE -> view.onPause()
                Lifecycle.Event.ON_STOP -> view.onStop()
                else -> {}
            }
        }
        lifecycle.addObserver(observer)
        onDispose {
            lifecycle.removeObserver(observer)
            if (lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) view.onPause()
            if (lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) view.onStop()
            view.onDestroy()
        }
    }
    AndroidView({ view }, modifier)
}

/** The map once MapLibre hands it over, and what it should show. */
private class MapHolder {
    var map: MapLibreMap? = null
    var wanted: Pair<String, Camera>? = null
    private var styled: String? = null

    fun apply(context: Context) {
        val map = map ?: return
        val (url, camera) = wanted ?: return
        if (styled != url) {
            styled = url
            map.setStyle(Style.Builder().fromJson(StyleTemplate.load(context).replace("{{PACK_URL}}", url)))
        }
        map.moveCamera(
            CameraUpdateFactory.newCameraPosition(
                CameraPosition.Builder().target(LatLng(camera.target.latitude, camera.target.longitude)).zoom(camera.zoom).bearing(camera.bearing).build(),
            ),
        )
    }
}
