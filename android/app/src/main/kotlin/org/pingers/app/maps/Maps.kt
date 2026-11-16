package org.pingers.app.maps

import android.content.Context
import android.content.SharedPreferences
import androidx.core.content.edit
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.pingers.app.BuildConfig
import org.pingers.app.data.Coordinate
import org.pingers.app.data.CoreHost
import uniffi.pingers.Core
import uniffi.pingers.CoreException
import uniffi.pingers.MapDownloadListener
import uniffi.pingers.MapDownloadRequest
import uniffi.pingers.MapFailure

/*
 * Region packs on this phone, and the user's map settings. The Rust core fetches catalogs and
 * downloads packs (design E.1: the shell makes no network calls), checks them against their
 * manifests and installs them in the app's maps directory; this class asks it to, follows
 * progress, and gives the map screens the installed packs to draw. Place search reads the
 * installed indexes on the phone.
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
    /** A cache-mode host is cutting the region; the core asks again after [seconds]. */
    data class Preparing(val seconds: Int) : DownloadState
    data class Failed(val reason: Failure) : DownloadState
}

enum class Failure { Network, Checksum, NotOffered, Preparing, NotThroughRelay, NoRelayRoute, KeyChanged, Storage, NoCore }

/** The region picker's list. */
sealed interface CatalogState {
    data object Idle : CatalogState
    data object Loading : CatalogState
    data class Loaded(val offers: List<RegionOffer>, val failedHosts: List<FailedHost>) : CatalogState
    /** The core did not open, so nothing can be fetched. */
    data object NoCore : CatalogState
}

/**
 * The map host the user's Pingers server advertises (`map_host` in `/v1/params`), as the core
 * last read it ([CoreHost.serverMapHost]), for the words about which host is in use. The core
 * adds it to the chain itself.
 */
fun interface ServerMapHost {
    fun url(): String?
}

