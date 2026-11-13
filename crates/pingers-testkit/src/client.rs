// SPDX-License-Identifier: AGPL-3.0-only
//! `pingers_client::Platform` for [`FakePlatform`] and `pingers_client::Events` for
//! [`RecordingEvents`] (feature `client`).
//!
//! The fake's inherent methods share names with the trait's but take the testkit's mirror
//! types, and inherent methods win in method-call syntax. Code that needs the trait's
//! signature calls it as `Platform::request_fix(&platform, ..)` or through `dyn Platform`.

use core::time::Duration;

use pingers_client::{
    Events, FixQuality as ClientFixQuality, LocationMode as ClientLocationMode, OsRegion, Platform,
    PlatformError, PushTokens, SecureStoreKey,
};
use pingers_gate::FocusState;
use pingers_meetup::ExactLocation;
use pingers_proto::{GroupId, LeafIndex, LocationRequestId, PartyId};
use zeroize::Zeroizing;

use crate::events::{RecordedEvent, RecordingEvents};
use crate::platform::{
    FakePlatform, FixQuality, Geofence, LocationMode, PushRegistration, SecureStoreError,
};

fn platform_error(error: SecureStoreError) -> PlatformError {
    match error {
        SecureStoreError::Unavailable => PlatformError::SecureStoreUnavailable,
    }
}

const fn fix_quality(quality: ClientFixQuality) -> FixQuality {
    match quality {
        ClientFixQuality::Balanced => FixQuality::Balanced,
        ClientFixQuality::High => FixQuality::High,
    }
}

const fn location_mode(mode: ClientLocationMode) -> LocationMode {
    match mode {
        ClientLocationMode::Idle => LocationMode::Idle,
        ClientLocationMode::Dwell => LocationMode::Dwell,
        ClientLocationMode::MeetupBackground => LocationMode::MeetupBackground,
        ClientLocationMode::MeetupForeground => LocationMode::MeetupForeground,
    }
}

impl Platform for FakePlatform {
    fn secure_store_get(
        &self,
        key: &SecureStoreKey,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, PlatformError> {
        Self::secure_store_get(self, key.as_str()).map_err(platform_error)
    }

    fn secure_store_put(&self, key: &SecureStoreKey, value: &[u8]) -> Result<(), PlatformError> {
        Self::secure_store_put(self, key.as_str(), value).map_err(platform_error)
    }

    fn set_os_geofences(&self, regions: &[OsRegion]) {
        let geofences: Vec<Geofence> = regions
            .iter()
            .map(|region| Geofence {
                id: region.id.clone(),
                region: region.region,
            })
            .collect();
        Self::set_os_geofences(self, &geofences);
    }

    fn request_fix(&self, quality: ClientFixQuality, timeout: Duration) {
        Self::request_fix(self, fix_quality(quality), timeout);
    }

    fn set_location_mode(&self, mode: ClientLocationMode) {
        Self::set_location_mode(self, location_mode(mode));
    }

    fn focus_state(&self) -> FocusState {
        self.focus()
    }

    fn schedule_wake(&self, after: Duration) {
        Self::schedule_wake(self, after);
    }

    fn push_tokens(&self) -> Option<PushTokens> {
        self.push_registration()
            .map(|PushRegistration { platform, token }| PushTokens { platform, token })
    }
}

impl Events for RecordingEvents {
    fn parties_changed(&self) {
        self.record(RecordedEvent::PartiesChanged);
    }

    fn join_requested(&self, party: PartyId) {
        self.record(RecordedEvent::JoinRequested { party });
    }

    fn meetup_started(&self, meetup: GroupId) {
        self.record(RecordedEvent::MeetupStarted { meetup });
    }

    fn meetup_updated(&self, meetup: GroupId) {
        self.record(RecordedEvent::MeetupUpdated { meetup });
    }

    fn meetup_message(&self, meetup: GroupId) {
        self.record(RecordedEvent::MeetupMessage { meetup });
    }

    fn peer_location(&self, meetup: GroupId, peer: LeafIndex) {
        self.record(RecordedEvent::PeerLocation { meetup, peer });
    }

    fn location_request(&self, meetup: GroupId, from: LeafIndex, request: LocationRequestId) {
        self.record(RecordedEvent::LocationRequest {
            meetup,
            from,
            request,
        });
    }

    fn location_access_changed(&self, meetup: GroupId, peer: LeafIndex, access: ExactLocation) {
        self.record(RecordedEvent::LocationAccessChanged {
            meetup,
            peer,
            access,
        });
    }

    fn meetup_ended(&self, meetup: GroupId) {
        self.record(RecordedEvent::MeetupEnded { meetup });
    }

    fn notice(&self, notice: pingers_client::Notice) {
        self.record(RecordedEvent::Notice(notice));
    }

