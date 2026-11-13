package org.pingers.app.maps

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.pingers.app.data.Coordinate
import org.pingers.app.data.MapFrame
import org.pingers.app.data.MapPoint
import org.pingers.app.ui.map.cameraFor

class MapsTest {
    private fun manifest(id: String, buildDate: String = "2026-11-12", bbox: Bbox = Bbox(144.95, -37.825, 144.98, -37.805)) = Manifest(
        id = id, name = id.substringAfterLast('/'), group = null, bbox = bbox, maxZoom = 15, buildDate = buildDate,
        pack = PackFile(id.replace('/', '.') + ".pmtiles", 10, "aa"),
        index = PackFile(id.replace('/', '.') + ".places.tsv", 5, "bb", 3),
    )

    private val override = MapHost("https://maps.mine.example", MapHostSource.UserOverride)
    private val server = MapHost("https://maps.flat7.org", MapHostSource.Server)
    private val publisher = MapHost("https://maps.pingers.example", MapHostSource.Publisher)

    @Test
    fun eachRegionComesFromTheFirstHostThatHasIt() {
        val offers = resolveRegions(
            listOf(
                HostCatalog.Listed(override, listOf(CatalogEntry.Ready(manifest("oceania/australia/victoria")))),
                HostCatalog.Failed(server),
                HostCatalog.Listed(
                    publisher,
                    listOf(
                        CatalogEntry.Ready(manifest("oceania/australia/victoria", "2026-11-13")),
                        CatalogEntry.Ready(manifest("europe/germany/berlin")),
                    ),
                ),
            ),
        ).associate { it.id to it.host }
        // The override carries Victoria, so the publisher never serves it, even with newer data...
        assertEquals(override, offers["oceania/australia/victoria"])
        // ...but Berlin, which the override lacks, falls back to the publisher for that region only.
        assertEquals(publisher, offers["europe/germany/berlin"])
    }

    @Test
    fun theServersHostComesBeforeThePublishers() {
        val offers = resolveRegions(
            listOf(
                HostCatalog.Listed(server, listOf(CatalogEntry.OnDemand("europe/germany/berlin", "Berlin", "Germany", null))),
                HostCatalog.Listed(publisher, listOf(CatalogEntry.Ready(manifest("europe/germany/berlin")))),
            ),
        )
        assertEquals(listOf(server), offers.map { it.host })
        assertTrue(offers.single().entry is CatalogEntry.OnDemand)
    }

    @Test
    fun updatesAreOfferedForNewerBuildsOnly() {
        val installed = manifest("oceania/australia/victoria")
        assertTrue(updateAvailable(installed, RegionOffer(CatalogEntry.Ready(manifest("oceania/australia/victoria", "2026-11-20")), publisher)))
        assertFalse(updateAvailable(installed, RegionOffer(CatalogEntry.Ready(installed), publisher)))
    }

    @Test
    fun theSmallestCoveringPackDraws() {
        val city = manifest("oceania/australia/victoria/melbourne-cbd")
        val state = manifest("oceania/australia/victoria", bbox = Bbox(140.9, -39.2, 150.0, -33.9))
        val at = Coordinate(-37.8136, 144.9631)
        assertEquals(listOf(city, state), covering(listOf(state, city), at))
        assertEquals(emptyList<Manifest>(), covering(listOf(city), Coordinate(51.5, -0.1)))
    }

    @Test
    fun searchKeysMatchTheRustNormalisation() {
        // The same cases as pingers-map-packs' keys_fold_case_accents_and_punctuation.
        assertEquals("flinders street station", searchKey("Flinders Street Station"))
        assertEquals("cafe eclair patisserie", searchKey("  Café   Éclair—Pâtisserie "))
        assertEquals("strasse", searchKey("Straße"))
        assertEquals("st kilda rd", searchKey("St. Kilda Rd"))
        assertEquals("東京", searchKey("東京"))
        assertEquals("", searchKey("--"))
    }

    @Test
    fun placeSearchMatchesWordPrefixesCoarsestFirst() {
        val index = PlaceIndex.parse(
            sequenceOf(
                PlaceIndex.HEADER,
                "flinders lane\tstreet\t-37.81838\t144.96038\tFlinders Lane",
                "flinders street station\tpoi\t-37.81830\t144.96720\tFlinders Street Station",
                "southbank\tsuburb\t-37.82250\t144.96430\tSouthbank",
                "the flinders\tpoi\t-37.8\t144.9\tThe Flinders",
                "broken line",
            ),
        )
        assertEquals(4, index.places.size)
        assertEquals(listOf("Flinders Lane", "Flinders Street Station", "The Flinders"), index.search("flind").map { it.name })
        assertEquals(listOf("Flinders Street Station"), index.search("Flinders st").map { it.name })
        assertEquals(listOf("Southbank"), index.search("SOUTH").map { it.name })
        assertTrue(index.search("  ").isEmpty())
    }

    @Test
    fun theCameraMatchesTheFramesScale() {
        // 2 m a unit, 390 units across a 390 dp wide view: 2 m a dp. At the equator, zoom z
        // shows 40,075 km in 512·2^z dp, so 2 m a dp is z = log2(40,075,200 / 1024) ≈ 15.256.
        val frame = MapFrame(Coordinate(0.0, 0.0), MapPoint(195f, 256f), 2f)
        val camera = cameraFor(frame, 390f, 512f, unitDp = 1f)
        assertEquals(0.0, camera.target.latitude, 1e-9)
        assertEquals(0.0, camera.target.longitude, 1e-9)
        assertEquals(15.256, camera.zoom, 1e-3)
        // Panning the overlays right by 10 units moves the camera 20 m west.
        val panned = cameraFor(frame, 390f, 512f, unitDp = 1f, pan = MapPoint(10f, 0f))
        assertEquals(-20.0 / 111_320.0, panned.target.longitude, 1e-9)
    }
}
