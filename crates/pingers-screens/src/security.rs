// SPDX-License-Identifier: AGPL-3.0-only
//! Security details ("How Pingers keeps you private"). Mostly fixed text; the parts that depend
//! on this device's settings come from here.

use pingers_client::Fallback;

use crate::server::Proofs;
use crate::snapshot::{ClientSnapshot, WakePushes};

/// The Security details screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecurityDetails {
    /// Whether "No wake-up pushes" is already on, so the waking section says so instead of
    /// suggesting it.
    pub wake_pushes: WakePushes,
    /// Whether this device is on the Privacy Pass fallback, so the account section says so.
    pub proofs: Proofs,
}

/// The Security details screen.
#[must_use]
pub fn security_details(snapshot: &ClientSnapshot) -> SecurityDetails {
    SecurityDetails {
        wake_pushes: snapshot.settings.wake_pushes,
        proofs: if snapshot.fallbacks.contains(&Fallback::PrivacyPassOnly) {
            Proofs::PrivacyPassOnly
        } else {
            Proofs::MembershipProofs
        },
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};

    #[test]
    fn details_follow_the_settings_and_fallbacks() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        assert_eq!(
            security_details(&snapshot),
            SecurityDetails {
                wake_pushes: WakePushes::On,
                proofs: Proofs::MembershipProofs
            }
        );
        snapshot.settings.wake_pushes = WakePushes::NoWakePushes;
        snapshot.fallbacks = vec![Fallback::PrivacyPassOnly];
        assert_eq!(
            security_details(&snapshot),
            SecurityDetails {
                wake_pushes: WakePushes::NoWakePushes,
                proofs: Proofs::PrivacyPassOnly
            }
        );
        Ok(())
    }
}
