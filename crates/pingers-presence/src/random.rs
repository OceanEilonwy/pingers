// SPDX-License-Identifier: AGPL-3.0-only
//! Uniform choices driven by the injected RNG (style guide §6).
//!
//! The draws are specified byte for byte so that `vectors/presence/` can list the randomness a
//! lease consumes and another implementation can reproduce it: each draw reads four bytes with
//! `fill_bytes`, read as a big-endian `u32`, and rejects values at or above the largest
//! multiple of the bound not above 2³² (so for a power-of-two bound nothing is rejected).

use rand_core::CryptoRng;

/// A uniform value in `0..bound`, or `None` for a zero bound.
fn uniform_below(bound: u32, rng: &mut impl CryptoRng) -> Option<u32> {
    let span = 1_u64 << 32;
    let zone = span
        .checked_div(u64::from(bound))?
        .checked_mul(u64::from(bound))?;
    loop {
        let mut bytes = [0; 4];
        rng.fill_bytes(&mut bytes);
        let draw = u32::from_be_bytes(bytes);
        if u64::from(draw) < zone {
            return draw.checked_rem(bound);
        }
    }
}

/// Shuffles `items` in place with Fisher–Yates: for `i` from the last index down to 1, swap
/// item `i` with item `j`, `j` uniform in `0..=i`.
pub(crate) fn shuffle<T>(items: &mut [T], rng: &mut impl CryptoRng) {
    for index in (1..items.len()).rev() {
        let Some(bound) = index
            .checked_add(1)
            .and_then(|bound| u32::try_from(bound).ok())
        else {
            continue;
        };
        let Some(other) = uniform_below(bound, rng).and_then(|other| usize::try_from(other).ok())
        else {
            continue;
        };
        if let Some((head, tail)) = items.split_at_mut_checked(index)
            && let (Some(item), Some(swap_with)) = (tail.first_mut(), head.get_mut(other))
        {
            core::mem::swap(item, swap_with);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FixedRng;

    #[test]
    fn draws_are_big_endian_and_reduced_modulo_the_bound() {
        let mut rng = FixedRng::new(&[0, 0, 0, 7]);
        assert_eq!(uniform_below(3, &mut rng), Some(1));
        assert!(rng.exhausted_exactly());
    }

    #[test]
    fn draws_in_the_rejection_zone_are_redrawn() {
        // 2^32 - 1 is not below 3 * 0x5555_5555, the largest multiple of 3 under 2^32.
        let mut rng = FixedRng::new(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 5]);
        assert_eq!(uniform_below(3, &mut rng), Some(2));
        assert!(rng.exhausted_exactly());
    }

    #[test]
    fn the_largest_accepted_draw_is_kept() {
        // 0xffff_fffe = 3 * 0x5555_5554 + 2 is the last value below the zone.
        let mut rng = FixedRng::new(&[0xff, 0xff, 0xff, 0xfe]);
        assert_eq!(uniform_below(3, &mut rng), Some(2));
        assert!(rng.exhausted_exactly());
    }

    #[test]
    fn power_of_two_bounds_never_reject() {
        let mut rng = FixedRng::new(&[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(uniform_below(4, &mut rng), Some(3));
        assert!(rng.exhausted_exactly());
    }

    #[test]
    fn zero_bound_has_no_value() {
        assert_eq!(uniform_below(0, &mut FixedRng::new(&[])), None);
    }

    #[test]
    fn shuffle_swaps_from_the_back() {
        // i = 3: j = 0; i = 2: j = 2 (no move); i = 1: j = 0.
        let mut rng = FixedRng::new(&[0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0]);
        let mut items = ['a', 'b', 'c', 'd'];
        shuffle(&mut items, &mut rng);
        assert_eq!(items, ['b', 'd', 'c', 'a']);
        assert!(rng.exhausted_exactly());
    }
}
