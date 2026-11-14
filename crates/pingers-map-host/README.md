# pingers-map-host

The map host: a public static service that serves whole-region map data. Clients download a
region's files whole and use them offline; they never fetch tiles per viewport. Like the push
relay, it is publisher-hosted by default and an operator can run their own.

## Files and routes

Region ids are lowercase `[a-z0-9-]` segments joined by `/`, for example
`oceania/australia/victoria`. In file names and URLs the `/` becomes `.`:

| Route | What | `Cache-Control` |
|---|---|---|
| `GET /catalog.json` | Generated: `{"format":1,"regions":[...]}` | `public, max-age=300` |
| `GET /<stem>.json` | Region manifest | `public, max-age=300` |
| `GET /<stem>.pmtiles` | PMTiles pack (`application/vnd.pmtiles`) | `public, max-age=31536000, immutable` |
| `GET /<stem>.places.tsv` | Place search index | `public, max-age=31536000, immutable` |
| `POST /ohttp` | The host's own OHTTP gateway (`message/ohttp-req`), below | `no-store` |
| `GET /ohttp-keys` | The gateway's key config (`application/ohttp-keys`) | `public, max-age=300` |

- Only regions on the `hosted` allow-list are served. Other files are answered 404, even if
  they are in the directory.
- HEAD works on every route. Any other method gets 405. Any other path gets 404 with no body.
  The host never serves dotfiles, subdirectories or paths with `..`, and it follows a symlink
  only to a file directly in the directory.
- Each file gets a strong `ETag`: the sha256 of its bytes, computed by the host and cached by
  length and modification time. The host does not trust the manifests' `sha256` fields for
  this. `If-None-Match` gets 304.
- A single byte range (`bytes=a-b`, `bytes=a-`, `bytes=-n`) gets 206 with `Content-Range`. A
  range that cannot be satisfied gets 416 with `Content-Range: bytes */<len>`. A request for
  several ranges gets the whole file with 200. `If-Range` is honoured with a strong
  comparison. Files are streamed from disk, never read whole into memory.
- The catalog lists the manifest of every allow-listed region in the directory, sorted by id.
  In cache mode it lists every allow-listed region in the region list. A region that has not
  been cut yet appears as `{"id","name","group","status":"on_demand"}`. It answers byte ranges
  and `If-Range` like a file, so a client can fetch a large catalog in pieces of one size.

### Downloads through a privacy relay

