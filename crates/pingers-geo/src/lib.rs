// SPDX-License-Identifier: AGPL-3.0-only
//! Fences, presence state machine, bearing.
//!
//! Geometry and the per-Spot presence state machine (architecture §3):
//! - WGS84 coordinates, distance and bearing ([`distance`], [`bearing`]);
//! - circle and polygon fences, containment, polygon area and validity ([`Fence`]);
//! - [`SpotPresence`]: Outside, Candidate, Inside, Suppressed, with the entry rule (two fixes
//!   30 s apart) and exit hysteresis (`max(50 m, r/4)`) of design F.2;
//! - exclusion-zone checks with the 5-minute trail buffer ([`ExclusionTrail`]);
//! - OS geofence selection: the nearest N Spots plus a horizon region
//!   ([`select_os_geofences`]).
//!
//! Pure: no I/O, no clock reads (times arrive inside inputs), no randomness.
#![forbid(unsafe_code)]

mod error;
mod exclusion;
mod fence;
mod geofence;
mod measure;
mod presence;
mod stub;

pub use error::GeoError;
pub use exclusion::{ExclusionTrail, ZoneStatus};
pub use fence::{Circle, Fence, MINIMUM_SPOT_RADIUS, Polygon, RECOMMENDED_SPOT_RADIUS};
pub use geofence::{
    GeofenceCandidate, GeofenceCapacity, GeofenceSelection, OsGeofence, select_os_geofences,
};
pub use measure::{
    Bearing, Coordinate, Fix, Meters, SquareMeters, bearing, distance, entry_accuracy_limit,
    exit_buffer,
};
pub use presence::{
    PresenceChange, PresenceInput, PresenceSnapshot, PresenceState, PresenceTransition,
    SpotPresence,
};
