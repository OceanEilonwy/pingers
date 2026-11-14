// SPDX-License-Identifier: AGPL-3.0-only
//! [`ClientSnapshot`]: the read-only state every screen is computed from.
//!
//! pingers-client defines and publishes it (architecture §3); it is re-exported here so the
//! screens and pingers-words name it where they always have. Its shared values ([`Name`],
//! [`Host`], [`Operator`], [`PartyColour`], [`Me`]) are re-exported from [`crate::common`].
//!
//! [`Name`]: crate::common::Name
//! [`Host`]: crate::common::Host
//! [`Operator`]: crate::common::Operator
//! [`PartyColour`]: crate::common::PartyColour
//! [`Me`]: crate::common::Me

pub use pingers_client::snapshot::{
    ChatEntry, ChatMessage, ChatReply, ClientSnapshot, DailyWindow, Days, EndedMeetup,
    ExclusionZone, HideTrail, Hosting, Invite, InviteLink, InviteStatus, InviteUrl, JoinRequest,
    Joining, LocationRequest, MeetPlace, MeetPointSnapshot, Meetup, MeetupPeer, MeetupSelf, Member,
    OwnPosition, Party, PartyConfig, PauseInFocus, PeerAccess, PostQuantum, PushRelay,
    PushRelayKind, QuietHoursPreference, RelayStatus, SafetyCheck, SafetyCode, ServerOperators,
    Service, Settings, Spot, Toggle, WakePushes, ZoneId, ZoneParties, ZoneSchedule, ZoneTimes,
    daily_window,
};
