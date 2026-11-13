// SPDX-License-Identifier: AGPL-3.0-only
//! Pure helpers.

use core::time::Duration;

use crate::MAX_RECONNECT_DELAY;
use crate::live::Reconnect;

#[test]
fn reconnect_is_immediate_once_then_backs_off_to_a_minute() {
    let delays: Vec<Duration> = (1..=9).map(Reconnect::delay).collect();
    let seconds: Vec<u64> = delays.iter().map(Duration::as_secs).collect();
    assert_eq!(seconds, vec![0, 1, 2, 4, 8, 16, 32, 60, 60]);
    assert_eq!(Reconnect::delay(u32::MAX), MAX_RECONNECT_DELAY);
}
