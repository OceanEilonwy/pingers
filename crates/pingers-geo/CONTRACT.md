# pingers-geo contract

## Fixed (other crates build against these)

- Values: `Meters` (finite, ≥ 0), `SquareMeters`, `Bearing` (`[0, 360)`), `Coordinate`
  (validated WGS84; `Debug` redacted), `Fix { coordinate, accuracy, taken_at }` (`Debug`
  redacted).
- Free functions: `distance`, `bearing`, `exit_buffer` (`max(50 m, r/4)`),
  `entry_accuracy_limit` (`max(50 m, r/2)`). Implemented.
- Fences: `Circle` (implemented), `Polygon` (`new`, `vertices`, `contains`,
  `distance_outside`, `area`, `enclosing_circle`), `Fence` (`contains`, `distance_outside`,
  `characteristic_radius`, `enclosing_circle`, `intersects_disc`); constants
  `MINIMUM_SPOT_RADIUS` (100 m) and `RECOMMENDED_SPOT_RADIUS` (150 m).
- Presence: `SpotPresence` (`new`, `restore`, `state`, `snapshot`, `fence`,
  `handle(PresenceInput) -> Result<PresenceChange, GeoError>`), `PresenceState`,
  `PresenceInput`, `PresenceChange`, `PresenceTransition`, `PresenceSnapshot`, and the timing
  constants `ENTRY_FIX_SPACING` (30 s), `CANDIDATE_TIMEOUT` (3 min), `INSIDE_TIMEOUT` (60 min).
- Exclusion: `ExclusionTrail` (`observe`, `status_at`, `restore`, `TRAIL_BUFFER` = 5 min) and
  `ZoneStatus`. Implemented.
- OS geofences: `select_os_geofences`, `GeofenceCandidate`, `GeofenceCapacity::ANDROID` (97),
  `OsGeofence`, `GeofenceSelection`. Implemented.
- `GeoError` variants.

## Still to implement

- `Polygon` (it holds an uninhabited `unimplemented` field so its infallible methods compile;
  replace it with real state): validity (≥ 3 vertices, no self-intersection, non-zero area),
  containment and distance to edges in a local ENU projection, area, enclosing circle.
- `SpotPresence::handle`: the F.2 table in the type's doc comment.
- Property tests for containment and hysteresis (style guide §7).

## Implementer decides

- The ENU projection and its precision; whether `Polygon::enclosing_circle` is the minimal
  circle or a bound no more than 10 % larger.
- Internal state kept by `SpotPresence` beyond `PresenceSnapshot` (for example the first
  candidate fix), as long as `snapshot`/`restore` round-trip the behaviour that matters across
  a restart.

## Decisions

- **`r` for a polygon** in the F.2 rules is the radius of its enclosing circle (design E.2
  already uses the enclosing circle for OS regions).
- **Stale inputs are errors**: an input older than the last processed one returns
  `GeoError::StaleInput` and leaves state unchanged.
- **Resume restarts at `Outside`**: after `Suppressed`, the Spot must be re-entered by the
  normal entry rule, so suppression never jumps straight back to `Inside`.
- **Trail buffer**: sharing stays suppressed for 5 minutes after the last fix whose accuracy
  circle touched a zone (the architecture's "5-minute trail buffer"; design B.11 gives only
  the intersection rule).
- **Horizon region** is omitted when every Spot fits in the OS capacity; circles are registered
  as-is, polygons as their enclosing circle inflated by the exit buffer (design E.2).
- Distances are spherical (mean Earth radius); fine at Spot scale.