A phone with "Download maps through privacy relay" on seals each request to this host's key and
sends it through its OHTTP relay to its Pingers server, which forwards the sealed bytes here
(`map_host_gateway` in the server's configuration; design B.12). So the relay sees the phone's
address but not the request, the server sees neither the address nor the file, and this host sees
the file but only the server's address.

- `POST /ohttp` opens the request (a `GET` or `HEAD` of any route above; anything else gets 405
  inside) and answers it exactly as a direct request, keeping only `content-type`,
  `content-range`, `etag`, `retry-after` and `accept-ranges`. Of the request's headers only
  `range`, `if-range` and `if-none-match` are used.
- The encoded answer is padded with zeros (RFC 9292) to a power of two of at least 1 KiB, or to
  the `pingers-pad` length the request names if larger, at most just under 1 MiB; an answer that
  would not fit is a 413 inside. Phones ask for 256 KiB pieces padded to 256 KiB, so every piece
  of every download looks the same.
- A request that does not open is 400, one over 8 KiB is 413, a wrong media type is 415.
- The key (X25519, key id 1) is 64 hex digits in `ohttp_key_file`, by default `.ohttp-key` in
  the directory (never served: the host serves no dotfiles). The host makes it on first start,
  readable by its owner only. If it is lost, phones that saw the old key stop relayed downloads
  until their users trust the new one; keep it out of `rsync --delete`.

## Configuration

```toml
listen = "127.0.0.1:8090"
directory = "/srv/pingers-maps"   # pingers-tiles output (or the cache, in cache mode)
max_disk = "60GB"                 # B, KB, MB, GB, TB (powers of 1000) or KiB, MiB, GiB, TiB
hosted = ["oceania", "europe/germany", "north-america/us/california"]
logging = "off"                   # or "counts"
counts_interval_s = 3600
ohttp_key_file = "/var/lib/pingers-maps/ohttp-key"   # optional; default <directory>/.ohttp-key

[max_zoom]                        # optional; the longest matching prefix wins
"europe/germany" = 13

[cache]                           # optional; its presence turns on cache mode
regions = "regions.toml"          # pingers-tiles region list, relative to this file
upstream = "https://build.protomaps.com/20261112.pmtiles"
pmtiles = "/usr/local/bin/pmtiles"
max_concurrent_extracts = 1
retry_after_seconds = 30
```

`listen`, `directory`, `max_disk` and `hosted` are required. Each `hosted` entry is a prefix
that matches whole segments: `"oceania"` hosts `oceania` and everything under `oceania/`, but
not `oceanian`. `max_zoom` overrides the region list's zoom when cache mode cuts a region.
`pingers-tiles plan` reads it too.

Run it:

```sh
cargo run --release -p pingers-map-host -- /etc/pingers/map-host.toml
```

It stops cleanly on Ctrl-C or SIGTERM.

### Modes

- **Pre-extracted** (default): serve the directory that `pingers-tiles` wrote. At start-up, if
  the directory holds more than `max_disk`, the host prints a one-line warning and serves
  anyway.
- **Cache**: when a client asks for a file of an allow-listed region that is not on disk, the
  host starts a cut with `pmtiles extract` from `upstream`. It then builds the index and
  manifest with pingers-map-packs, the same code `pingers-tiles` uses. Until the cut is done,
  the client gets `503` with `Retry-After: <retry_after_seconds>` and
  `{"status":"preparing","retry_after":N}`. At most `max_concurrent_extracts` cuts run at
  once, and the rest wait their turn. After each cut, the host evicts the least recently
  served regions until the region files fit within `max_disk`. It never evicts the region it
  just cut or one being cut. It evicts a region that is being downloaded only if nothing else
  is left. Last-served times and provenance (the upstream and cut time of each region) are
  kept in `cache-state.json` in the directory. A region that is not allow-listed is never cut.

## Logging and what the host learns

Logging is off by default. With `logging = "counts"`, every `counts_interval_s` the host
prints one line to stderr. The line holds requests by status class, body bytes, cuts, failed
cuts, evictions and directory disk usage against `max_disk`. The host never logs or stores
client addresses, paths, regions or single requests. It sets no cookies.

The host still sees each client's IP address and which region files that client downloads,
unless the client fetches through its privacy relay and Pingers server (above): then it sees the
server's address instead. It never sees viewports, positions or
searches, because clients search on the device against the downloaded index.

## Deployment

The listener speaks HTTP/1.1 and cleartext HTTP/2 (h2c). Put a reverse proxy in front of it
to terminate TLS (Caddy, nginx and others work), and keep the listener on loopback or a
private network. Packs and indexes are immutable and versioned by their manifest's sha256, so
a CDN can cache them for a long time. Manifests and the catalog have a 5-minute lifetime.

## Sizing guide for operators

Check pack sizes before you commit to an allow-list:

```sh
pingers-tiles plan --host-config host.toml regions.toml
```

This prints the estimated size of each allow-listed region (from `pmtiles extract --dry-run`,
with `max_zoom` overrides applied). It refuses if the total is over `max_disk`.

Rough examples for a 60 GB box. Pack sizes vary with how dense the map is, so treat these
figures as guesses and confirm them with `plan`:

- **One continent at zoom 14.** For example `hosted = ["europe"]` with
  `[max_zoom] "europe" = 14`. Zoom 15 for a whole continent will not fit.
- **A few countries at full zoom 15.** For example
  `hosted = ["europe/germany", "europe/france", "oceania/australia"]`.
- **Mixed.** A home country at 15 and its neighbours at 13, using `[max_zoom]` prefixes.

Cache mode trade-offs:

- **Latency on first request.** The first client to ask for a region waits for the cut, which
  takes minutes for a large region. Later clients get it immediately until it is evicted.
  Pre-extract regions that you know will be popular.
- **Politeness to the upstream.** Each cut reads from the upstream bucket. Keep
  `max_concurrent_extracts` low (1 is the default). Pin `upstream` to a dated build, and
  mirror it yourself if you cut often.
- **What the upstream learns.** The upstream bucket sees the host's IP address and which
  regions the host cuts and when. It does not see your users' addresses or which user asked.
- **Disk churn.** If `max_disk` is smaller than what your users ask for, regions are evicted
  and cut again. Watch `cuts` and `evictions` in the counts log.
