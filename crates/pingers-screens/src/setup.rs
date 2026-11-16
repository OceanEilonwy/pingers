// SPDX-License-Identifier: AGPL-3.0-only
//! First run (canvas `SetupWelcome`, `SetupBackground`): a welcome with the location
//! permission, then background location, then notifications, one step per screen. First run
//! asks for no server or code: those come with New party or an invite.
//!
//! The first-phone setup link has its own step, [`SetupLink`]: the owner scans the setup page
//! of their appliance with the phone's camera, and the app makes its account with the code.

use pingers_client::ports::{SetupCheck, SetupOutcome};
use pingers_client::snapshot::Host;

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

/// The first-phone setup link's step (the owner scanned their appliance's setup page), at each
/// stage: the question, the wait, and what came of it. It comes before first run on a fresh
/// install, and over Home on one already through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupLink {
    /// Asks before making the account ("Set up", "Not now").
    Confirm {
        /// The server the account would be made on.
        server: Host,
    },
    /// "Set up" was tapped; the account is being made.
    Working {
        /// The server.
        server: Host,
    },
    /// The account was made with the link's code.
    SetUp,
    /// The account was made before; the code is not needed.
    AlreadySetUp,
    /// The server refused this code (now, or before: then it was not sent again).
    Refused,
    /// The link is for another server than the app's.
    OtherServer {
        /// The app's server.
        app: Host,
        /// The link's server.
        link: Host,
    },
    /// The build pins keys and the link's are not among them: an app build made before the
    /// appliance's pins were copied into it.
    BuildKeysDiffer {
        /// The server.
        server: Host,
    },
    /// The gateway key the relay served is not the link's: the code was not sent.
    ServerKeysDiffer {
        /// The server.
        server: Host,
    },
    /// The relay or the server did not answer; the user may try again.
    Unreachable {
        /// The server.
        server: Host,
    },
    /// The link does not decode (cut short, or changed).
    Broken,
}

/// What the step's main button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SetupLinkAction {
    /// Uses the link ("Set up").
    SetUp,
    /// Uses the link again after the server did not answer ("Try again").
    TryAgain,
    /// Leaves the step: on to first run, or Home ("Continue", "Close").
    Leave,
}

impl SetupLink {
    /// What a check of the link found, before anything was sent.
    #[must_use]
    pub fn checked(check: SetupCheck) -> Self {
        match check {
            SetupCheck::Ready { server } => Self::Confirm { server },
            SetupCheck::AlreadySetUp => Self::AlreadySetUp,
            SetupCheck::Refused => Self::Refused,
            SetupCheck::OtherServer { app, link } => Self::OtherServer { app, link },
            SetupCheck::BuildKeysDiffer { server } => Self::BuildKeysDiffer { server },
            // SetupCheck is non-exhaustive: a check this version does not know stops the step.
            _ => Self::Broken,
        }
    }

    /// What using the link came to.
    #[must_use]
    pub fn finished(outcome: SetupOutcome) -> Self {
        match outcome {
            SetupOutcome::SetUp => Self::SetUp,
            SetupOutcome::AlreadySetUp => Self::AlreadySetUp,
            SetupOutcome::Refused => Self::Refused,
            SetupOutcome::OtherServer { app, link } => Self::OtherServer { app, link },
            SetupOutcome::BuildKeysDiffer { server } => Self::BuildKeysDiffer { server },
            SetupOutcome::ServerKeysDiffer { server } => Self::ServerKeysDiffer { server },
            SetupOutcome::Unreachable { server } => Self::Unreachable { server },
            // SetupOutcome is non-exhaustive: an outcome this version does not know stops the
            // step.
            _ => Self::Broken,
        }
    }

    /// The main button's action; `None` while the account is being made (no buttons then).
    #[must_use]
    pub const fn primary(&self) -> Option<SetupLinkAction> {
        match self {
            Self::Confirm { .. } => Some(SetupLinkAction::SetUp),
            Self::Working { .. } => None,
            Self::Unreachable { .. } => Some(SetupLinkAction::TryAgain),
            Self::SetUp
            | Self::AlreadySetUp
            | Self::Refused
            | Self::OtherServer { .. }
            | Self::BuildKeysDiffer { .. }
            | Self::ServerKeysDiffer { .. }
            | Self::Broken => Some(SetupLinkAction::Leave),
        }
    }

    /// Whether a second button leaves the step without using the link ("Not now"): before the
    /// account is made, while using the link is still on offer.
    #[must_use]
    pub const fn can_put_off(&self) -> bool {
        matches!(self, Self::Confirm { .. } | Self::Unreachable { .. })
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

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

    fn host(text: &str) -> Result<Host, pingers_client::snapshot::SnapshotError> {
        Host::new(text)
    }

    #[test]
    fn a_ready_check_asks_and_the_others_stop_the_step()
    -> Result<(), pingers_client::snapshot::SnapshotError> {
        let server = host("pingers.example")?;
        let confirm = SetupLink::checked(SetupCheck::Ready {
            server: server.clone(),
        });
        assert_eq!(
            confirm,
            SetupLink::Confirm {
                server: server.clone()
            }
        );
        assert_eq!(confirm.primary(), Some(SetupLinkAction::SetUp));
        assert!(confirm.can_put_off());
        for stop in [
            SetupCheck::AlreadySetUp,
            SetupCheck::Refused,
            SetupCheck::BuildKeysDiffer {
                server: server.clone(),
            },
            SetupCheck::OtherServer {
                app: server.clone(),
                link: host("other.example")?,
            },
        ] {
            let link = SetupLink::checked(stop);
            assert_eq!(link.primary(), Some(SetupLinkAction::Leave), "{link:?}");
            assert!(!link.can_put_off(), "{link:?}");
        }
        Ok(())
    }

    #[test]
    fn only_an_unreachable_server_offers_to_try_again()
    -> Result<(), pingers_client::snapshot::SnapshotError> {
        let server = host("pingers.example")?;
        let offline = SetupLink::finished(SetupOutcome::Unreachable {
            server: server.clone(),
        });
        assert_eq!(offline.primary(), Some(SetupLinkAction::TryAgain));
        assert!(offline.can_put_off());
        for done in [
            SetupOutcome::SetUp,
            SetupOutcome::AlreadySetUp,
            SetupOutcome::Refused,
            SetupOutcome::ServerKeysDiffer {
                server: server.clone(),
            },
        ] {
            assert_eq!(
                SetupLink::finished(done).primary(),
                Some(SetupLinkAction::Leave)
            );
        }
        let working = SetupLink::Working { server };
        assert_eq!(working.primary(), None);
        assert!(!working.can_put_off());
        Ok(())
    }
}
