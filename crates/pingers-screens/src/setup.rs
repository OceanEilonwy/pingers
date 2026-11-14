// SPDX-License-Identifier: AGPL-3.0-only
//! First run (canvas `SetupWelcome`, `SetupBackground`): a welcome with the location
//! permission, then background location, then notifications, one step per screen. First run
//! asks for no server or code: those come with New party or an invite.

/// One first-run step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SetupStep {
    /// Welcome, and the location permission.
    Location,
    /// Location while the app is closed, with Android's required disclosure.
    Background,
    /// Notifications.
    Notifications,
}

impl SetupStep {
    /// Every step, in order.
    pub const ALL: [Self; 3] = [Self::Location, Self::Background, Self::Notifications];

    /// The step after this one, or `None` after the last.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Location => Some(Self::Background),
            Self::Background => Some(Self::Notifications),
            Self::Notifications => None,
        }
    }

    /// Where the step is among all of them, from the first.
    #[must_use]
    pub const fn position(self) -> StepPosition {
        let index = match self {
            Self::Location => 0,
            Self::Background => 1,
            Self::Notifications => 2,
        };
        StepPosition {
            index,
            count: Self::ALL.len(),
        }
    }
}

/// Where a step is among the steps: the progress dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepPosition {
    /// How many steps come before it.
    pub index: usize,
    /// How many steps there are.
    pub count: usize,
}

/// A first-run screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    /// The step.
    pub step: SetupStep,
    /// The progress dots.
    pub position: StepPosition,
}

/// The screen for a first-run step.
#[must_use]
pub const fn setup(step: SetupStep) -> Setup {
    Setup {
        step,
        position: step.position(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_run_location_background_notifications_then_end() {
        assert_eq!(SetupStep::Location.next(), Some(SetupStep::Background));
        assert_eq!(SetupStep::Background.next(), Some(SetupStep::Notifications));
        assert_eq!(SetupStep::Notifications.next(), None);
    }

    #[test]
    fn each_step_knows_its_dot() {
        let dots: Vec<_> = SetupStep::ALL
            .iter()
            .map(|step| setup(*step).position)
            .collect();
        assert_eq!(
            dots,
            [
                StepPosition { index: 0, count: 3 },
                StepPosition { index: 1, count: 3 },
                StepPosition { index: 2, count: 3 },
            ]
        );
    }
}
