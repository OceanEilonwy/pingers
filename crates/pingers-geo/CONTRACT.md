# pingers-geo contract

Implemented in full.

## Fixed (other crates build against these)

- Values: `Meters` (finite, ≥ 0), `SquareMeters`, `Bearing` (`[0, 360)`), `Coordinate`
  (validated WGS84; `Debug` redacted), `Fix { coordinate, accuracy, taken_at }` (`Debug`
  redacted).
- Free functions: `distance`, `bearing`, `exit_buffer` (`max(50 m, r/4)`),
  `entry_accuracy_limit` (`max(50 m, r/2)`).
- Fences: `Circle`, `Polygon` (`new`, `vertices`, `contains`, `distance_outside`, `area`,
  `enclosing_circle`), `Fence` (`contains`, `distance_outside`, `characteristic_radius`,
  `enclosing_circle`, `intersects_disc`); constants `MINIMUM_SPOT_RADIUS` (100 m),
  `RECOMMENDED_SPOT_RADIUS` (150 m) and `MAXIMUM_POLYGON_EXTENT` (100 km).
- Presence: `SpotPresence` (`new`, `restore`, `state`, `snapshot`, `fence`,
  `handle(PresenceInput) -> Result<PresenceChange, GeoError>`), `PresenceState`,
  `PresenceInput`, `PresenceChange`, `PresenceTransition`, `PresenceSnapshot`, and the timing
  constants `ENTRY_FIX_SPACING` (30 s), `CANDIDATE_TIMEOUT` (3 min), `INSIDE_TIMEOUT` (60 min).
- Exclusion: `ExclusionTrail` (`observe`, `status_at`, `restore`, `TRAIL_BUFFER` = 5 min) and
  `ZoneStatus`.
- OS geofences: `select_os_geofences`, `GeofenceCandidate`, `GeofenceCapacity::ANDROID` (97),
  `OsGeofence`, `GeofenceSelection`.
- `GeoError` variants.

## Supported range

- **Coordinates:** anywhere on the sphere. Distances and bearings are spherical (mean Earth
  radius 6,371,008.8 m), within about 0.5 % of the ellipsoid.
- **Polygons:** any latitude, including polygons that surround a pole, and polygons that
  straddle the antimeridian. Every vertex must be within `MAXIMUM_POLYGON_EXTENT` (100 km) of
  the first. Geometry is computed in an azimuthal equidistant projection about the polygon's
  enclosing-circle centre, so there is no seam anywhere; edges are straight lines in that
  projection (not great circles), which differ by millimetres at Spot scale. Distances to a
  polygon from points far away are approximate (within 2 % at 2,000 km),
  but containment is still correct.
- **Tolerance:** a point within 1 cm of an edge is on it (inside). Vertices within 1 cm of one
  line are degenerate; edges within 1 cm of each other touch (self-intersecting). This absorbs
  the rounding of coordinates stored to 7 decimal places.

## Implementer decides

- The projection and its precision (decided above); `Polygon::enclosing_circle` is the
  minimal circle (computed in the plane, then its radius re-measured on the sphere so it
  covers every vertex).
- Internal state kept by `SpotPresence` beyond `PresenceSnapshot` (decided below).

## Decisions

- **`r` for a polygon** in the F.2 rules is the radius of its enclosing circle (design E.2
  already uses the enclosing circle for OS regions).
- **Stale inputs are errors**: an input older than the last processed one returns
  `GeoError::StaleInput` and leaves state unchanged. Inputs at the same second are not stale.
- **Resume restarts at `Outside`**: after `Suppressed`, the Spot must be re-entered by the
  normal entry rule, so suppression never jumps straight back to `Inside`. While
  `Suppressed`, every other input is ignored (fixes still update `last_fix_at`).
- **Timeouts** are checked before each input; reaching the limit exactly has expired. The
  Candidate timeout runs from entering Candidate; the Inside timeout from the last confirming
  fix (any fix not beyond the exit buffer), OS dwell, or entry. When one input causes a
  timeout and then a transition, `handle` reports the net change.
- **Entry pair**: the first inside fix with accuracy within the limit starts the pair; a fix
  outside the fence clears it; later fixes under 30 s after the first do not restart it.
- **OS exit while Candidate** returns to `Outside` (not in the F.2 table, which only times
  Candidates out): the OS has seen the device leave before entry was confirmed.
- **OS dwell confirms entry only for circles.** A polygon's OS region is its enclosing circle
  inflated by the exit buffer, so dwelling there does not mean being inside the polygon; for a
  polygon, dwell acts as an OS enter.
- **Exit evidence**: an OS exit plus a fix strictly beyond the buffer (either order), or two
  such fixes. A fix inside the fence, an OS enter or (circles) an OS dwell clears it; a fix in
  the band between fence and buffer keeps Inside alive but leaves the evidence standing.
- **Restore** is conservative: a restored Candidate needs a fresh confirming pair, exit
  evidence is forgotten, and both the Inside timeout and the stale cut-off run from the later
  of `since` and `last_fix_at`.
- **Trail buffer**: sharing stays suppressed for 5 minutes after the newest fix whose accuracy
  circle touched a zone (the architecture's "5-minute trail buffer"; design B.11 gives only
  the intersection rule). A late, older fix never shortens it.
- **Horizon region** is omitted when every Spot fits in the OS capacity; circles are registered
  as-is, polygons as their enclosing circle inflated by the exit buffer (design E.2).
- **Contract change (2026-11-13): `GeoError::PolygonTooLarge` and `MAXIMUM_POLYGON_EXTENT`
  added** (additive; `GeoError` is `#[non_exhaustive]`). A polygon spanning hundreds of
  kilometres is not a Spot or zone, and the local projection's guarantees stop holding.
  **`GeoError::NotImplemented` removed**, with the stub module, now that nothing is stubbed;
  no other crate referred to it.
