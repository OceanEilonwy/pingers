// SPDX-License-Identifier: AGPL-3.0-only
//! Rounding floating-point measurements to the integers of the wire format.

/// The largest magnitude below which every integer is exactly representable in an `f64`.
const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;

/// `value` rounded half away from zero, or `None` if it is not finite or too large to be an
/// exact integer. The caller narrows the result with `TryFrom`.
pub(crate) fn rounded(value: f64) -> Option<i64> {
    let whole = value.round();
    if !whole.is_finite() || whole.abs() >= EXACT_INTEGER_LIMIT {
        return None;
    }
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "finite whole number with magnitude below 2^53, so the cast is exact"
    )]
    let exact = whole as i64;
    Some(exact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_half_away_from_zero() {
        assert_eq!(rounded(2.5), Some(3));
        assert_eq!(rounded(-2.5), Some(-3));
        assert_eq!(rounded(2.4), Some(2));
    }

    #[test]
    fn rejects_non_finite_values() {
        assert_eq!(rounded(f64::NAN), None);
        assert_eq!(rounded(f64::INFINITY), None);
    }

    #[test]
    fn the_exact_integer_limit_is_exclusive() {
        assert_eq!(rounded(EXACT_INTEGER_LIMIT), None);
        assert_eq!(rounded(-EXACT_INTEGER_LIMIT), None);
        assert_eq!(
            rounded(EXACT_INTEGER_LIMIT - 1.0),
            Some(9_007_199_254_740_991)
        );
    }
}
