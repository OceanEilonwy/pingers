package org.pingers.app.maps

import android.content.Context
import android.content.SharedPreferences
import androidx.core.content.edit
import java.io.File
import java.io.IOException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import org.pingers.app.BuildConfig
import org.pingers.app.data.Coordinate
import org.pingers.app.data.CoreHost

/*
 * Region packs on this phone: which hosts to use, what they offer, what is installed, and
 * downloads. Everything a map shows comes from these files; the app never asks a host for
 * the part of the map on screen, and place search reads the installed indexes.
 */

/** The user's map settings (Advanced). */
data class MapSettings(
    /** A map host the user chose; null to use the server's or the publisher's. */
    val hostOverride: String?,
    /** "Download maps through privacy relay". */
    val throughRelay: Boolean,
)

/** Where a region's download is. */
sealed interface DownloadState {
    data class Running(val done: Long, val total: Long) : DownloadState
    /** A cache-mode host is cutting the region; the app retries after [seconds]. */
    data class Preparing(val seconds: Int) : DownloadState
    data class Failed(val reason: Failure) : DownloadState
}

enum class Failure { Network, Checksum, RelayUnavailable, NotOffered }

/** The region picker's list. */
sealed interface CatalogState {
    data object Idle : CatalogState
    data object Loading : CatalogState
    data class Loaded(val offers: List<RegionOffer>, val failedHosts: List<MapHost>, val relayUnavailable: Boolean = false) : CatalogState
}

/**
 * The map host the user's Pingers server advertises (`map_host` in `/v1/params`), as the core
 * last read it ([CoreHost.serverMapHost]). Null until then: the chain goes from the user's
 * override straight to the publisher's default.
 */
fun interface ServerMapHost {
    fun url(): String?
}

class Maps(context: Context, private val serverMapHost: ServerMapHost = ServerMapHost { null }) {
    private val directory = File(context.filesDir, "maps").apply { mkdirs() }
    private val prefs: SharedPreferences = context.getSharedPreferences("maps", Context.MODE_PRIVATE)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _settings = MutableStateFlow(
        MapSettings(prefs.getString(KEY_HOST, null)?.takeIf { it.isNotBlank() }, prefs.getBoolean(KEY_RELAY, false)),
    )
    val settings: StateFlow<MapSettings> = _settings.asStateFlow()

    private val _installed = MutableStateFlow(readInstalled())
    val installed: StateFlow<List<Manifest>> = _installed.asStateFlow()

    private val _catalog = MutableStateFlow<CatalogState>(CatalogState.Idle)
    val catalog: StateFlow<CatalogState> = _catalog.asStateFlow()

    private val _downloads = MutableStateFlow<Map<String, DownloadState>>(emptyMap())
    val downloads: StateFlow<Map<String, DownloadState>> = _downloads.asStateFlow()

    private val jobs = HashMap<String, Job>()
    private val indexes = HashMap<String, PlaceIndex>()

    /** The fallback chain, highest priority first, without repeats. */
    fun chain(): List<MapHost> = listOfNotNull(
        _settings.value.hostOverride?.let { MapHost(it.trimEnd('/'), MapHostSource.UserOverride) },
        serverMapHost.url()?.let { MapHost(it.trimEnd('/'), MapHostSource.Server) },
        MapHost(BuildConfig.MAP_HOST.trimEnd('/'), MapHostSource.Publisher),
    ).distinctBy { it.url }

    /** The host maps come from first. */
    fun primaryHost(): MapHost = chain().first()

    fun setHostOverride(url: String?) {
        val clean = url?.trim()?.takeIf { it.isNotEmpty() }
        prefs.edit { putString(KEY_HOST, clean) }
        _settings.update { it.copy(hostOverride = clean) }
        _catalog.value = CatalogState.Idle
    }

    fun setThroughRelay(on: Boolean) {
        prefs.edit { putBoolean(KEY_RELAY, on) }
        _settings.update { it.copy(throughRelay = on) }
    }

    private fun fetcher(): Fetcher = if (_settings.value.throughRelay) RelayFetcher else DirectFetcher

    /** Fetches each host's catalog in the chain. Catalogs say nothing about where the user is. */
    fun refreshCatalog() {
        _catalog.value = CatalogState.Loading
        scope.launch {
            val fetcher = fetcher()
            if (fetcher is RelayFetcher) {
                // Nothing goes direct while the relay setting is on, catalogs included.
                _catalog.value = CatalogState.Loaded(emptyList(), emptyList(), relayUnavailable = true)
                return@launch
            }
            val catalogs = chain().map { host ->
                try {
                    HostCatalog.Listed(host, parseCatalog(fetchText(fetcher, "${host.url}/catalog.json")))
                } catch (e: IOException) {
                    HostCatalog.Failed(host)
                } catch (e: org.json.JSONException) {
                    HostCatalog.Failed(host)
                }
            }
            _catalog.value = CatalogState.Loaded(resolveRegions(catalogs), catalogs.filterIsInstance<HostCatalog.Failed>().map { it.host })
        }
    }

