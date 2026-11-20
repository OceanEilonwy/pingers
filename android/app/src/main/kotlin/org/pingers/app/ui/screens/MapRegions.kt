package org.pingers.app.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.pingers.app.Navigator
import org.pingers.app.R
import org.pingers.app.data.Coordinate
import org.pingers.app.maps.CatalogEntry
import org.pingers.app.maps.CatalogState
import org.pingers.app.maps.DownloadState
import org.pingers.app.maps.FailedHost
import org.pingers.app.maps.Failure
import org.pingers.app.maps.HostProblem
import org.pingers.app.maps.Manifest
import org.pingers.app.maps.Maps
import org.pingers.app.maps.RegionOffer
import org.pingers.app.maps.updateAvailable
import org.pingers.app.ui.E2eTags
import org.pingers.app.ui.components.Card
import org.pingers.app.ui.components.RowDivider
import org.pingers.app.ui.components.TopBar
import org.pingers.app.ui.theme.Pc
import org.pingers.app.ui.theme.body
import uniffi.pingers.DownloadedMapsWords

/*
 * Downloaded maps (design canvas DownloadedMaps), the region picker: what is on this phone,
 * the areas on offer from the map host chain, and what the host sees. Its words are
 * pingers-words' (downloadedMapsWords, sizeWords, downloadingWords). "This area" is worked out
 * here from [around] (the map the user came from, on this phone only); nothing about it is sent
 * anywhere. Updates are offered, never automatic.
 */

@Composable
fun MapRegionsScreen(maps: Maps, around: Coordinate?, nav: Navigator) {
    LaunchedEffect(Unit) { maps.refreshCatalog() }
    val catalog by maps.catalog.collectAsState()
    val installed by maps.installed.collectAsState()
    val downloads by maps.downloads.collectAsState()
    val settings by maps.settings.collectAsState()
    val host = maps.primaryHost()
    val words = uniffi.pingers.downloadedMapsWords(host.name, host.source.toFfi(), settings.throughRelay)
    Page(top = { TopBar(words.title, { nav.back() }, words.back) }, spacing = 0) {
        Text(words.intro, style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f), modifier = Modifier.padding(start = 4.dp, end = 4.dp, top = 4.dp, bottom = 6.dp))
        // The same switch as in Advanced, here too: this is where people look for map settings.
        val relay = mapsWords(host, settings.throughRelay)
        Card(Modifier.padding(top = 8.dp, bottom = 4.dp)) {
            org.pingers.app.ui.components.SwitchRow(relay.relaySetting, relay.relayDetail, settings.throughRelay, maps::setThroughRelay, titleSize = 15)
        }
        val offers = (catalog as? CatalogState.Loaded)?.offers.orEmpty()
        val offered = offers.map { it.id }.toSet()
        val here = around?.let { c -> offers.filter { o -> o.bbox()?.let { c in it } == true } }.orEmpty().map { it.id }.toSet()
        val mine = installed.map { it.id }.toSet()
        val onPhone = installed.filter { it.id !in offered }
        val installedOffers = offers.filter { it.id in mine }
        if (installedOffers.isNotEmpty() || onPhone.isNotEmpty()) {
            Heading(words.installed)
            Card {
                installedOffers.forEachIndexed { i, offer ->
                    if (i > 0) RowDivider()
                    RegionRow(offer, installed.firstOrNull { it.id == offer.id }, downloads[offer.id], maps, offer.id in here, words)
                }
                onPhone.forEachIndexed { i, m ->
                    if (i > 0 || installedOffers.isNotEmpty()) RowDivider()
                    InstalledOnlyRow(m, maps, words)
                }
            }
        }
        when (val c = catalog) {
            CatalogState.Idle, CatalogState.Loading ->
                Text(stringResource(R.string.maps_loading), style = body(14.sp, color = Pc.Muted), modifier = Modifier.padding(start = 4.dp, top = 20.dp))
            is CatalogState.Loaded -> {
                // "This area" first, then the rest by group.
                val rest = c.offers.filter { it.id !in mine }.sortedByDescending { it.id in here }
                rest.groupBy { it.entry.group }.forEach { (group, inGroup) ->
                    Heading(group ?: words.available)
                    Card {
                        inGroup.forEachIndexed { i, offer ->
                            if (i > 0) RowDivider()
                            RegionRow(offer, null, downloads[offer.id], maps, offer.id in here, words)
                        }
                    }
                }
                if (c.offers.isEmpty() && c.failedHosts.none { it.problem != HostProblem.Unreachable }) {
                    Text(stringResource(R.string.maps_none), style = body(14.sp, color = Pc.Muted, lineHeight = 1.45f), modifier = Modifier.padding(top = 12.dp))
                }
                c.failedHosts.forEach { failed -> HostProblemLine(failed) }
            }
            CatalogState.NoCore ->
                Text(stringResource(R.string.maps_no_core), style = body(14.sp, color = Pc.OrangeText, lineHeight = 1.45f), modifier = Modifier.padding(start = 4.dp, top = 20.dp))
        }
        Card(Modifier.padding(top = 20.dp), padding = PaddingValues(horizontal = 16.dp, vertical = 14.dp)) {
            Text(words.hostTitle, style = body(15.sp, 600))
            Text(words.hostDetail, style = body(13.sp, color = Pc.Muted, lineHeight = 1.45f), modifier = Modifier.padding(top = 6.dp))
        }
        Note(words.attribution, size = 12)
    }
}

private fun org.pingers.app.maps.MapHostSource.toFfi() = when (this) {
    org.pingers.app.maps.MapHostSource.UserOverride -> uniffi.pingers.MapHostSource.USER_OVERRIDE
    org.pingers.app.maps.MapHostSource.Server -> uniffi.pingers.MapHostSource.SERVER
    org.pingers.app.maps.MapHostSource.Publisher -> uniffi.pingers.MapHostSource.PUBLISHER
}

