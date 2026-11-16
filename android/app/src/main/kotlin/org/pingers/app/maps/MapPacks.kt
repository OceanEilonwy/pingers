package org.pingers.app.maps

import org.pingers.app.data.Coordinate

/*
 * The region packs the core downloads (crates/pingers-map-downloads, through pingers-ffi): a
 * catalog of regions from the map host chain, each a PMTiles pack and a place-name index with
 * their sizes and SHA-256s. Packs are downloaded whole, so no request ever says which part of
 * the map is on screen. The shell only draws them and searches their indexes.
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

/** A region's manifest, as the core checked it. */
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
    companion object {
        fun of(m: uniffi.pingers.MapManifest) = Manifest(
            id = m.id,
            name = m.name,
            group = m.group,
            bbox = Bbox(m.west, m.south, m.east, m.north),
            maxZoom = m.maxZoom.toInt(),
            buildDate = m.buildDate,
            pack = m.pack.toShell(),
            index = m.index.toShell(),
        )

        private fun uniffi.pingers.MapFile.toShell() = PackFile(file, size.toLong(), sha256, entries?.toLong())
    }
}

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

    data class OnDemand(override val id: String, override val name: String, override val group: String?) : CatalogEntry
}

/** A map host, and why it is in the chain. */
data class MapHost(val url: String, val source: MapHostSource) {
    /** The host name, for "Maps: <host>". */
    val name: String get() = url.substringAfter("://").substringBefore('/')

    companion object {
        fun of(h: uniffi.pingers.MapHostInfo) = MapHost(
            h.url,
            when (h.source) {
                uniffi.pingers.MapHostSource.USER_OVERRIDE -> MapHostSource.UserOverride
                uniffi.pingers.MapHostSource.SERVER -> MapHostSource.Server
                uniffi.pingers.MapHostSource.PUBLISHER -> MapHostSource.Publisher
            },
        )
    }
}

/** Where a map host came from, highest priority first (the order of the fallback chain). */
enum class MapHostSource { UserOverride, Server, Publisher }

/** A region on offer, with the host that will serve it; [offer] goes back to the core to download it. */
data class RegionOffer(val entry: CatalogEntry, val host: MapHost, val offer: uniffi.pingers.MapOffer) {
    val id: String get() = entry.id

    companion object {
        fun of(o: uniffi.pingers.MapOffer) = RegionOffer(
            when (val e = o.entry) {
                is uniffi.pingers.MapEntry.Ready -> CatalogEntry.Ready(Manifest.of(e.manifest))
                is uniffi.pingers.MapEntry.OnDemand -> CatalogEntry.OnDemand(e.id, e.name, e.group)
            },
            MapHost.of(o.host),
            o,
        )
    }
}

/** Why a host in the chain offered nothing. */
enum class HostProblem { Unreachable, NotThroughRelay, NoRelayRoute, KeyChanged }

/** A host that offered nothing, and why. */
data class FailedHost(val host: MapHost, val problem: HostProblem) {
    companion object {
        fun of(f: uniffi.pingers.MapHostFailure) = FailedHost(
            MapHost.of(f.host),
            when (f.problem) {
                uniffi.pingers.MapHostProblem.UNREACHABLE -> HostProblem.Unreachable
                uniffi.pingers.MapHostProblem.NOT_THROUGH_RELAY -> HostProblem.NotThroughRelay
                uniffi.pingers.MapHostProblem.NO_RELAY_ROUTE -> HostProblem.NoRelayRoute
                uniffi.pingers.MapHostProblem.KEY_CHANGED -> HostProblem.KeyChanged
            },
        )
    }
}

/** The installed packs covering [at], the most detailed (smallest) first. */
fun covering(installed: List<Manifest>, at: Coordinate): List<Manifest> =
    installed.filter { at in it.bbox }.sortedBy { it.bbox.size }
