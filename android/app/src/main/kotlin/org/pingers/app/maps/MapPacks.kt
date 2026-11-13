package org.pingers.app.maps

import org.json.JSONObject
import org.pingers.app.data.Coordinate

/*
 * The region pack formats pingers-tiles writes and pingers-map-host serves
 * (crates/pingers-map-packs/src/manifest.rs): a catalog of manifests, each naming a PMTiles
 * pack and a place-name index with their sizes and SHA-256s. Packs are downloaded whole,
 * so no request ever says which part of the map is on screen.
 */

/** West, south, east, north, in degrees. */
data class Bbox(val west: Double, val south: Double, val east: Double, val north: Double) {
    operator fun contains(c: Coordinate): Boolean =
        c.latitude in south..north && c.longitude in west..east

    val centre: Coordinate get() = Coordinate((south + north) / 2, (west + east) / 2)

    /** In square degrees: only for preferring the smaller of two packs that cover a point. */
    val size: Double get() = (east - west) * (north - south)
}

data class PackFile(val file: String, val size: Long, val sha256: String, val entries: Long? = null)

/** A region's manifest. */
data class Manifest(
    val id: String,
    val name: String,
    val group: String?,
    val bbox: Bbox,
    val maxZoom: Int,
    /** `YYYY-MM-DD` of the Protomaps build. */
    val buildDate: String,
    val pack: PackFile,
    val index: PackFile,
) {
    /** The file name stem the region's files share (`oceania.australia.victoria`). */
    val stem: String get() = id.replace('/', '.')

    fun toJson(): JSONObject = JSONObject()
        .put("format", 1)
        .put("id", id)
        .put("name", name)
        .apply { if (group != null) put("group", group) }
        .put("bbox", org.json.JSONArray(listOf(bbox.west, bbox.south, bbox.east, bbox.north)))
        .put("max_zoom", maxZoom)
        .put("build_date", buildDate)
        .put("pack", pack.toJson())
        .put("index", index.toJson())

    companion object {
        /** Parses a manifest; null if it is not format 1 or misses a field. */
        fun parse(o: JSONObject): Manifest? = runCatching {
            if (o.optInt("format", 1) != 1) return null
            val b = o.getJSONArray("bbox")
            Manifest(
                id = o.getString("id"),
                name = o.getString("name"),
                group = o.optString("group").takeIf { it.isNotEmpty() },
                bbox = Bbox(b.getDouble(0), b.getDouble(1), b.getDouble(2), b.getDouble(3)),
                maxZoom = o.getInt("max_zoom"),
                buildDate = o.getString("build_date"),
                pack = file(o.getJSONObject("pack")),
                index = file(o.getJSONObject("index")),
            ).takeIf { SAFE_STEM.matches(it.stem) && it.pack.file == it.stem + ".pmtiles" && it.index.file == it.stem + ".places.tsv" }
        }.getOrNull()

        private fun file(o: JSONObject) = PackFile(
            o.getString("file"), o.getLong("size"), o.getString("sha256").lowercase(),
            if (o.has("entries")) o.getLong("entries") else null,
        )

        /** Region ids are lowercase segments; anything else could name a path outside the maps directory. */
        private val SAFE_STEM = Regex("[a-z0-9][a-z0-9-]*(\\.[a-z0-9][a-z0-9-]*)*")
    }
}

private fun PackFile.toJson() = JSONObject().put("file", file).put("size", size).put("sha256", sha256)
    .apply { if (entries != null) put("entries", entries) }

/** One entry of a host's catalog: cut and ready, or (a cache-mode host) cut on first request. */
sealed interface CatalogEntry {
    val id: String
    val name: String
    val group: String?

    data class Ready(val manifest: Manifest) : CatalogEntry {
        override val id get() = manifest.id
        override val name get() = manifest.name
        override val group get() = manifest.group
    }

    data class OnDemand(override val id: String, override val name: String, override val group: String?, val bbox: Bbox?) : CatalogEntry
}

/** Parses `catalog.json`; entries that don't parse are left out. */
fun parseCatalog(text: String): List<CatalogEntry> {
    val regions = JSONObject(text).optJSONArray("regions") ?: return emptyList()
    return (0 until regions.length()).mapNotNull { i ->
        val o = regions.optJSONObject(i) ?: return@mapNotNull null
        if (o.optString("status") == "on_demand") {
            val b = o.optJSONArray("bbox")
            CatalogEntry.OnDemand(
                o.optString("id").takeIf { it.isNotEmpty() } ?: return@mapNotNull null,
                o.optString("name"),
                o.optString("group").takeIf { it.isNotEmpty() },
                b?.let { Bbox(it.getDouble(0), it.getDouble(1), it.getDouble(2), it.getDouble(3)) },
            )
        } else {
            Manifest.parse(o)?.let(CatalogEntry::Ready)
        }
    }
}

/** A map host, and why it is in the chain. */
data class MapHost(val url: String, val source: MapHostSource) {
    /** The host name, for "Maps: <host>". */
    val name: String get() = url.substringAfter("://").substringBefore('/')
}

/** Where a map host came from, highest priority first (the order of the fallback chain). */
enum class MapHostSource { UserOverride, Server, Publisher }

/** What a host offered, or why it offered nothing. */
sealed interface HostCatalog {
    val host: MapHost

    data class Listed(override val host: MapHost, val entries: List<CatalogEntry>) : HostCatalog
    data class Failed(override val host: MapHost) : HostCatalog
}

/** A region on offer, with the host that will serve it. */
data class RegionOffer(val entry: CatalogEntry, val host: MapHost) {
    val id: String get() = entry.id
}

/**
 * The per-region fallback: each region comes from the first host in the chain that lists it,
 * so a host that doesn't carry an area hands only that area to the next host (the user's
 * override, then the server's host, then the publisher's default). [chain] is in priority order.
 */
fun resolveRegions(chain: List<HostCatalog>): List<RegionOffer> {
    val chosen = LinkedHashMap<String, RegionOffer>()
    for (catalog in chain) {
        if (catalog !is HostCatalog.Listed) continue
        for (entry in catalog.entries) {
            val existing = chosen[entry.id]
            // A ready pack beats an on-demand one only from the same priority; otherwise priority wins.
            if (existing == null) chosen[entry.id] = RegionOffer(entry, catalog.host)
        }
    }
    return chosen.values.sortedWith(compareBy({ it.entry.group ?: it.entry.name }, { it.entry.name }))
}

/** The installed packs covering [at], the most detailed (smallest) first. */
fun covering(installed: List<Manifest>, at: Coordinate): List<Manifest> =
    installed.filter { at in it.bbox }.sortedBy { it.bbox.size }