private fun RegionOffer.bbox() = (entry as? CatalogEntry.Ready)?.manifest?.bbox

/** Why a host offered nothing. */
@Composable
private fun HostProblemLine(failed: FailedHost) {
    val text = when (failed.problem) {
        HostProblem.Unreachable -> stringResource(R.string.maps_host_failed, failed.host.name)
        HostProblem.NotThroughRelay -> stringResource(R.string.maps_host_not_through_relay, failed.host.name)
        HostProblem.NoRelayRoute -> stringResource(R.string.maps_host_no_relay_route, failed.host.name)
        HostProblem.KeyChanged -> stringResource(R.string.maps_host_key_changed, failed.host.name)
    }
    val warning = failed.problem != HostProblem.Unreachable
    Text(
        text,
        style = body(if (warning) 14.sp else 12.sp, color = if (warning) Pc.OrangeText else Pc.Muted, lineHeight = 1.4f),
        modifier = Modifier.padding(top = 6.dp),
    )}

private fun Manifest.bytes() = (pack.size + index.size).coerceAtLeast(0).toULong()

@Composable
private fun RegionRow(offer: RegionOffer, installed: Manifest?, download: DownloadState?, maps: Maps, thisArea: Boolean, words: DownloadedMapsWords) {
    val size = (offer.entry as? CatalogEntry.Ready)?.manifest?.let { uniffi.pingers.sizeWords(it.bytes()) }
        ?: installed?.let { uniffi.pingers.sizeWords(it.bytes()) }
        ?: stringResource(R.string.maps_prepared_on_request)
    val update = installed != null && updateAvailable(installed, offer)
    val busy = download is DownloadState.Running || download is DownloadState.Preparing
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(if (update) 4.dp else 2.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(offer.entry.name, style = body(16.sp, 600))
                    if (thisArea) Chip(words.thisArea, Pc.Body, Pc.Chip)
                }
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    val line = (download as? DownloadState.Running)?.let { uniffi.pingers.downloadingWords(it.done.toULong(), it.total.toULong()) } ?: size
                    Text(line, style = body(13.sp, color = Pc.Muted))
                    if (update && !busy) Chip(words.updateAvailable, Pc.BlueDark, Pc.BlueTint)
                }
            }
            when {
                busy -> {}
                installed == null -> RowButton(words.download, Pc.Ink, outlined = true) { maps.download(offer) }
                update -> RowButton(words.update, Color.White, outlined = false) { maps.download(offer) }
                else -> RowButton(words.delete, Pc.Red, outlined = true) { maps.delete(offer.id) }
            }
        }
        DownloadLine(download, offer.entry.name)
    }
}

@Composable
private fun InstalledOnlyRow(manifest: Manifest, maps: Maps, words: DownloadedMapsWords) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(manifest.name, style = body(16.sp, 600))
            Text(uniffi.pingers.sizeWords(manifest.bytes()), style = body(13.sp, color = Pc.Muted))
        }
        RowButton(words.delete, Pc.Red, outlined = true) { maps.delete(manifest.id) }
    }
}

@Composable
private fun Chip(text: String, color: Color, background: Color) {
    Text(
        text, style = body(12.sp, 600, color),
        modifier = Modifier.clip(RoundedCornerShape(999.dp)).background(background).padding(horizontal = 8.dp, vertical = 2.dp),
    )
}

/** Update (blue), Delete (outlined, red) and Download (outlined). */
@Composable
private fun RowButton(text: String, content: Color, outlined: Boolean, onClick: () -> Unit) {
    val shape = RoundedCornerShape(20.dp)
    Box(
        Modifier
            .height(40.dp)
            .clip(shape)
            .background(if (outlined) Pc.Card else Pc.Blue)
            .then(if (outlined) Modifier.border(1.dp, Pc.Handle, shape) else Modifier)
            .clickable(role = Role.Button, onClick = onClick)
            .testTag(E2eTags.MAPS_ACTION)
            .padding(horizontal = 16.dp),
        contentAlignment = Alignment.Center,
    ) { Text(text, style = body(14.sp, 600, content)) }
}

@Composable
private fun DownloadLine(download: DownloadState?, name: String) {
    when (download) {
        is DownloadState.Running -> LinearProgressIndicator(
            progress = { if (download.total > 0) download.done.toFloat() / download.total else 0f },
            modifier = Modifier.fillMaxWidth().height(6.dp).semantics { contentDescription = name },
            color = Pc.Blue,
            trackColor = Pc.Line,
        )
        is DownloadState.Preparing -> {
            LinearProgressIndicator(Modifier.fillMaxWidth().height(6.dp), color = Pc.Blue, trackColor = Pc.Line)
            Text(stringResource(R.string.maps_preparing, download.seconds), style = body(12.sp, color = Pc.Muted))
        }
        is DownloadState.Failed -> Text(
            stringResource(
                when (download.reason) {
                    Failure.Network -> R.string.maps_failed_network
                    Failure.Checksum -> R.string.maps_failed_checksum
                    Failure.Preparing -> R.string.maps_failed_preparing
                    Failure.NotThroughRelay -> R.string.maps_failed_not_through_relay
                    Failure.NoRelayRoute -> R.string.maps_failed_no_relay_route
                    Failure.KeyChanged -> R.string.maps_failed_key_changed
                    Failure.Storage -> R.string.maps_failed_storage
                    Failure.NoCore -> R.string.maps_no_core
                    Failure.NotOffered -> R.string.maps_failed_not_offered
                },
            ),
            style = body(13.sp, color = Pc.OrangeText, lineHeight = 1.4f),
        )
        null -> {}
    }
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
