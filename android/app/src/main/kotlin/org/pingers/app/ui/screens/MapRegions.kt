package org.pingers.app.ui.screens

import android.text.format.Formatter
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.data.Coordinate
import org.pingers.app.maps.CatalogEntry
import org.pingers.app.maps.CatalogState
import org.pingers.app.maps.DownloadState
import org.pingers.app.maps.Failure
import org.pingers.app.maps.Manifest
import org.pingers.app.maps.Maps
import org.pingers.app.maps.RegionOffer
import org.pingers.app.maps.updateAvailable
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.SectionLabel
import org.pingers.app.ui.components.Tag
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body

/*
 * The region picker: areas on offer from the map host chain, which host serves each, sizes,
 * what is on this phone and from which build. "This area" is worked out here from [around]
 * (the map the user came from, on this phone only); nothing about it is sent anywhere.
 * Updates are offered, never automatic.
 */

@Composable
fun MapRegionsScreen(maps: Maps, around: Coordinate?, nav: Navigator) {
    LaunchedEffect(Unit) { maps.refreshCatalog() }
    val catalog by maps.catalog.collectAsState()
    val installed by maps.installed.collectAsState()
    val downloads by maps.downloads.collectAsState()
    Page(top = { TopBar(stringResource(R.string.maps_title), { nav.back() }, stringResource(R.string.cd_back)) }) {
        Text(stringResource(R.string.maps_intro), style = body(14.sp, color = Pc.Body, lineHeight = 1.45f), modifier = Modifier.padding(top = 4.dp))
        val offers = (catalog as? CatalogState.Loaded)?.offers.orEmpty()
        val offered = offers.map { it.id }.toSet()
        val here = around?.let { c -> offers.filter { o -> o.bbox()?.let { c in it } == true } }.orEmpty()
        val mine = installed.map { it.id }.toSet()
        if (here.isNotEmpty()) {
            SectionLabel(stringResource(R.string.maps_this_area), Modifier.padding(top = 8.dp))
            RegionCard(here, installed, downloads, maps, thisArea = true)
        }
        val onPhone = installed.filter { it.id !in offered }
        val installedOffers = offers.filter { it.id in mine && it !in here }
        if (installedOffers.isNotEmpty() || onPhone.isNotEmpty()) {
            SectionLabel(stringResource(R.string.maps_installed), Modifier.padding(top = 8.dp))
            Card {
                installedOffers.forEachIndexed { i, offer ->
                    if (i > 0) RowDivider()
                    RegionRow(offer, installed.firstOrNull { it.id == offer.id }, downloads[offer.id], maps, thisArea = false)
                }
                onPhone.forEachIndexed { i, m ->
                    if (i > 0 || installedOffers.isNotEmpty()) RowDivider()
                    InstalledOnlyRow(m, maps)
                }
            }
        }
        when (val c = catalog) {
            CatalogState.Idle, CatalogState.Loading ->
                Text(stringResource(R.string.maps_loading), style = body(14.sp, color = Pc.Muted), modifier = Modifier.padding(top = 8.dp))
            is CatalogState.Loaded -> {
                val rest = c.offers.filter { it.id !in mine && it !in here }
                rest.groupBy { it.entry.group }.forEach { (group, inGroup) ->
                    SectionLabel(group ?: stringResource(R.string.maps_available), Modifier.padding(top = 8.dp))
                    RegionCard(inGroup, installed, downloads, maps, thisArea = false)
                }
                if (c.relayUnavailable) {
                    Text(stringResource(R.string.maps_failed_relay), style = body(14.sp, color = Pc.OrangeText, lineHeight = 1.45f))
                } else if (c.offers.isEmpty()) {
                    Text(stringResource(R.string.maps_none), style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f))
                }
                c.failedHosts.forEach { host ->
                    Text(stringResource(R.string.maps_host_failed, host.name), style = body(12.sp, color = Pc.Muted))
                }
            }
        }
    }
}

