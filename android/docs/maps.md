# Maps

Pingers shows real maps without telling anyone where the user is looking.

- **Whole region packs, never tiles for the view.** A pack is one PMTiles file covering a city,
  state or country (OpenStreetMap data, the [Protomaps basemap](https://docs.protomaps.com/basemaps/downloads)),
  downloaded whole from a map host, checked against its SHA-256 and kept on the phone.
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
| Exclusion zones | real map; circle zones drawn from their centre and radius. The zones view model has no vertices for drawn areas yet, so those are a fixed-size hatched block at their label |
| Meetup, Meet point | real map under members, the meet point and the pin |
| Location request | stays the placeholder: a dimmed background, and its view model has no frame |

## The map host chain

Highest priority first, per region:

1. a user override (Advanced, "Map host");
2. the host the user's Pingers server advertises: `map_host` in `/v1/params` (pingers-proto
   `Params.map_host`). **Gap:** pingers-server still answers `/v1/params` with 501 and pingers-ffi
   does not expose the field, so `Maps(serverMapHost = …)` answers null for now. pingers-server
   must fill `map_host` from a `map_host = "https://…"` line in its config, and pingers-ffi must
   pass it to the shell (a `Snapshot` field or a `Core` call);
3. the publisher's default, `BuildConfig.MAP_HOST`, set with `-PpingersMapHost=https://…`
   (a placeholder, `https://maps.pingers.invalid`, until the publisher decides who hosts).

The picker fetches `catalog.json` from every host in the chain and offers each region from the
first host that lists it (`resolveRegions`), so a host that lacks an area hands only that area
to the next one. Each row says which host serves it. A cache-mode host lists regions it has not
cut yet as `on_demand`; downloading one shows "The map host is preparing this area" and retries
after the host's `Retry-After`.

## Region picker

Advanced → Maps → Downloaded maps, or the prompt on any placeholder map. It lists:

- **This area**: offers whose bbox contains the centre of the map the user came from. Worked out
  on the phone; nothing is sent.
- **On this phone**: installed packs with "Map data from <build date>", and "Update available"
  when a host offers a newer build. Updates are opt-in.
- the rest, grouped by region group (a country's states under the country), with the download
  size and host.

Downloads resume (`Range`), are verified against the manifest's SHA-256 before they replace
anything, and a region counts as installed only once its manifest is written (last).

## Download maps through privacy relay

The setting exists and is honoured: with it on, the app does not download directly.

**Gap: relay downloads are not implemented.** The shell has no OHTTP client: the only one is in
the Rust core (pingers-transport), and pingers-ffi exposes no "fetch through the relay" call. So
with the setting on, downloads fail with "Downloads through the privacy relay aren't available
yet" instead of quietly going direct. To close it:

1. **Core:** a `Core::fetch_map_file(host, file, range)` that sends `GET <host>/<file>` with a
   `Range` header as ordinary OHTTP requests through the party's relay, ranges of at most 1 MiB
   each (relays limit response sizes; B.12 notes board pages are up to 256 KiB, so check the
   relay's limit), with a fresh HPKE context per request. Chunked OHTTP (draft-08) would carry a
   whole file in one exchange, but the transport's chunked support is shaped for the meetup live
   channel; per-range requests are simpler and resume naturally.
2. **Gateway:** the OHTTP gateway is the Pingers server, which only serves its own API. It needs
   a route that forwards `GET` with `Range` to the configured map host (an allow-list of one or
   more hosts, no other targets), or the map host must run its own OHTTP gateway.
3. **Shell:** `RelayFetcher` calls the core instead of throwing.

The relay then sees the phone's IP and the transfer sizes; the map host sees the gateway's IP and
the region; neither sees both.

## Testing on the emulator

```sh
# On the host: cut the test pack, then serve it with pingers-map-host.
pingers-tiles cut regions.toml --build https://build.protomaps.com/20261112.pmtiles \
    --out /path/packs --pmtiles /path/pmtiles
cargo run -p pingers-map-host -- host.toml      # listen = "0.0.0.0:8090", directory = /path/packs

# In the app (debug builds): point maps at the host, which the emulator sees as 10.0.2.2.
adb shell am start -n org.pingers.app.debug/org.pingers.app.MainActivity \
    --ez fake true --es map_host http://10.0.2.2:8090 --es screen SpotEditor
```

Debug builds allow plain HTTP to `10.0.2.2` and `localhost` only
(`src/debug/res/xml/network_security_config.xml`); release builds are HTTPS only.

## Open decisions

- **Who hosts the default packs**, and so `MAP_HOST`.
- **Shell network calls.** The design says the shells make no network calls (design E.1); pack
  downloads are an exception made in the shell (plain HTTPS, whole files). Moving them into the
  core would also give the relay path for free.
- **Pack granularity** for the publisher's host: city packs say more about the user than state
  packs (B.12, "Map host").

## Screenshots

From the emulator test (Pixel 9 Pro, 2026-11-13; the Central Melbourne pack served by
pingers-map-host on the host), in `docs/screenshots/maps/`: the placeholder with the download
prompt, the picker before and after downloading, "Search a place", the Spot editor and Draw
area on the real map, and the picker with the relay setting on.
