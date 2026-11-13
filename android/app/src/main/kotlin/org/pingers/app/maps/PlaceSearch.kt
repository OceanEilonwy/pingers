package org.pingers.app.maps

import java.io.File
import java.text.Normalizer
import org.pingers.app.data.Coordinate

/*
 * "Search a place", on the phone: each pack ships a place-name index (pingers-map-packs
 * src/index.rs): `key TAB kind TAB latitude TAB longitude TAB name`, sorted by key. The query
 * never leaves the device.
 */

enum class PlaceKind { Country, Region, City, Suburb, Street, Poi }

data class Place(val name: String, val kind: PlaceKind, val at: Coordinate, val key: String)

/**
 * The same normalisation as pingers-map-packs' `search_key`: lowercase, accents folded, every
 * run of other characters one space. Rust folds Latin letters from a table; NFD plus dropping
 * combining marks agrees with it there (ß, æ, œ, ø, ł, đ, þ are folded by hand as Rust does).
 */
fun searchKey(text: String): String {
    val folded = Normalizer.normalize(text.lowercase(), Normalizer.Form.NFD)
        .replace(COMBINING, "")
        .replace("ß", "ss").replace("æ", "ae").replace("œ", "oe").replace("ø", "o")
        .replace("ł", "l").replace("đ", "d").replace("þ", "th").replace("ħ", "h").replace("ı", "i")
    val out = StringBuilder(folded.length)
    var space = false
    var i = 0
    while (i < folded.length) {
        val cp = folded.codePointAt(i)
        if (Character.isLetterOrDigit(cp)) {
            if (space && out.isNotEmpty()) out.append(' ')
            space = false
            out.appendCodePoint(cp)
        } else {
            space = true
        }
        i += Character.charCount(cp)
    }
    return out.toString()
}

private val COMBINING = Regex("\\p{Mn}+")

/** One pack's index, in memory. */
class PlaceIndex(val places: List<Place>) {
    /**
     * Places matching [query]: every word of the query must begin a word of the name. Whole-name
     * prefixes come first, then coarser kinds (a suburb before a café of the same name), then
     * shorter names.
     */
    fun search(query: String, limit: Int = 20): List<Place> {
        val q = searchKey(query)
        if (q.isEmpty()) return emptyList()
        val words = q.split(' ')
        return places.asSequence()
            .filter { p -> words.all { w -> p.key.startsWith(w) || p.key.contains(" $w") } }
            .sortedWith(compareBy({ if (it.key.startsWith(q)) 0 else 1 }, { it.kind.ordinal }, { it.name.length }))
            .take(limit)
            .toList()
    }

    companion object {
        const val HEADER = "# pingers-places 1"

        fun parse(lines: Sequence<String>): PlaceIndex {
            val places = ArrayList<Place>()
            for (line in lines) {
                if (line.startsWith("#") || line.isBlank()) continue
                val f = line.split('\t', limit = 5)
                if (f.size != 5) continue
                val kind = when (f[1]) {
                    "country" -> PlaceKind.Country
                    "region" -> PlaceKind.Region
                    "city" -> PlaceKind.City
                    "suburb" -> PlaceKind.Suburb
                    "street" -> PlaceKind.Street
                    "poi" -> PlaceKind.Poi
                    else -> continue
                }
                val lat = f[2].toDoubleOrNull() ?: continue
                val lon = f[3].toDoubleOrNull() ?: continue
                places += Place(f[4], kind, Coordinate(lat, lon), f[0])
            }
            return PlaceIndex(places)
        }

        fun load(file: File): PlaceIndex = file.bufferedReader().useLines { parse(it) }
    }
}

/** Searches several packs' indexes, nearest to [near] first among equals. */
fun searchAll(indexes: List<PlaceIndex>, query: String, limit: Int = 20): List<Place> =
    indexes.flatMap { it.search(query, limit) }
        .distinctBy { Triple(it.key, it.kind, (it.at.latitude * 1000).toInt() to (it.at.longitude * 1000).toInt()) }
        .take(limit)