private fun RegionOffer.bbox() = when (val e = entry) {
    is CatalogEntry.Ready -> e.manifest.bbox
    is CatalogEntry.OnDemand -> e.bbox
}

@Composable
private fun RegionCard(offers: List<RegionOffer>, installed: List<Manifest>, downloads: Map<String, DownloadState>, maps: Maps, thisArea: Boolean) {
    Card {
        offers.forEachIndexed { i, offer ->
            if (i > 0) RowDivider()
            RegionRow(offer, installed.firstOrNull { it.id == offer.id }, downloads[offer.id], maps, thisArea)
        }
    }
}

@Composable
private fun RegionRow(offer: RegionOffer, installed: Manifest?, download: DownloadState?, maps: Maps, thisArea: Boolean) {
    val context = LocalContext.current
    val size = (offer.entry as? CatalogEntry.Ready)?.manifest?.let { Formatter.formatShortFileSize(context, it.pack.size + it.index.size) }
        ?: stringResource(R.string.maps_prepared_on_request)
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(offer.entry.name, style = body(16.sp, 600))
                    if (thisArea) Tag(stringResource(R.string.maps_this_area), Pc.BlueDark, Pc.BlueTint)
                }
                Text("$size · " + stringResource(R.string.maps_from_host, offer.host.name), style = body(13.sp, color = Pc.Muted))
                if (installed != null) {
                    Text(stringResource(R.string.maps_data_from, installed.buildDate), style = body(13.sp, color = Pc.Muted))
                    if (updateAvailable(installed, offer)) Text(stringResource(R.string.maps_update_available), style = body(13.sp, 600, Pc.OrangeText))
                }
            }
            when {
                download is DownloadState.Running || download is DownloadState.Preparing -> {}
                installed == null -> Action(stringResource(if (download is DownloadState.Failed) R.string.maps_retry else R.string.maps_download)) { maps.download(offer) }
                updateAvailable(installed, offer) -> Action(stringResource(R.string.maps_update)) { maps.download(offer) }
                else -> Action(stringResource(R.string.maps_delete), destructive = true) { maps.delete(offer.id) }
            }
        }
        DownloadLine(download)
    }
}

@Composable
private fun InstalledOnlyRow(manifest: Manifest, maps: Maps) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(manifest.name, style = body(16.sp, 600))
            Text(stringResource(R.string.maps_data_from, manifest.buildDate), style = body(13.sp, color = Pc.Muted))
        }
        Action(stringResource(R.string.maps_delete), destructive = true) { maps.delete(manifest.id) }
    }
}

@Composable
private fun DownloadLine(download: DownloadState?) {
    val context = LocalContext.current
    when (download) {
        is DownloadState.Running -> {
            LinearProgressIndicator(
                progress = { if (download.total > 0) download.done.toFloat() / download.total else 0f },
                modifier = Modifier.fillMaxWidth().padding(top = 4.dp),
                color = Pc.Blue,
                trackColor = Pc.Line,
            )
            Text(
                stringResource(R.string.maps_downloading, Formatter.formatShortFileSize(context, download.done), Formatter.formatShortFileSize(context, download.total)),
                style = body(12.sp, color = Pc.Muted),
            )
        }
        is DownloadState.Preparing -> {
            LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 4.dp), color = Pc.Blue, trackColor = Pc.Line)
            Text(stringResource(R.string.maps_preparing, download.seconds), style = body(12.sp, color = Pc.Muted))
        }
        is DownloadState.Failed -> Text(
            stringResource(
                when (download.reason) {
                    Failure.Network -> R.string.maps_failed_network
                    Failure.Checksum -> R.string.maps_failed_checksum
                    Failure.RelayUnavailable -> R.string.maps_failed_relay
                    Failure.NotOffered -> R.string.maps_failed_not_offered
                },
            ),
            style = body(13.sp, color = Pc.OrangeText, lineHeight = 1.4f),
        )
        null -> {}
    }
}