    fn privacy_status_changed(&self) {
        self.record(RecordedEvent::PrivacyStatusChanged);
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use pingers_client::Notice;
    use pingers_geo::{Circle, Coordinate, GeoError, Meters};
    use pingers_proto::{PushPlatform, PushToken};

    use super::*;
    use crate::events::EventKind;
    use crate::test_clock;

    fn platform() -> (FakePlatform, Box<dyn Platform>) {
        let fake = FakePlatform::new(test_clock());
        let core: Box<dyn Platform> = Box::new(fake.clone());
        (fake, core)
    }

    #[test]
    fn secure_store_goes_through_the_fake_and_maps_a_lock() -> Result<(), PlatformError> {
        let (fake, core) = platform();
        core.secure_store_put(&SecureStoreKey::database(), b"key")?;
        assert_eq!(
            fake.secure_store_get("pingers.database-key")
                .ok()
                .flatten()
                .as_deref()
                .map(Vec::as_slice),
            Some(&b"key"[..])
        );
        fake.set_store_availability(crate::StoreAvailability::Locked);
        assert_eq!(
            core.secure_store_get(&SecureStoreKey::database()),
            Err(PlatformError::SecureStoreUnavailable)
        );
        assert_eq!(
            core.secure_store_put(&SecureStoreKey::database(), b"x"),
            Err(PlatformError::SecureStoreUnavailable)
        );
        Ok(())
    }

    #[test]
    fn geofences_keep_their_ids_and_circles() -> Result<(), GeoError> {
        let (fake, core) = platform();
        let region = Circle::new(Coordinate::new(1.0, 2.0)?, Meters::from_whole(150))?;
        core.set_os_geofences(&[OsRegion {
            id: String::from("spot-1"),
            region,
        }]);
        assert_eq!(
            fake.os_geofences(),
            [Geofence {
                id: String::from("spot-1"),
                region
            }]
        );
        Ok(())
    }

    #[test]
    fn fix_requests_keep_their_quality() {
        let (fake, core) = platform();
        core.request_fix(ClientFixQuality::Balanced, Duration::from_secs(30));
        core.request_fix(ClientFixQuality::High, Duration::from_secs(5));
        let qualities: Vec<FixQuality> = fake
            .fix_requests()
            .iter()
            .map(|request| request.quality)
            .collect();
        assert_eq!(qualities, [FixQuality::Balanced, FixQuality::High]);
    }

    #[test]
    fn every_location_mode_maps_to_its_mirror() {
        let (fake, core) = platform();
        for mode in [
            ClientLocationMode::Dwell,
            ClientLocationMode::MeetupBackground,
            ClientLocationMode::MeetupForeground,
            ClientLocationMode::Idle,
        ] {
            core.set_location_mode(mode);
        }
        assert_eq!(
            fake.location_mode_history(),
            [
                LocationMode::Dwell,
                LocationMode::MeetupBackground,
                LocationMode::MeetupForeground,
                LocationMode::Idle
            ]
        );
    }

    #[test]
    fn focus_wakes_and_push_tokens_pass_through() {
        let (fake, core) = platform();
        fake.set_focus(FocusState::Unknown);
        assert_eq!(core.focus_state(), FocusState::Unknown);
        core.schedule_wake(Duration::from_mins(15));
        assert_eq!(
            fake.pending_wake().map(|wake| wake.after),
            Some(Duration::from_mins(15))
        );
        assert_eq!(core.push_tokens(), None);
        let token = PushToken::from_bytes(b"t".to_vec());
        fake.set_push_registration(Some(PushRegistration {
            platform: PushPlatform::UnifiedPush,
            token: token.clone(),
        }));
        assert_eq!(
            core.push_tokens(),
            Some(PushTokens {
                platform: PushPlatform::UnifiedPush,
                token
            })
        );
    }

    #[test]
    fn every_events_call_is_recorded_as_its_kind() {
        let events = RecordingEvents::new();
        let core: Box<dyn Events> = Box::new(events.clone());
        let meetup = GroupId::from_bytes([3; 32]);
        let peer = LeafIndex::new(4);
        core.parties_changed();
        core.join_requested(PartyId::from_bytes([1; 16]));
        core.meetup_started(meetup);
        core.meetup_updated(meetup);
        core.meetup_message(meetup);
        core.peer_location(meetup, peer);
        core.location_request(meetup, peer, LocationRequestId::from_bytes([5; 16]));
        core.location_access_changed(meetup, peer, ExactLocation::Private);
        core.meetup_ended(meetup);
        core.notice(Notice::ClockSkew);
        core.privacy_status_changed();
        let kinds: Vec<EventKind> = events.events().iter().map(RecordedEvent::kind).collect();
        assert_eq!(
            kinds,
            [
                EventKind::PartiesChanged,
                EventKind::JoinRequested,
                EventKind::MeetupStarted,
                EventKind::MeetupUpdated,
                EventKind::MeetupMessage,
                EventKind::PeerLocation,
                EventKind::LocationRequest,
                EventKind::LocationAccessChanged,
                EventKind::MeetupEnded,
                EventKind::Notice,
                EventKind::PrivacyStatusChanged,
            ]
        );
        assert_eq!(
            events.events().get(7),
            Some(&RecordedEvent::LocationAccessChanged {
                meetup,
                peer,
                access: ExactLocation::Private
            })
        );
    }
}
