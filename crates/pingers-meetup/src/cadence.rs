// SPDX-License-Identifier: AGPL-3.0-only
//! The location-frame cadence: two fixed rates, never driven by movement (design B.6.1, F.3).
//!
//! A member sends one `SealedLocation` frame per slot, real or dummy, whatever it shares and
//! however it moves, so neither the server nor other members learn anything from timing
//! except which mode the device is in:
//!
//! - [`CadenceMode::Background`]: one frame every [`BACKGROUND_FRAME_INTERVAL`] (60 s).
//! - [`CadenceMode::Live`]: one frame every [`LIVE_FRAME_INTERVAL`] (2 s), while the Meetup
//!   screen or the compass is open on this device, so peers' compasses get fresh positions.
//!
//! What the mode switch reveals: roughly when the app is in the foreground on the Meetup
//! screen. The server already sees that from the live channel's activity, and members see
//! only that this device's frames speed up, not why or towards whom.

use core::time::Duration;

use pingers_time::UnixSeconds;

use crate::MeetupError;

/// Frame interval while the app is in the background (design F.3 "Meetup, backgrounded").
pub const BACKGROUND_FRAME_INTERVAL: Duration = Duration::from_secs(60);

/// Frame interval while the Meetup screen or compass is open (design F.3 "compass open").
pub const LIVE_FRAME_INTERVAL: Duration = Duration::from_secs(2);

/// Which fixed rate location frames go out at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CadenceMode {
    /// The app is in the background, or showing another screen. The default.
    #[default]
    Background,
    /// The Meetup screen or the compass is open on this device.
    Live,
}

impl CadenceMode {
    /// The interval between frames in this mode.
    #[must_use]
    pub const fn interval(self) -> Duration {
        match self {
            Self::Background => BACKGROUND_FRAME_INTERVAL,
            Self::Live => LIVE_FRAME_INTERVAL,
        }
    }
}

/// When the next location frame is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cadence {
    mode: CadenceMode,
    next: UnixSeconds,
    last_sent: Option<UnixSeconds>,
}

impl Cadence {
    /// Background mode, with the first frame due at `start`.
    pub(crate) const fn new(start: UnixSeconds) -> Self {
        Self {
            mode: CadenceMode::Background,
            next: start,
            last_sent: None,
        }
    }

    pub(crate) const fn mode(&self) -> CadenceMode {
        self.mode
    }

    pub(crate) const fn next_due(&self) -> UnixSeconds {
        self.next
    }

    pub(crate) fn is_due(&self, now: UnixSeconds) -> bool {
        now >= self.next
    }

    /// Records a frame sent at `now` and schedules the next one on the same grid; a caller
    /// that ticked late does not get a burst of catch-up frames.
    pub(crate) fn sent(&mut self, now: UnixSeconds) -> Result<(), MeetupError> {
        let interval = self.mode.interval();
        let on_grid = add(self.next, interval)?;
        self.next = if on_grid > now {
            on_grid
        } else {
            add(now, interval)?
        };
        self.last_sent = Some(now);
        Ok(())
    }

    /// Switches mode. Going live makes a frame due at once (or one live interval after the
    /// last frame, if that was just sent); going back to the background spaces the next frame
    /// a full background interval after the last one sent.
    pub(crate) fn set_mode(
        &mut self,
        mode: CadenceMode,
        now: UnixSeconds,
    ) -> Result<(), MeetupError> {
        if mode == self.mode {
            return Ok(());
        }
        self.mode = mode;
        self.next = match (mode, self.last_sent) {
            (_, None) => self.next.min(now),
            (CadenceMode::Live, Some(last)) => self.next.min(add(last, mode.interval())?.max(now)),
            (CadenceMode::Background, Some(last)) => add(last, mode.interval())?.max(now),
        };
        Ok(())
    }
}

fn add(time: UnixSeconds, duration: Duration) -> Result<UnixSeconds, MeetupError> {
    time.checked_add(duration).ok_or(MeetupError::Overflow)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;

    fn at(seconds: u64) -> UnixSeconds {
        UnixSeconds::new(seconds)
    }

    #[test]
    fn first_frame_is_due_at_start() {
        let cadence = Cadence::new(at(100));
        assert!(!cadence.is_due(at(99)));
        assert!(cadence.is_due(at(100)));
    }

    #[test]
    fn background_frames_stay_on_a_sixty_second_grid() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.sent(at(100))?;
        assert_eq!(cadence.next_due(), at(160));
        // A tick three seconds late does not shift the grid.
        cadence.sent(at(163))?;
        assert_eq!(cadence.next_due(), at(220));
        Ok(())
    }

    #[test]
    fn a_very_late_tick_sends_one_frame_not_a_burst() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.sent(at(400))?;
        assert_eq!(cadence.next_due(), at(460));
        Ok(())
    }

    #[test]
    fn a_tick_exactly_one_interval_late_moves_to_the_next_slot() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.sent(at(160))?;
        assert_eq!(cadence.next_due(), at(220));
        Ok(())
    }

    #[test]
    fn going_live_makes_a_frame_due_now_then_every_two_seconds() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.sent(at(100))?;
        cadence.set_mode(CadenceMode::Live, at(130))?;
        assert_eq!(cadence.mode(), CadenceMode::Live);
        assert_eq!(cadence.next_due(), at(130));
        cadence.sent(at(130))?;
        assert_eq!(cadence.next_due(), at(132));
        Ok(())
    }

    #[test]
    fn going_live_just_after_a_frame_waits_one_live_interval() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.sent(at(100))?;
        cadence.set_mode(CadenceMode::Live, at(101))?;
        assert_eq!(cadence.next_due(), at(102));
        Ok(())
    }

    #[test]
    fn going_live_before_the_first_frame_keeps_it_due_at_start() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.set_mode(CadenceMode::Live, at(150))?;
        assert_eq!(cadence.next_due(), at(100));
        Ok(())
    }

    #[test]
    fn going_back_to_background_waits_a_full_interval_after_the_last_frame()
    -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.set_mode(CadenceMode::Live, at(100))?;
        cadence.sent(at(100))?;
        cadence.sent(at(102))?;
        cadence.set_mode(CadenceMode::Background, at(103))?;
        assert_eq!(cadence.next_due(), at(162));
        Ok(())
    }

    #[test]
    fn going_back_to_background_long_after_the_last_frame_is_due_now() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.set_mode(CadenceMode::Live, at(100))?;
        cadence.sent(at(100))?;
        cadence.set_mode(CadenceMode::Background, at(500))?;
        assert_eq!(cadence.next_due(), at(500));
        Ok(())
    }

    #[test]
    fn going_to_background_before_any_frame_keeps_the_first_slot() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.set_mode(CadenceMode::Live, at(90))?;
        cadence.set_mode(CadenceMode::Background, at(95))?;
        assert_eq!(cadence.next_due(), at(90));
        Ok(())
    }

    #[test]
    fn setting_the_same_mode_changes_nothing() -> Result<(), MeetupError> {
        let mut cadence = Cadence::new(at(100));
        cadence.sent(at(100))?;
        let before = cadence;
        cadence.set_mode(CadenceMode::Background, at(110))?;
        assert_eq!(cadence, before);
        Ok(())
    }

    #[test]
    fn intervals_are_sixty_and_two_seconds() {
        assert_eq!(CadenceMode::Background.interval(), Duration::from_secs(60));
        assert_eq!(CadenceMode::Live.interval(), Duration::from_secs(2));
    }
}
