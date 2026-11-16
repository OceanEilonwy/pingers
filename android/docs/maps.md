# Maps

Pingers shows real maps without telling anyone where the user is looking.

- **Whole region packs, never tiles for the view.** A pack is one PMTiles file covering a city,
  state or country (OpenStreetMap data, the [Protomaps basemap](https://docs.protomaps.com/basemaps/downloads)),
  downloaded whole from a map host by the Rust core, checked against its SHA-256 and kept on
  the phone.
  MapLibre Native draws it from the file. No request ever names the part of the map on screen.
- **Search on the phone.** Each pack ships a place-name index (`<region>.places.tsv`: towns,
  suburbs, streets, points of interest). "Search a place" in the Spot editor matches it
  locally. There is no online geocoder.
- **No Google or Apple map SDKs.** "© OpenStreetMap contributors" is shown on every real map
  (ODbL).

Packs are cut by `crates/pingers-tiles` and served by `crates/pingers-map-host`; the shared
formats are in `crates/pingers-map-packs` (manifest: `src/manifest.rs`, index: `src/index.rs`).

## MapLibre and PMTiles

- `org.maplibre.gl:android-sdk-opengl:13.7.1` (OpenGL ES renderer, released 15 October 2026).
  MapLibre Native reads PMTiles through its `pmtiles://` source (since 11.7); a local pack is
  `pmtiles://file:///data/user/0/<app>/files/maps/<region>.pmtiles`.
- The style (`assets/maps/style.json`) is the Protomaps basemap's "light" flavour from
  `@protomaps/basemaps` 5.7.2, recoloured to the app's map palette, with `{{PACK_URL}}` replaced
  by the pack's URL when a map opens. Glyphs (Noto Sans: Latin, Greek, Cyrillic and punctuation
  ranges, SIL OFL) and sprites (v4, MIT, from tangrams/icons) are bundled under `assets/maps`,
  so nothing is fetched at render time. Text in scripts outside the bundled ranges falls back to
  Android's own fonts for CJK and is otherwise left unlabelled.
- Regenerate them with `android/tools/maps`: `node gen-style.mjs <@protomaps/basemaps
  dist/esm/index.js> app/src/main/assets/maps/style.json` and
  `sh assets.sh app/src/main/assets/maps`. Keep `@protomaps/basemaps` in step with the tiles'
  `version` (basemaps 5.x for 4.x tiles; the 2026-11-12 build is 4.15.2).

## How screens use it

Every map screen already carries a `MapFrame` (a coordinate at a scene point, metres per unit).
`MapBox(scene, frame = …)` works out MapLibre's camera from the same frame
(`cameraFor`: zoom z shows the world 512 dp wide, so a dp is `C·cos φ / (512·2^z)` metres), so
overlays drawn in map units (Spot circles and areas, hatched zones, member pins, the meet point,
the Draw area editor) lie on the right streets without changing their drawing code. Gestures on
MapLibre are off; the screens' own gestures move the frame (dragging the Spot editor's map moves
the Spot, dragging the meet point map moves the pin).

When no installed pack covers the frame's centre, the canvas's stylised placeholder is drawn,
with a **Download map for this area** prompt that opens the region picker.

| Screen | Map |
| --- | --- |
| Party | real map behind the Spot circles and areas |
| Spot editor | real map; drag to move the Spot; "Search a place" moves it to a found place |
| Draw area | real map under the point-by-point editor |
| Exclusion zones | real map; circle zones drawn from their centre and radius, drawn areas through their own vertices (the view model's `ZoneShape.Area` carries them) |
| Meetup, Meet point | real map under members, the meet point and the pin |
| Location request | stays the placeholder: a dimmed background, and its view model has no frame |

## Who downloads

The Rust core, never the shell (design E.1: the shells make no network calls). `Maps` asks the
core (pingers-ffi, over `crates/pingers-map-downloads`):

| Call | What the core does |
| --- | --- |
| `mapCatalog(MapSettings)` | fetches `catalog.json` from every host in the chain, in pieces, and offers each region from the first host that lists it |
| `downloadMap(MapDownloadRequest, listener)` | downloads the region's index and pack into `files/maps` in byte-range pieces, resuming a `.partial` file named after the checksum it must reach, checks each against the manifest's SHA-256 (pingers-crypto), moves it into place and writes the manifest last; reports bytes and "preparing" to the listener; retries a preparing cache-mode host after its `Retry-After` |
| `installedMaps(dir)` | the manifests in `files/maps` whose pack is there at full length |
| `deleteMap(dir, id)` | removes the manifest first, then the files and any partial downloads |

The shell keeps the user's settings (the override and the relay switch, in shared preferences),
draws the installed packs with MapLibre and searches their indexes on the phone. Cancelling a
download (Delete while it runs) cancels the core's call; the next download resumes the partial.

## The map host chain

Highest priority first, per region:

1. a user override (Advanced, "Map host");
2. the host the user's Pingers server advertises: `map_host` in `/v1/params` (pingers-proto
   `Params.map_host`). The core reads `/v1/params` through the relay once per process and keeps
   the last value in its store; it adds this host to the chain itself. `CoreHost.serverMapHost()`
   gives it to `Maps` for the words about which host is in use;