@Composable
private fun Action(text: String, destructive: Boolean = false, onClick: () -> Unit) {
    Box(
        Modifier.heightIn(min = 48.dp).clickable(role = Role.Button, onClick = onClick).padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = body(15.sp, 600, if (destructive) Pc.OrangeText else Pc.Blue)) }
}

/** The Maps rows for Advanced: the host in use, an override, the relay setting and the picker. */
@Composable
fun MapsAdvancedSection(maps: Maps, onOpenRegions: () -> Unit) {
    val settings by maps.settings.collectAsState()
    val host = maps.primaryHost()
    val words = mapsWords(host, settings.throughRelay)
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(words.heading, style = body(13.sp, 600, Pc.Muted))
        Card(radius = 14.dp) {
            Column(Modifier.padding(horizontal = 14.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(words.row, style = body(15.sp, 500))
                Text(words.source, style = body(12.sp, color = Pc.Muted))
            }
            RowDivider()
            org.pingers.app.ui.components.SwitchRow(words.relaySetting, words.relayDetail, settings.throughRelay, maps::setThroughRelay, titleSize = 15)
            RowDivider()
            org.pingers.app.ui.components.ListRow(onOpenRegions, padding = PaddingValues(horizontal = 14.dp, vertical = 12.dp)) {
                Text(stringResource(R.string.maps_regions_link), style = body(15.sp, 500), modifier = Modifier.weight(1f))
                org.pingers.app.ui.components.ChevronIcon()
            }
        }
        org.pingers.app.ui.components.LabeledField(
            stringResource(R.string.maps_host_label), settings.hostOverride.orEmpty(), { maps.setHostOverride(it) },
            background = Pc.Card, height = 52.dp, helper = stringResource(R.string.maps_host_hint),
        )
    }
}

/** What the map host sees, for What your server sees. */
@Composable
fun MapsServerSection(maps: Maps) {
    val settings by maps.settings.collectAsState()
    val words = mapsWords(maps.primaryHost(), settings.throughRelay)
    Column(verticalArrangement = Arrangement.spacedBy(0.dp)) {
        Text(words.row, style = body(15.sp, 600), modifier = Modifier.padding(top = 18.dp, bottom = 2.dp))
        Text(words.source, style = body(12.sp, color = Pc.Muted), modifier = Modifier.padding(bottom = 8.dp))
        Card(padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
            words.canSee.forEachIndexed { i, t ->
                if (i > 0) RowDivider()
                Text(t, style = body(14.sp, lineHeight = 1.45f), modifier = Modifier.padding(vertical = 10.dp))
            }
        }
        Card(Modifier.padding(top = 10.dp), padding = PaddingValues(horizontal = 16.dp, vertical = 6.dp)) {
            words.neverSees.forEachIndexed { i, t ->
                if (i > 0) RowDivider()
                Text(t, style = body(14.sp, lineHeight = 1.45f), modifier = Modifier.padding(vertical = 10.dp))
            }
        }
        Text(words.fallback, style = body(12.sp, color = Pc.Muted, lineHeight = 1.4f), modifier = Modifier.padding(top = 8.dp))
    }
}

/** pingers-words' maps lines (through pingers-ffi), for the host in use. */
private fun mapsWords(host: org.pingers.app.maps.MapHost, throughRelay: Boolean): uniffi.pingers.MapsWords =
    uniffi.pingers.mapsWords(
        host.name,
        when (host.source) {
            org.pingers.app.maps.MapHostSource.UserOverride -> uniffi.pingers.MapHostSource.USER_OVERRIDE
            org.pingers.app.maps.MapHostSource.Server -> uniffi.pingers.MapHostSource.SERVER
            org.pingers.app.maps.MapHostSource.Publisher -> uniffi.pingers.MapHostSource.PUBLISHER
        },
        throughRelay,
    )
