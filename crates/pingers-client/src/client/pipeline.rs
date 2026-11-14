// SPDX-License-Identifier: AGPL-3.0-only
//! [`Client`]'s pipeline inputs from the shell: pingers-wake. The outcome goes back to the shell
//! as is.

use pingers_geo::Fix;
use pingers_proto::PushHandle;

use super::Client;
use crate::ClientError;
use crate::ports::{ClientEvent, Foreground, GeofenceTransition, WakeInput, WakeOutcome};

impl Client {
    /// A location fix arrived.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_location(&self, fix: Fix) -> Result<(), ClientError> {
        self.wake(WakeInput::Fix(fix)).await
    }

    /// An OS geofence reported a transition.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_os_geofence(
        &self,
        region: String,
        transition: GeofenceTransition,
    ) -> Result<(), ClientError> {
        self.wake(WakeInput::Geofence { region, transition }).await
    }

    /// A scheduled background wake fell due.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_wake(&self) -> Result<(), ClientError> {
        self.wake(WakeInput::Scheduled).await
    }

    /// A push woke the device, carrying this handle.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_push(&self, handle: PushHandle) -> Result<(), ClientError> {
        self.wake(WakeInput::Push(handle)).await
    }

    /// The interruption filter changed; it is read fresh from the platform.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_dnd_changed(&self) -> Result<(), ClientError> {
        let focus = self.platform.focus_state();
        self.wake(WakeInput::FocusChanged(focus)).await
    }

    /// The app came on screen or left it.
    ///
    /// # Errors
    ///
    /// [`ClientError::Wake`].
    pub async fn on_foreground(&self, foreground: Foreground) -> Result<(), ClientError> {
        self.wake(WakeInput::Foreground(foreground)).await
    }

    async fn wake(&self, input: WakeInput) -> Result<(), ClientError> {
        let outcome = self.crates.wake.run(input).await?;
        self.apply(outcome);
        self.publish_snapshot().await.map(drop)
    }

    fn apply(&self, outcome: WakeOutcome) {
        if let Some(regions) = outcome.geofences {
            self.platform.set_os_geofences(&regions);
        }
        if let Some(mode) = outcome.location_mode {
            self.platform.set_location_mode(mode);
        }
        if let Some(fix) = outcome.fix {
            self.platform.request_fix(fix.quality, fix.timeout);
        }
        if let Some(after) = outcome.next_wake {
            self.platform.schedule_wake(after);
        }
        for event in outcome.events {
            self.emit(event);
        }
    }

    /// Records the priority FCM delivered a push at against the one it was sent with (the
    /// shell compares them on every message, design B.4.9); raises `Notice::PushDowngraded`
    /// when downgrades have just become frequent.
    ///
    /// # Errors
    ///
    /// [`ClientError::Store`] if the count cannot be stored.
    pub async fn report_push_priority(
        &self,
        original: pingers_wake::PushPriority,
        delivered: pingers_wake::PushPriority,
    ) -> Result<(), ClientError> {
        let notice = self
            .crates
            .sync
            .report_push_priority(original, delivered)
            .await?;
        if let Some(notice) = notice {
            self.emit(ClientEvent::Notice(notice));
        }
        self.publish_snapshot().await.map(drop)
    }

    fn emit(&self, event: ClientEvent) {
        let events = self.events.as_ref();
        match event {
            ClientEvent::PartiesChanged => events.parties_changed(),
            ClientEvent::JoinRequested(party) => events.join_requested(party),
            ClientEvent::MeetupStarted(meetup) => events.meetup_started(meetup),
            ClientEvent::MeetupUpdated(meetup) => events.meetup_updated(meetup),
            ClientEvent::MeetupEnded(meetup) => events.meetup_ended(meetup),
            ClientEvent::Notice(notice) => events.notice(notice),
            ClientEvent::PrivacyStatusChanged => events.privacy_status_changed(),
        }
    }
}
