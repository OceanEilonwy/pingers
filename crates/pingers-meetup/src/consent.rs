// SPDX-License-Identifier: AGPL-3.0-only
//! The vocabulary of exact-location consent (design B.6.1).

use pingers_crypto::AccountPublicKey;
use pingers_time::UnixSeconds;

/// Who receives this device's location key by default (`settings.location_visibility`, per
/// device with an optional per-Party override).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LocationVisibility {
    /// Nobody holds the key until this member approves their request. The default.
    #[default]
    AskEachTime,
    /// Every current and newly added member is granted automatically.
    Everyone,
    /// Requests are silently dropped; no grants are ever sent.
    Nobody,
}

/// The local allow-list for one Party: requests from these members are approved automatically.
/// Never leaves the device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlwaysAllow {
    members: Vec<AccountPublicKey>,
}

impl AlwaysAllow {
    /// The list from the `always_allow` table.
    #[must_use]
    pub const fn new(members: Vec<AccountPublicKey>) -> Self {
        Self { members }
    }

    /// Whether `member` is on the list.
    #[must_use]
    pub fn contains(&self, member: &AccountPublicKey) -> bool {
        self.members.contains(member)
    }

    /// Adds `member` ("Always allow"); a member already on the list is not added twice.
    pub fn insert(&mut self, member: AccountPublicKey) {
        if !self.contains(&member) {
            self.members.push(member);
        }
    }

    /// The members on the list, for writing back to the `always_allow` table.
    #[must_use]
    pub fn members(&self) -> &[AccountPublicKey] {
        &self.members
    }
}

/// How long a grant lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GrantScope {
    /// Until the Meetup ends (or this member leaves).
    ThisMeetup,
    /// Fifteen minutes from approval.
    FifteenMinutes,
}

/// Whether approving a request also asks the requester for theirs ("Ask to see Chloé too").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AskBack {
    /// Send a request back.
    AskBack,
    /// Do not.
    DoNotAskBack,
}

/// Whether approving a request also adds the requester to the allow-list ("Always allow").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AllowListChoice {
    /// Add them.
    AddToAllowList,
    /// Do not.
    JustThisTime,
}

/// The answer to a location request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocationAnswer {
    /// Grant the key.
    Allow {
        /// For how long.
        scope: GrantScope,
        /// Whether to ask back.
        ask_back: AskBack,
        /// Whether to add to the allow-list.
        always: AllowListChoice,
    },
    /// Send nothing; the request expires. Looks the same to the requester as not seeing it.
    NotNow,
}

/// Whether this device can see a member's exact location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExactLocation {
    /// The member's key is held.
    Shared {
        /// When the grant lapses; `None` for "until the Meetup ends".
        expires: Option<UnixSeconds>,
    },
    /// Not held: no pin, distance or bearing is shown or computed.
    Private,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserting_twice_lists_the_member_once() {
        let member = AccountPublicKey::from_bytes([3; 32]);
        let mut list = AlwaysAllow::default();
        list.insert(member);
        list.insert(member);
        assert_eq!(list.members(), &[member]);
        assert!(list.contains(&member));
    }
}
