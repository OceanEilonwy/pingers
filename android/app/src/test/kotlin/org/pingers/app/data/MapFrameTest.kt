package org.pingers.app.data

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.pingers.app.maps.Manifest
import org.pingers.app.maps.covering
import org.pingers.app.maps.startingPoint
import org.pingers.app.ui.map.cameraFor

/**
 * Which pack a map draws is decided at its frame's centre (MapBox). A screen with nothing of
 * its own to centre on (no fix since the app started, no Spot, no meet point) used to centre
 * on 0,0, which no pack covers: Western Australia downloaded and installed, and every map
 * still said "Download map for this area", and a Spot saved there landed on 0,0.
 */
class MapFrameTest {
    /** Western Australia exactly as the cache-mode map host cut it (pingers #5). */
    private val westernAustralia = Manifest.of(
        uniffi.pingers.MapManifest(
            id = "oceania/australia/western-australia", name = "Western Australia", group = "Australia",
            west = 112.0, south = -36.0, east = 129.3, north = -13.0,
            maxZoom = 15u, buildDate = "2026-08-11",
            pack = uniffi.pingers.MapFile(
                "oceania.australia.western-australia.pmtiles", 358_586_175u,
                "b881a1572b35c3bdd2c0b1ba3e0597bac8f53c76c1907a0def17f114f8fd41e6", null,
            ),
            index = uniffi.pingers.MapFile(
                "oceania.australia.western-australia.places.tsv", 12_175_741u,
                "3bc1cc1f3a05c156135ea5b8db011bac3edcb0b3d4b67bd8130c9a4385335892", 199_349u,
            ),
        ),
    )
    private val act = westernAustralia.copy(
        id = "oceania/australia/australian-capital-territory",
        bbox = org.pingers.app.maps.Bbox(148.7, -36.0, 149.5, -35.05),
    )
    private val perth = Coordinate(-31.95, 115.86)

    @After
    fun reset() {
        FrameStart.centre = { null }
    }

    /** The pack the Spot editor's map draws for [frame], as MapBox picks it. */
    private fun drawn(frame: MapFrame, installed: List<Manifest>): Manifest? =
        covering(installed, cameraFor(frame, 390f, 640f, unitDp = 1f).target).firstOrNull()

    @Test
    fun perthIsInWesternAustralia() {
        assertEquals(listOf(westernAustralia), covering(listOf(act, westernAustralia), perth))
        assertEquals(westernAustralia, drawn(frameAt(perth, MapPoint(195f, 300f)), listOf(westernAustralia)))
    }

    @Test
    fun aMapWithNowhereToBeStartsWhereThePhoneLastWas() {
        val installed = listOf(act, westernAustralia)
        FrameStart.centre = { startingPoint(perth, installed) }
        val frame = frameAt(null, MapPoint(195f, 300f))
        assertFalse(frame.placed)
        assertEquals(perth, frame.origin)
        assertEquals(westernAustralia, drawn(frame, installed))
    }

    @Test
    fun withoutAFixItStartsOnTheDownloadedMap() {
        val installed = listOf(westernAustralia)
        FrameStart.centre = { startingPoint(null, installed) }
        val frame = frameAt(null, MapPoint(195f, 300f))
        assertFalse(frame.placed)
        assertEquals(westernAustralia, drawn(frame, installed))
        // Of two packs, the more detailed.
        FrameStart.centre = { startingPoint(null, listOf(westernAustralia, act)) }
        assertEquals(act, drawn(frameAt(null), listOf(westernAustralia, act)))
    }

    @Test
    fun onlyWithNoFixAndNoMapIsItZeroZero() {
        assertNull(startingPoint(null, emptyList()))
        val frame = frameAt(null)
        assertEquals(Coordinate(0.0, 0.0), frame.origin)
        assertFalse(frame.placed)
        // What every unplaced map did before: no pack covers 0,0.
        assertNull(drawn(frame, listOf(westernAustralia)))
        assertTrue(frameAt(perth).placed)
    }

    /** A turned map (two-finger rotate, "Orient") turns overlays, taps and the camera together. */
    @Test
    fun aTurnedFrameTurnsAboutItsOrigin() {
        val north = MapFrame(perth, MapPoint(100f, 100f), metresPerUnit = 2f)
        val east = north.copy(bearing = 90f)
        // 200 m east of the origin: to the right when north is up, straight up when east is up.
        val there = north.toCoordinate(MapPoint(200f, 100f))
        val up = east.toMap(there)
        assertEquals(100f, up.x, 0.01f)
        assertEquals(0f, up.y, 0.01f)
        // A tap goes back to the same place, whichever way the map faces.
        for (bearing in listOf(0f, 37f, 90f, 200f, 359f)) {
            val f = north.copy(bearing = bearing)
            val back = f.toCoordinate(f.toMap(there))
            assertEquals(there.latitude, back.latitude, 1e-9)
            assertEquals(there.longitude, back.longitude, 1e-9)
        }
        assertEquals(perth, east.toCoordinate(MapPoint(100f, 100f)))
        assertEquals(90.0, cameraFor(east, 200f, 200f, unitDp = 1f).bearing, 0.0)
    }
}