class Maps(
    context: Context,
    private val serverMapHost: ServerMapHost = ServerMapHost { null },
    private val core: suspend () -> Core? = { CoreHost.core() },
) {
    private val directory = File(context.filesDir, "maps").apply { mkdirs() }
    private val prefs: SharedPreferences = context.getSharedPreferences("maps", Context.MODE_PRIVATE)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _settings = MutableStateFlow(
        MapSettings(prefs.getString(KEY_HOST, null)?.takeIf { it.isNotBlank() }, prefs.getBoolean(KEY_RELAY, false)),
    )
    val settings: StateFlow<MapSettings> = _settings.asStateFlow()

    private val _installed = MutableStateFlow<List<Manifest>>(emptyList())
    val installed: StateFlow<List<Manifest>> = _installed.asStateFlow()

    private val _catalog = MutableStateFlow<CatalogState>(CatalogState.Idle)
    val catalog: StateFlow<CatalogState> = _catalog.asStateFlow()

    private val _downloads = MutableStateFlow<Map<String, DownloadState>>(emptyMap())
    val downloads: StateFlow<Map<String, DownloadState>> = _downloads.asStateFlow()

    private val jobs = HashMap<String, Job>()
    private val indexes = HashMap<String, PlaceIndex>()

    init {
        scope.launch { reloadInstalled() }
    }

    /** The fallback chain, highest priority first, without repeats: for the words only. */
    fun chain(): List<MapHost> = listOfNotNull(
        _settings.value.hostOverride?.let { MapHost(it.trimEnd('/'), MapHostSource.UserOverride) },
        serverMapHost.url()?.let { MapHost(it.trimEnd('/'), MapHostSource.Server) },
        MapHost(BuildConfig.MAP_HOST.trimEnd('/'), MapHostSource.Publisher),
    ).distinctBy { it.url }

    /**
     * The host maps come from first: as the core's catalog named it once loaded (the core knows
     * the server's map host first-hand), otherwise the head of [chain].
     */
    fun primaryHost(): MapHost =
        (_catalog.value as? CatalogState.Loaded)?.offers?.firstOrNull()?.host ?: chain().first()

    fun setHostOverride(url: String?) {
        val clean = url?.trim()?.takeIf { it.isNotEmpty() }
        prefs.edit { putString(KEY_HOST, clean) }
        _settings.update { it.copy(hostOverride = clean) }
        _catalog.value = CatalogState.Idle
    }

    fun setThroughRelay(on: Boolean) {
        prefs.edit { putBoolean(KEY_RELAY, on) }
        _settings.update { it.copy(throughRelay = on) }
        _catalog.value = CatalogState.Idle
    }

    private fun ffiSettings() = _settings.value.let {
        uniffi.pingers.MapSettings(it.hostOverride, BuildConfig.MAP_HOST.trimEnd('/'), it.throughRelay)
    }

    /** Asks the core what the host chain offers. Catalogs say nothing about where the user is. */
    fun refreshCatalog() {
        _catalog.value = CatalogState.Loading
        scope.launch {
            val core = core()
            if (core == null) {
                _catalog.value = CatalogState.NoCore
                return@launch
            }
            _catalog.value = try {
                val catalog = core.mapCatalog(ffiSettings())
                CatalogState.Loaded(catalog.offers.map(RegionOffer::of), catalog.failed.map(FailedHost::of))
            } catch (e: CoreException) {
                CatalogState.Loaded(emptyList(), emptyList())
            }
        }
    }

    /** Has the core download (or update) a region from the host that offers it. */
    fun download(offer: RegionOffer) {
        if (jobs[offer.id]?.isActive == true) return
        jobs[offer.id] = scope.launch {
            val core = core()
            if (core == null) {
                fail(offer.id, Failure.NoCore)
                return@launch
            }
            val listener = object : MapDownloadListener {
                override fun downloaded(done: ULong, total: ULong) =
                    _downloads.update { it + (offer.id to DownloadState.Running(done.toLong(), total.toLong())) }

                override fun preparing(retryAfterSeconds: ULong) =
                    _downloads.update { it + (offer.id to DownloadState.Preparing(retryAfterSeconds.toInt())) }
            }
            try {
                core.downloadMap(MapDownloadRequest(offer.offer, ffiSettings(), directory.absolutePath), listener)
                _downloads.update { it - offer.id }
                synchronized(indexes) { indexes.remove(offer.id) }
                reloadInstalled(core)
            } catch (e: CancellationException) {
                throw e
            } catch (e: CoreException.Map) {
                fail(offer.id, failureOf(e.failure))
            } catch (e: CoreException) {
                fail(offer.id, Failure.Network)
            }
        }
    }

    private fun fail(id: String, reason: Failure) = _downloads.update { it + (id to DownloadState.Failed(reason)) }

    fun delete(id: String) {
        jobs.remove(id)?.cancel()
        synchronized(indexes) { indexes.remove(id) }
        _downloads.update { it - id }
        scope.launch {
            val core = core() ?: return@launch
            try {
                core.deleteMap(directory.absolutePath, id)
            } catch (_: CoreException) {
                // What is left stays listed as installed or not by the reload below.
            }
            reloadInstalled(core)
        }
    }

    private suspend fun reloadInstalled(opened: Core? = null) {
        val core = opened ?: core() ?: return
        _installed.value = try {
            core.installedMaps(directory.absolutePath).map(Manifest::of)
        } catch (_: CoreException) {
            emptyList()
        }
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

/** Why the core's download failed, as the picker says it. */
fun failureOf(failure: MapFailure): Failure = when (failure) {
    MapFailure.NETWORK -> Failure.Network
    MapFailure.CHECKSUM -> Failure.Checksum
    MapFailure.NOT_OFFERED -> Failure.NotOffered
    MapFailure.PREPARING -> Failure.Preparing
    MapFailure.NOT_THROUGH_RELAY -> Failure.NotThroughRelay
    MapFailure.NO_RELAY_ROUTE -> Failure.NoRelayRoute
    MapFailure.KEY_CHANGED -> Failure.KeyChanged
    MapFailure.STORAGE -> Failure.Storage
}

/** Whether [installed] is older than what the host offers: updates are offered, never automatic. */
fun updateAvailable(installed: Manifest, offer: RegionOffer): Boolean =
    (offer.entry as? CatalogEntry.Ready)?.manifest?.let { it.buildDate > installed.buildDate || (it.buildDate == installed.buildDate && it.pack.sha256 != installed.pack.sha256) } ?: false
