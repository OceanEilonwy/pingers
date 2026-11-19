# pingers-tiles

The operator tool that cuts Pingers' map packs. A pack is a whole region of the
[Protomaps basemap](https://docs.protomaps.com/basemaps/downloads) (OpenStreetMap data) as
one PMTiles file. Phones download whole packs and render offline with MapLibre Native, so no
tile request ever tells anyone where a user is looking (design B.12, "Map tiles").

For each region the tool writes three files into one flat directory:

| File | What it is |
| --- | --- |
| `<stem>.pmtiles` | The pack: `pmtiles extract` of the region from a Protomaps daily build. |
| `<stem>.places.tsv` | The place-name search index read from the pack's own labels (towns, suburbs, streets, points of interest). "Search a place" in the app searches this file on the phone. |
| `<stem>.json` | The manifest: id, name, group, bbox, max zoom, build date, source build, and the size and SHA-256 of the pack and the index. |

plus a `catalog.json` listing every manifest, for plain static hosting. `pingers-map-host`
builds its own catalog from the manifests, keeping only the regions it is configured to host.

`<stem>` is the region id with `/` replaced by `.`
(`oceania/australia/victoria` → `oceania.australia.victoria.pmtiles`).

The code shared with `pingers-map-host` (PMTiles and vector-tile reading, the index, the
manifest format, region lists and budgets) is in `crates/pingers-map-packs`.

## The region list

```toml
# regions.toml
[[region]]
id = "oceania/australia/victoria"       # lowercase segments: continent/country/state/...
name = "Victoria"                       # what the app shows
group = "Australia"                     # optional: the parent's name, for grouping in the app
polygon = "regions/au-victoria.geojson" # a GeoJSON Polygon or MultiPolygon, relative to this file
max_zoom = 15                           # 0 to 15; each zoom lower makes the pack about 4x smaller

[[region]]
id = "oceania/australia/new-south-wales"
name = "New South Wales"
group = "Australia"
polygon = "regions/au-nsw.geojson"
max_zoom = 15
```

The repository ships one list, `regions/regions.toml` beside this README: Australia's states
and territories and New Zealand, with rough polygons in `regions/polygons/`. The monokulo-nodo
map host package installs it and cuts from it on demand.

### Sub-regions

Ids are paths so that large countries can be split. A country pack at zoom 15 can run to
tens of gigabytes; nobody should download all of Germany to see their own street. Split it
into states or provinces, each its own region and pack:

```toml
[[region]]
id = "europe/germany/bayern"
name = "Bayern"
group = "Germany"
polygon = "regions/de-bayern.geojson"
max_zoom = 15

[[region]]
id = "europe/germany/berlin"
name = "Berlin"
group = "Germany"
polygon = "regions/de-berlin.geojson"
max_zoom = 15
```

The app groups packs by `group` and marks the one containing the phone's position as
"This area" (worked out on the phone; the position is never sent). A host's allow-list can name
any level: `hosted = ["europe/germany"]` carries every German state. Keep the sub-region
polygons slightly overlapping at their borders, so a place on a border is in both packs.

Boundary polygons can come from OpenStreetMap administrative boundaries (for example exported
with [osm-boundaries](https://osm-boundaries.com) or `osmium`), simplified to a few hundred
points; `pmtiles extract` only needs them to pick tiles.

## Operator workflow

1. **Get `pmtiles`.** Download the `go-pmtiles` release for your platform from
   <https://github.com/protomaps/go-pmtiles/releases> and check it against the release's
   `checksums.txt`.
2. **Pick a build.** Protomaps publishes a daily planet build at
   `https://build.protomaps.com/YYYYMMDD.pmtiles` (list:
   <https://maps.protomaps.com/builds/>). Builds are removed after a while, so cut from a
   recent one.
3. **See what it costs.** Dry runs fetch only the build's directories and print each pack's
   size; with a host config the tool applies its allow-list and zoom caps and checks
   `max_disk`:

   ```sh
   pingers-tiles plan regions.toml --build https://build.protomaps.com/20261112.pmtiles \
       --host-config /etc/pingers/map-host.toml --pmtiles ./pmtiles
   ```

   ```text
   region                                    zoom        size
   oceania/australia/victoria/melbourne-cbd    15      3.1 MB
   total                                                3.1 MB
   budget                                              60.0 GB  fits
   ```

   It exits non-zero if the total is over budget. Without `--host-config`, add `--estimate`
   for the sizes; without either it only prints the `pmtiles extract` commands, to run
   elsewhere.
4. **Cut.** This refuses up front if the packs would not fit `max_disk`, then extracts each
   region (through a `.partial` file, so a failed cut leaves nothing), writes its index and
   manifest, and the catalog:

   ```sh
   pingers-tiles cut regions.toml --build https://build.protomaps.com/20261112.pmtiles \
       --out /srv/pingers-maps --host-config /etc/pingers/map-host.toml --pmtiles ./pmtiles
   ```

   If you ran the extracts yourself, `pingers-tiles publish` with the same arguments writes the
   indexes, manifests and catalog for the packs already in `--out`.
5. **Serve.** Point `pingers-map-host` at the directory (see its README), or any static host
   that answers `Range` requests. Packs are named by region, not by build, so serve them with
   a short cache lifetime unless you version the directory, or let `pingers-map-host` set
   ETags from the SHA-256.
6. **Update.** Re-run `cut` with a newer build. The manifest's `build_date` changes; the app
   shows "Map data from <date>" and an "Update available" note, and the user chooses when to
   download (updates are never automatic).

Extracting fetches only the byte ranges a region needs from the build host: the build host
sees the operator's IP and which regions are cut, never anything about users.

## Testing locally

```sh
pmtiles extract https://build.protomaps.com/20261112.pmtiles /tmp/probe.pmtiles \
    --region=regions/melbourne-cbd.geojson --maxzoom=15 --dry-run
```

The test region used for the Android work is central Melbourne
(`[[144.95,-37.825],[144.98,-37.825],[144.98,-37.805],[144.95,-37.805]]`, about 2.6 × 2.2 km)
at zoom 15: a 3.1 MB pack with about 4,800 indexed places.
