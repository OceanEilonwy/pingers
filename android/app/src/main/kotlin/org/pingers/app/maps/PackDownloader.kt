package org.pingers.app.maps

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/*
 * Downloads whole files from a map host: resumable (Range), verified against the manifest's
 * SHA-256, written beside the final name and renamed into place only when complete.
 *
 * Direct downloads tell the map host this phone's internet address and which region it fetched.
 * "Download maps through privacy relay" sends them through the OHTTP relay instead; see
 * [RelayFetcher] for how far that goes today.
 */

/** One HTTP response's status, headers we use and body. */
class Fetched(val status: Int, val retryAfterSeconds: Int?, val body: InputStream?, val close: () -> Unit)

/** How a request reaches the map host. */
fun interface Fetcher {
    /** GET [url], from byte [from] onwards when it is above zero. */
    @Throws(IOException::class)
    fun get(url: String, from: Long): Fetched
}

/** Straight to the map host. */
object DirectFetcher : Fetcher {
    override fun get(url: String, from: Long): Fetched {
        val connection = URL(url).openConnection() as HttpURLConnection
        connection.connectTimeout = 15_000
        connection.readTimeout = 30_000
        connection.instanceFollowRedirects = false
        connection.useCaches = false
        connection.setRequestProperty("User-Agent", "Pingers")
        if (from > 0) connection.setRequestProperty("Range", "bytes=$from-")
        val status = connection.responseCode
        val body = if (status in 200..299) connection.inputStream else null
        return Fetched(status, connection.getHeaderField("Retry-After")?.trim()?.toIntOrNull(), body) { connection.disconnect() }
    }
}

/**
 * Through the privacy relay (OHTTP), so the map host never sees this phone's address.
 *
 * Not available yet: the shell has no OHTTP client. The Rust core's transport
 * (pingers-transport) holds the only one, and pingers-ffi does not expose a "fetch through the
 * relay" call. The intended design, documented in android/docs/maps.md: the core fetches each
 * file in ranges of at most 1 MiB, each range one ordinary OHTTP request whose inner request is
 * `GET <map host>/<file>` with `Range`, through the party's relay to the gateway, which needs a
 * route that forwards to the map host. Until then this fetcher refuses, and the app says so
 * rather than downloading directly behind the user's back.
 */
object RelayFetcher : Fetcher {
    override fun get(url: String, from: Long): Fetched = throw RelayUnavailable()
}

class RelayUnavailable : IOException("Downloads through the privacy relay aren't available yet")

/** The map host is cutting this region (cache mode); try again after [seconds]. */
class Preparing(val seconds: Int) : IOException("The map host is preparing this area")

class ChecksumMismatch(file: String) : IOException("$file did not match its checksum")

class HttpFailure(val status: Int) : IOException("The map host answered $status")

/**
 * Fetches [url] into [target], resuming a `.partial` file left by an earlier attempt, and
 * checks the result against [sha256]. [progress] gets bytes so far.
 */
@Throws(IOException::class)
fun downloadFile(fetcher: Fetcher, url: String, target: File, size: Long, sha256: String, progress: (Long) -> Unit) {
    val partial = File(target.path + ".partial")
    var from = if (partial.exists()) partial.length() else 0L
    if (from > size) {
        partial.delete()
        from = 0
    }
    if (from < size || size == 0L) {
        val response = fetcher.get(url, from)
        try {
            when (response.status) {
                200 -> from = 0
                206 -> {}
                416 -> {
                    partial.delete()
                    throw HttpFailure(416)
                }
                503 -> throw Preparing(response.retryAfterSeconds ?: 30)
                else -> throw HttpFailure(response.status)
            }
            val body = response.body ?: throw HttpFailure(response.status)
            FileOutputStream(partial, from > 0).use { out ->
                val buffer = ByteArray(64 * 1024)
                var done = from
                progress(done)
                while (true) {
                    val n = body.read(buffer)
                    if (n < 0) break
                    out.write(buffer, 0, n)
                    done += n
                    progress(done)
                    if (done > size) throw ChecksumMismatch(target.name)
                }
            }
        } finally {
            response.close()
        }
    }
    if (sha256Of(partial) != sha256) {
        partial.delete()
        throw ChecksumMismatch(target.name)
    }
    if (!partial.renameTo(target)) throw IOException("Could not move ${target.name} into place")
}

fun sha256Of(file: File): String {
    val digest = MessageDigest.getInstance("SHA-256")
    file.inputStream().use { input ->
        val buffer = ByteArray(64 * 1024)
        while (true) {
            val n = input.read(buffer)
            if (n < 0) break
            digest.update(buffer, 0, n)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

/** Reads a small text resource (a catalog or a manifest) whole. */
@Throws(IOException::class)
fun fetchText(fetcher: Fetcher, url: String): String {
    val response = fetcher.get(url, 0)
    try {
        when (response.status) {
            200 -> {}
            503 -> throw Preparing(response.retryAfterSeconds ?: 30)
            else -> throw HttpFailure(response.status)
        }
        val body = response.body ?: throw HttpFailure(response.status)
        val out = java.io.ByteArrayOutputStream()
        val buffer = ByteArray(16 * 1024)
        while (true) {
            val n = body.read(buffer)
            if (n < 0) break
            out.write(buffer, 0, n)
            if (out.size() > MAX_TEXT) throw IOException("$url is too large")
        }
        return out.toByteArray().decodeToString()
    } finally {
        response.close()
    }
}

/** Larger than any catalog of a few thousand regions; small enough that a bad host can't fill memory. */
private const val MAX_TEXT = 8 * 1024 * 1024