3. the publisher's default, `BuildConfig.MAP_HOST`, set with `-PpingersMapHost=https://…`
   or the `PINGERS_MAP_HOST` environment variable in CI. Deployment configuration, never source;
   unset, it is `https://maps.pingers.invalid`, which never resolves.

Map hosts are `https://` only. A host that lacks an area hands only that area to the next one.
Each row in the picker says which host serves it. A cache-mode host lists regions it has not cut
yet as `on_demand`; downloading one shows "The map host is preparing this area" while the core
waits.

## Region picker

Advanced → Maps → Downloaded maps, or the prompt on any placeholder map. It lists:

- **This area**: ready offers whose bbox contains the centre of the map the user came from.
  Worked out on the phone; nothing is sent.
- **On this phone**: installed packs with "Map data from <build date>", and "Update available"
  when a host offers a newer build. Updates are opt-in.
- the rest, grouped by region group (a country's states under the country), with the download
  size and host;
- below, each host that offered nothing and why: it did not answer, it cannot be reached
  through the privacy relay (or the app holds no pin for its key), there is no Pingers server to
  relay through yet, or its key is not the one the app pins.

## Download maps through privacy relay

With the setting on, every map request goes through the user's OHTTP relay and Pingers server
to the map host's own OHTTP gateway, sealed to the map host's key (design B.12, "Downloads
through the privacy relay"):

- The relay sees the phone's address and sizes; the server sees that some anonymous client
  downloaded map data of a size class; the map host sees which files, from the server's address.
  Every answer is padded to 256 KiB and each download's request count to a size class, so sizes
  do not name a region.
- Only the server's own map host can be reached this way (the server forwards to it and nowhere
  else). The override and the publisher's host are skipped, and the picker says so.
- The map host's key config comes through the server too, so the core uses it only if it is
  among the map host pins: `BuildConfig.MAP_HOST_KEY_PINS` (`PINGERS_MAP_HOST_KEY_PINS`, from the
  appliance's Setup page) for the built-in server, or the pins a setup link or invite brought.
  Nothing is pinned on first use and the user cannot override a mismatch; without pins nothing
  is fetched through the relay.
- Without a Pingers server (no instance), there is no relay route: nothing is sent, and the picker
  says so.

## Testing on the emulator

With the local stack (`cargo xtask local-stack --packs <dir of pingers-tiles output>`; docs/deploy-nodo.md,
"Local testing"), the map host is `https://10.0.2.2:9446` behind the stack's TLS front, the
server advertises it and forwards relayed map downloads to it (`map_host_gateway =
"http://127.0.0.1:9474"`), and `cargo xtask local-app` builds a debug app that trusts the stack's
CA and uses that host as the publisher's default:

```sh
cargo xtask local-stack --packs /path/packs
cargo xtask local-app && adb install -r android/app/build/outputs/apk/foss/debug/app-foss-debug.apk
# Debug builds can point maps at another https host:
adb shell am start -n org.pingers.app.foss.debug/org.pingers.app.MainActivity --es map_host https://10.0.2.2:9446
```

Turn **Download maps through privacy relay** on in Advanced to test the relayed path: the map
host's log shows the requests arriving from the server, not from the emulator.

## Open decisions

- ~~Who hosts the default packs~~: the publisher, configured per build through `PINGERS_MAP_HOST`.
- ~~Shell network calls~~: the core downloads (2026-11-14).
- **Pack granularity** for the publisher's host: city packs say more about the user than state
  packs (B.12, "Map host").

## Screenshots

From the emulator test (Pixel 9 Pro, 2026-11-13; the Central Melbourne pack served by
pingers-map-host on the host), in `docs/screenshots/maps/`: the placeholder with the download
prompt, the picker before and after downloading, "Search a place", and the Spot editor and Draw
area on the real map (`01` to `07`).

From the core-download test (Pixel 9 Pro, host GPU, 2026-11-14; `cargo xtask local-stack` with
the Central Melbourne pack), on Downloaded maps: the catalog from the core (`10`), a direct
download in progress (`11`) and the map drawn from it after "Search a place" (`12`); with
"Download maps through privacy relay" on, the catalog and host card (`13`), a relayed download
in progress (`14`), the region installed (`15`) and the map drawn from the relayed pack (`16`).