    /** Downloads (or updates) a region from the host that offers it. */
    fun download(offer: RegionOffer) {
        if (jobs[offer.id]?.isActive == true) return
        jobs[offer.id] = scope.launch {
            val fetcher = fetcher()
            var attempt = 0
            while (true) {
                try {
                    val manifest = when (val entry = offer.entry) {
                        is CatalogEntry.Ready -> entry.manifest
                        is CatalogEntry.OnDemand -> Manifest.parse(JSONObject(fetchText(fetcher, "${offer.host.url}/${entry.id.replace('/', '.')}.json")))
                            ?: throw HttpFailure(404)
                    }
                    install(fetcher, offer.host, manifest)
                    _downloads.update { it - offer.id }
                    return@launch
                } catch (e: Preparing) {
                    // A cache-mode host cuts the region on first request; wait as it asks, a bounded number of times.
                    if (++attempt > 40) {
                        fail(offer.id, Failure.Network)
                        return@launch
                    }
                    _downloads.update { it + (offer.id to DownloadState.Preparing(e.seconds)) }
                    delay(e.seconds.coerceIn(1, 120) * 1000L)
                } catch (e: RelayUnavailable) {
                    fail(offer.id, Failure.RelayUnavailable)
                    return@launch
                } catch (e: ChecksumMismatch) {
                    fail(offer.id, Failure.Checksum)
                    return@launch
                } catch (e: HttpFailure) {
                    fail(offer.id, if (e.status == 404) Failure.NotOffered else Failure.Network)
                    return@launch
                } catch (e: IOException) {
                    fail(offer.id, Failure.Network)
                    return@launch
                } catch (e: org.json.JSONException) {
                    fail(offer.id, Failure.Network)
                    return@launch
                }
            }
        }
    }

    private fun fail(id: String, reason: Failure) = _downloads.update { it + (id to DownloadState.Failed(reason)) }

    private fun install(fetcher: Fetcher, host: MapHost, manifest: Manifest) {
        val total = manifest.index.size + manifest.pack.size
        val report = { done: Long -> _downloads.update { it + (manifest.id to DownloadState.Running(done, total)) } }
        downloadFile(fetcher, "${host.url}/${manifest.index.file}", File(directory, manifest.index.file), manifest.index.size, manifest.index.sha256, report)
        downloadFile(fetcher, "${host.url}/${manifest.pack.file}", File(directory, manifest.pack.file), manifest.pack.size, manifest.pack.sha256) {
            report(manifest.index.size + it)
        }
        // The manifest goes last: a region is installed exactly when its manifest is on disk.
        val json = File(directory, manifest.stem + ".json")
        val partial = File(json.path + ".partial")
        partial.writeText(manifest.toJson().toString())
        partial.renameTo(json)
        synchronized(indexes) { indexes.remove(manifest.id) }
        _installed.value = readInstalled()
    }

    fun delete(id: String) {
        jobs.remove(id)?.cancel()
        val stem = id.replace('/', '.')
        File(directory, "$stem.json").delete()
        listOf(".pmtiles", ".places.tsv", ".pmtiles.partial", ".places.tsv.partial").forEach { File(directory, stem + it).delete() }
        synchronized(indexes) { indexes.remove(id) }
        _downloads.update { it - id }
        _installed.value = readInstalled()
    }

    private fun readInstalled(): List<Manifest> =
        directory.listFiles { f -> f.name.endsWith(".json") }.orEmpty().mapNotNull { file ->
            runCatching { Manifest.parse(JSONObject(file.readText())) }.getOrNull()
                ?.takeIf { File(directory, it.pack.file).length() == it.pack.size }
        }

    /** The installed pack to draw [at] with, if any. */
    fun packFor(at: Coordinate): Manifest? = covering(_installed.value, at).firstOrNull()

    /** MapLibre's URL for a pack on this phone. */
    fun packUrl(manifest: Manifest): String = "pmtiles://file://" + File(directory, manifest.pack.file).absolutePath

    /** Searches the installed packs' place names. Runs on this phone only. */
    suspend fun search(query: String): List<Place> = withContext(Dispatchers.IO) {
        val loaded = _installed.value.mapNotNull { m ->
            synchronized(indexes) { indexes[m.id] } ?: runCatching { PlaceIndex.load(File(directory, m.index.file)) }.getOrNull()?.also {
                synchronized(indexes) { indexes[m.id] = it }
            }
        }
        searchAll(loaded, query)
    }

    companion object {
        private const val KEY_HOST = "host_override"
        private const val KEY_RELAY = "through_relay"

        @Volatile private var instance: Maps? = null

        fun get(context: Context): Maps = instance ?: synchronized(this) {
            instance ?: Maps(context.applicationContext, ServerMapHost { CoreHost.serverMapHost() }).also { instance = it }
        }
    }
}

/** Whether [installed] is older than what the host offers: updates are offered, never automatic. */
fun updateAvailable(installed: Manifest, offer: RegionOffer): Boolean =
    (offer.entry as? CatalogEntry.Ready)?.manifest?.let { it.buildDate > installed.buildDate || (it.buildDate == installed.buildDate && it.pack.sha256 != installed.pack.sha256) } ?: false
