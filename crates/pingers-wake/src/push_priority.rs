// SPDX-License-Identifier: AGPL-3.0-only
//! FCM's priority downgrades (design B.4.9, H.1 #12): the app compares each message's original
//! priority with the one it was delivered at, and the core keeps the last
//! [`PriorityWindow::SPAN`] high-priority messages. When [`PriorityWindow::THRESHOLD`] of them
//! came downgraded, knocks are likely to wait for a Doze maintenance window: the core raises
//! `Notice::PushDowngraded` and marks pushes downgraded in the snapshot until the window
//! clears.
//!
//! Only high-priority messages count: a message sent at normal priority (a quiet Party commit)
//! cannot be downgraded.

/// The priority FCM reports for a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PushPriority {
    /// High priority: knocks and alerts.
    High,
    /// Normal priority: housekeeping, or a downgraded high-priority message.
    Normal,
}

/// The last [`PriorityWindow::SPAN`] high-priority messages, newest in the lowest bit, and
/// how many have been seen (up to the span).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PriorityWindow {
    downgraded: u32,
    seen: u8,
}

/// What recording one message changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum PriorityChange {
    /// Nothing the user should hear about.
    Unchanged,
    /// The window just crossed the threshold: raise `Notice::PushDowngraded`.
    BecameDowngraded,
    /// The window fell back under the threshold.
    Recovered,
}

impl PriorityWindow {
    /// How many high-priority messages are remembered.
    pub const SPAN: u8 = 20;
    /// How many of them must have come downgraded: 3 of 20 is 15 %, against H.1 #12's target
    /// of under 1 %, so a stray downgrade raises nothing.
    pub const THRESHOLD: u32 = 3;

    /// A window from stored bits; bits beyond the span and a count beyond it are dropped.
    #[must_use]
    pub fn from_parts(downgraded: u32, seen: u8) -> Self {
        let seen = seen.min(Self::SPAN);
        Self {
            downgraded: downgraded & Self::mask(seen),
            seen,
        }
    }

    /// The bits to store: which of the remembered messages came downgraded.
    #[must_use]
    pub const fn downgraded_bits(self) -> u32 {
        self.downgraded
    }

    /// How many messages are remembered.
    #[must_use]
    pub const fn seen(self) -> u8 {
        self.seen
    }

    fn mask(seen: u8) -> u32 {
        1_u32
            .checked_shl(u32::from(seen))
            .map_or(u32::MAX, |bit| bit.wrapping_sub(1))
    }

    /// Whether enough of the remembered messages came downgraded.
    #[must_use]
    pub const fn is_downgraded(self) -> bool {
        self.downgraded.count_ones() >= Self::THRESHOLD
    }

    /// Records one message.
    pub fn record(&mut self, original: PushPriority, delivered: PushPriority) -> PriorityChange {
        if original != PushPriority::High {
            return PriorityChange::Unchanged;
        }
        let before = self.is_downgraded();
        let bit = u32::from(delivered != PushPriority::High);
        self.seen = self.seen.saturating_add(1).min(Self::SPAN);
        self.downgraded =
            (self.downgraded.checked_shl(1).unwrap_or(0) | bit) & Self::mask(self.seen);
        match (before, self.is_downgraded()) {
            (false, true) => PriorityChange::BecameDowngraded,
            (true, false) => PriorityChange::Recovered,
            (false, false) | (true, true) => PriorityChange::Unchanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(window: &mut PriorityWindow, downgraded: bool) -> PriorityChange {
        let delivered = if downgraded {
            PushPriority::Normal
        } else {
            PushPriority::High
        };
        window.record(PushPriority::High, delivered)
    }

    #[test]
    fn the_third_downgrade_in_twenty_raises_the_notice_once() {
        let mut window = PriorityWindow::default();
        assert_eq!(record(&mut window, true), PriorityChange::Unchanged);
        assert_eq!(record(&mut window, false), PriorityChange::Unchanged);
        assert_eq!(record(&mut window, true), PriorityChange::Unchanged);
        assert!(!window.is_downgraded());
        assert_eq!(record(&mut window, true), PriorityChange::BecameDowngraded);
        assert!(window.is_downgraded());
        assert_eq!(record(&mut window, true), PriorityChange::Unchanged);
    }

    #[test]
    fn downgrades_older_than_twenty_messages_are_forgotten() {
        let mut window = PriorityWindow::default();
        for _ in 0..3 {
            let _ = record(&mut window, true);
        }
        assert!(window.is_downgraded());
        for _ in 0..17 {
            assert_eq!(record(&mut window, false), PriorityChange::Unchanged);
        }
        // The 18th clean message pushes the first downgrade out.
        assert_eq!(record(&mut window, false), PriorityChange::Recovered);
        assert_eq!(window.seen(), PriorityWindow::SPAN);
        assert_eq!(window.downgraded_bits().count_ones(), 2);
    }

    #[test]
    fn normal_priority_messages_do_not_count() {
        let mut window = PriorityWindow::default();
        for _ in 0..30 {
            let change = window.record(PushPriority::Normal, PushPriority::Normal);
            assert_eq!(change, PriorityChange::Unchanged);
        }
        assert_eq!(window, PriorityWindow::default());
    }

    #[test]
    fn stored_parts_round_trip_and_extra_bits_are_dropped() {
        let mut window = PriorityWindow::default();
        let _ = record(&mut window, true);
        let _ = record(&mut window, false);
        let restored = PriorityWindow::from_parts(window.downgraded_bits(), window.seen());
        assert_eq!(restored, window);
        let clipped = PriorityWindow::from_parts(u32::MAX, 2);
        assert_eq!(clipped.downgraded_bits(), 0b11);
        assert_eq!(PriorityWindow::from_parts(u32::MAX, 200).seen(), 20);
        assert_eq!(
            PriorityWindow::from_parts(u32::MAX, 200).downgraded_bits(),
            0xF_FFFF
        );
    }
}
