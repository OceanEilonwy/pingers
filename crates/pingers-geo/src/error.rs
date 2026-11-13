// SPDX-License-Identifier: AGPL-3.0-only
//! The crate's error type.

/// Why a geometric value or presence input was rejected. Never carries coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GeoError {
    /// Latitude outside ±90° or not a number.
    #[error("latitude out of range")]
    LatitudeOutOfRange,
    /// Longitude outside ±180° or not a number.
    #[error("longitude out of range")]
    LongitudeOutOfRange,
    /// A length that must be finite and not negative was not.
    #[error("length must be finite and not negative")]
    InvalidLength,
    /// A circle's radius must be greater than zero.
    #[error("radius must be positive")]
    ZeroRadius,
    /// A polygon needs at least three vertices.
    #[error("polygon has fewer than three vertices")]
    TooFewVertices,
    /// A polygon's edges cross each other.
    #[error("polygon is self-intersecting")]
    SelfIntersecting,
    /// A polygon encloses no area (for example, all vertices on one line).
    #[error("polygon has no area")]
    Degenerate,
    /// A polygon spans more than `MAXIMUM_POLYGON_EXTENT`.
    #[error("polygon is too large")]
    PolygonTooLarge,
    /// A bearing outside `[0, 360)` degrees or not a number.
    #[error("bearing out of range")]
    BearingOutOfRange,
    /// A presence input is timestamped before the last input the state machine processed.
    /// The caller drops it; state is unchanged.
    #[error("presence input is older than the last one processed")]
    StaleInput,
}
