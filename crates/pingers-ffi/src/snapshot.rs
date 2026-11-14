// SPDX-License-Identifier: AGPL-3.0-only
//! [`Snapshot`]: one moment of the client's state, and each screen computed from it; and
//! [`AreaEditor`], the Draw area editing model.
//!
//! Every method is one pingers-screens function and a conversion (views.rs). The app takes a
//! fresh snapshot from [`Core::snapshot`](crate::Core::snapshot) whenever an event says
//! something changed.

use std::sync::{Arc, Mutex, PoisonError};

use pingers_screens as screens;
use pingers_screens::snapshot::ClientSnapshot;

use crate::CoreError;
use crate::geo::Coordinate;
use crate::ids::{InviteId, LocationRequestId, MeetupId, MessageId, PartyId};
use crate::views::{
    AdvancedView, AnswerDraft, AskedToJoinView, ChatView, CoPresenceAlert, CompassSelection,
    CompassView, DrawAreaView, ExclusionZonesView, HomeView, InviteView, JoinRequestView,
    LetInView, LocationRequestView, MeetPointDraft, MeetPointView, MeetupEndedView, MeetupView,
    NewPartyView, OpenInviteView, PartyDraft, PartyView, PendingJoinView, PrivacyView,
    ServerSeesView, SpotDraft, SpotEditorView, YourNameView,
};

/// The device's clock, for screens that print times or count down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LocalClock {
    /// Now, in Unix seconds.
    pub now: u64,
    /// The time zone's offset from UTC now, in seconds.
    pub utc_offset_seconds: i32,
}

impl TryFrom<LocalClock> for screens::clock::LocalClock {
    type Error = CoreError;

    fn try_from(clock: LocalClock) -> Result<Self, CoreError> {
        Ok(Self {
            now: pingers_time::UnixSeconds::new(clock.now),
            offset: screens::clock::UtcOffset::from_seconds(clock.utc_offset_seconds)?,
        })
    }
}

/// The client's state at one moment. Read-only.
#[derive(Debug, uniffi::Object)]
pub struct Snapshot(Arc<ClientSnapshot>);

impl Snapshot {
    /// Wraps a snapshot from pingers-client.
    pub(crate) fn new(snapshot: Arc<ClientSnapshot>) -> Arc<Self> {
        Arc::new(Self(snapshot))
    }

    /// The client's snapshot, for the screens in other modules.
    pub(crate) fn client(&self) -> &ClientSnapshot {
        &self.0
    }
}

#[uniffi::export]
impl Snapshot {
    /// Home.
    #[must_use]
    pub fn home(&self) -> HomeView {
        screens::home::home(&self.0).into()
    }

    /// The co-presence alert, if one arrived while the app was open.
    #[must_use]
    pub fn alert(&self) -> Option<CoPresenceAlert> {
        screens::home::alert(&self.0).map(Into::into)
    }

    /// A Party.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn party(&self, id: &PartyId) -> Result<PartyView, CoreError> {
        Ok(screens::party::party(&self.0, id.try_into()?)?.into())
    }

    /// New party, for what has been typed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty operator name.
    pub fn new_party(&self, draft: &PartyDraft) -> Result<NewPartyView, CoreError> {
        Ok(screens::party::new_party(&draft.try_into()?, self.0.settings.wake_pushes).into())
    }

    /// Advanced, for what has been typed.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty operator name.
    pub fn advanced(&self, draft: &PartyDraft) -> Result<AdvancedView, CoreError> {
        Ok(screens::party::advanced(&draft.try_into()?, self.0.settings.wake_pushes).into())
    }

    /// Invite, for a Party.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn invite(&self, party: &PartyId, clock: LocalClock) -> Result<InviteView, CoreError> {
        Ok(screens::invite::invite(
            &self.0,
            party.try_into()?,
            pingers_time::UnixSeconds::new(clock.now),
        )?
        .into())
    }

    /// "Ben asked to join", for one request in a Party.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] once it was decided or withdrawn, or [`CoreError::Invalid`].
    pub fn join_request(
        &self,
        party: &PartyId,
        request: &InviteId,
        clock: LocalClock,
    ) -> Result<JoinRequestView, CoreError> {
        Ok(screens::invite::join_request(
            &self.0,
            party.try_into()?,
            request.try_into()?,
            pingers_time::UnixSeconds::new(clock.now),
        )?
        .into())
    }

    /// Open invite, shown when the user opens a link.
    #[must_use]
    pub fn open_invite(&self) -> OpenInviteView {
        screens::invite::open_invite(&self.0).into()
    }

    /// Asked to join, while this device waits to be let in.
    #[must_use]
    pub fn asked_to_join(&self) -> Option<AskedToJoinView> {
        screens::invite::asked_to_join(&self.0).map(Into::into)
    }

    /// The row at the top of Home while a join request waits; `None` once let in or withdrawn.
    #[must_use]
    pub fn pending_join(&self) -> Option<PendingJoinView> {
        screens::invite::asked_to_join(&self.0)
            .as_ref()
            .map(Into::into)
    }

    /// You're in, once this device was let in and until `Core::dismiss_join`.
    #[must_use]
    pub fn let_in(&self) -> Option<LetInView> {
        screens::invite::let_in(&self.0).map(Into::into)
    }

    /// The Spot editor, for what has been typed.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn spot_editor(
        &self,
        party: &PartyId,
        draft: &SpotDraft,
    ) -> Result<SpotEditorView, CoreError> {
        Ok(screens::spot::spot_editor(&self.0, party.try_into()?, &draft.try_into()?)?.into())
    }

    /// Exclusion zones.
    #[must_use]
    pub fn exclusion_zones(&self) -> ExclusionZonesView {
        screens::zones::exclusion_zones(&self.0).into()
    }

    /// Privacy settings.
    #[must_use]
    pub fn privacy(&self) -> PrivacyView {
        screens::settings::privacy(&self.0).into()
    }

    /// The name this device's user goes by in Parties they create or join.
    #[must_use]
    pub fn your_name(&self) -> YourNameView {
        screens::settings::your_name(&self.0).into()
    }

    /// What a Party's server sees.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn server_sees(&self, party: &PartyId) -> Result<ServerSeesView, CoreError> {
        Ok(screens::server::server_sees(&self.0, party.try_into()?)?.into())
    }

    /// The map host the user's Pingers server advertises (`map_host` in `/v1/params`), for
    /// the map host chain: after the user's override, before the publisher's default.
    #[must_use]
    pub fn server_map_host(&self) -> Option<String> {
        self.0.map_host.as_ref().map(|url| url.as_str().to_owned())
    }

    /// Who the user's Pingers server declares to run it (`server_operator` in `/v1/params`),
    /// once a reachability check has read the parameters: New party's server check.
    #[must_use]
    pub fn server_operator(&self) -> Option<String> {
        self.0
            .operators
            .as_ref()
            .map(|operators| operators.server.as_str().to_owned())
    }

    /// Who the server says runs the instance's push relay, once read: New party's push relay
    /// operator.
    #[must_use]
    pub fn push_relay_operator(&self) -> Option<String> {
        self.0
            .operators
            .as_ref()
            .and_then(|operators| operators.push_relay.as_ref())
            .map(|operator| operator.as_str().to_owned())
    }

    /// The live Meetups, newest last.
    #[must_use]
    pub fn meetups(&self) -> Vec<MeetupId> {
        self.0
            .meetups
            .iter()
            .map(|meetup| meetup.id.into())
            .collect()
    }

    /// The location requests waiting for an answer.
    #[must_use]
    pub fn location_requests(&self) -> Vec<LocationRequestId> {
        self.0
            .location_requests
            .iter()
            .map(|request| request.id.into())
            .collect()
    }

    /// A Meetup.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn meetup(&self, id: &MeetupId) -> Result<MeetupView, CoreError> {
        Ok(screens::meetup::meetup(&self.0, id.try_into()?)?.into())
    }

    /// Meetup ended, while its summary is kept.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an impossible UTC offset.
    pub fn meetup_ended(&self, clock: LocalClock) -> Result<Option<MeetupEndedView>, CoreError> {
        Ok(screens::meetup::meetup_ended(&self.0, clock.try_into()?).map(Into::into))
    }

    /// A Meetup's chat.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn chat(
        &self,
        id: &MeetupId,
        replying_to: Option<MessageId>,
        clock: LocalClock,
    ) -> Result<ChatView, CoreError> {
        let replying_to = replying_to.map(|id| (&id).try_into()).transpose()?;
        Ok(screens::chat::chat(&self.0, id.try_into()?, replying_to, clock.try_into()?)?.into())
    }

    /// Meet point, for what has been chosen.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn meet_point(
        &self,
        id: &MeetupId,
        draft: &MeetPointDraft,
    ) -> Result<MeetPointView, CoreError> {
        Ok(screens::meet_point::meet_point(&self.0, id.try_into()?, &draft.try_into()?)?.into())
    }

    /// The compass, pointing at `selected`.
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn compass(
        &self,
        id: &MeetupId,
        selected: CompassSelection,
    ) -> Result<CompassView, CoreError> {
        Ok(screens::compass::compass(&self.0, id.try_into()?, selected.into())?.into())
    }

    /// A location request, with the answer so far (`None`: the initial answer).
    ///
    /// # Errors
    ///
    /// [`CoreError::NotFound`] or [`CoreError::Invalid`].
    pub fn location_request(
        &self,
        id: &LocationRequestId,
        answer: Option<AnswerDraft>,
    ) -> Result<LocationRequestView, CoreError> {
        let answer = answer.map_or(screens::location_request::AnswerDraft::INITIAL, Into::into);
        Ok(screens::location_request::location_request(&self.0, id.try_into()?, answer)?.into())
    }
}

/// The Draw area editor (pingers-screens' point-by-point polygon model), held in Rust so the
/// app only forwards gestures.
#[derive(Debug, uniffi::Object)]
pub struct AreaEditor(Mutex<screens::draw::PolygonEditor>);

impl AreaEditor {
    /// Applies one step of the model.
    fn step(
        &self,
        change: impl FnOnce(screens::draw::PolygonEditor) -> screens::draw::PolygonEditor,
    ) {
        let mut editor = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *editor = change(core::mem::take(&mut *editor));
    }
}

fn point(index: u32) -> screens::draw::PointIndex {
    screens::draw::PointIndex::new(usize::try_from(index).unwrap_or(usize::MAX))
}

#[uniffi::export]
impl AreaEditor {
    /// An empty editor.
    #[uniffi::constructor]
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(screens::draw::PolygonEditor::new())))
    }

    /// An editor holding an existing area.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] if the vertices are not a valid polygon.
    #[uniffi::constructor]
    pub fn editing(vertices: &[Coordinate]) -> Result<Arc<Self>, CoreError> {
        let polygon = pingers_geo::Polygon::new(crate::geo::coordinates(vertices)?)?;
        Ok(Arc::new(Self(Mutex::new(
            screens::draw::PolygonEditor::editing(&polygon),
        ))))
    }

    /// A tap on the map.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an impossible coordinate.
    pub fn tap_map(&self, at: Coordinate) -> Result<(), CoreError> {
        let at = at.try_into()?;
        self.step(|editor| editor.tap_map(at));
        Ok(())
    }

    /// A tap on a point.
    pub fn tap_point(&self, index: u32) {
        self.step(|editor| editor.tap_point(point(index)));
    }

    /// A point dragged to `to`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an impossible coordinate.
    pub fn drag(&self, index: u32, to: Coordinate) -> Result<(), CoreError> {
        let to = to.try_into()?;
        self.step(|editor| editor.drag(point(index), to));
        Ok(())
    }

    /// The drag ended.
    pub fn drag_end(&self) {
        self.step(screens::draw::PolygonEditor::drag_end);
    }

    /// The "+" after point `after` tapped.
    pub fn insert_midpoint(&self, after: u32) {
        self.step(|editor| editor.insert_midpoint(point(after)));
    }

    /// Delete the selected point.
    pub fn delete_selected(&self) {
        self.step(screens::draw::PolygonEditor::delete_selected);
    }

    /// Undo.
    pub fn undo(&self) {
        self.step(screens::draw::PolygonEditor::undo);
    }

    /// Clear.
    pub fn clear(&self) {
        self.step(screens::draw::PolygonEditor::clear);
    }

    /// The screen, for a Spot called `spot_name`.
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] for an empty name.
    pub fn view(&self, spot_name: &str) -> Result<DrawAreaView, CoreError> {
        let name = screens::common::Name::new(spot_name)?;
        let editor = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(editor.view(&name).into())
    }

    /// The finished area, for "Use this area".
    ///
    /// # Errors
    ///
    /// [`CoreError::Invalid`] while the shape is open or not valid.
    pub fn polygon(&self) -> Result<Vec<Coordinate>, CoreError> {
        let editor = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(editor
            .polygon()?
            .vertices()
            .iter()
            .copied()
            .map(Into::into)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::{AreaEditor, Snapshot};
    use crate::CoreError;
    use crate::views::{CompassSelection, PeerLocation, Sharing, TimeOfDay, ZoneDays, ZoneTimes};
    use pingers_screens::fixtures;

    fn snapshot() -> Result<std::sync::Arc<Snapshot>, CoreError> {
        let snapshot = fixtures::snapshot().map_err(|error| CoreError::Invalid {
            reason: format!("{error:?}"),
        })?;
        Ok(Snapshot::new(std::sync::Arc::new(snapshot)))
    }

    #[test]
    fn home_shows_the_pending_join_with_the_codes_first_group() -> Result<(), CoreError> {
        let pending = snapshot()?
            .pending_join()
            .ok_or_else(|| CoreError::invalid("no pending join"))?;
        assert_eq!(pending.title, "Waiting to be let in");
        assert_eq!(pending.code_start, "4821");
        Ok(())
    }

    #[test]
    fn join_screens_carry_their_codes_and_words() -> Result<(), CoreError> {
        let asked = snapshot()?
            .asked_to_join()
            .ok_or_else(|| CoreError::invalid("not asked"))?;
        assert_eq!(asked.code, "4821 0937 5512");
        assert_eq!(asked.withdraw, "Withdraw request");
        assert_eq!(
            snapshot()?
                .open_invite()
                .join_as
                .map(|join_as| join_as.title)
                .as_deref(),
            Some("You'll join as Sam")
        );
        let request = snapshot()?.join_request(
            &fixtures::climbing_crew().into(),
            &fixtures::ben_request().into(),
            super::LocalClock {
                now: fixtures::at(19, 30).get(),
                utc_offset_seconds: 0,
            },
        )?;
        assert_eq!(request.id, fixtures::ben_request().into());
        assert_eq!(request.let_in, "Codes match · Let Ben in");
        let let_in_snapshot = fixtures::let_in_snapshot().map_err(|error| CoreError::Invalid {
            reason: format!("{error:?}"),
        })?;
        let let_in = Snapshot::new(std::sync::Arc::new(let_in_snapshot))
            .let_in()
            .ok_or_else(|| CoreError::invalid("not let in"))?;
        assert_eq!(let_in.party, fixtures::climbing_crew().into());
        assert_eq!(let_in.notification_title, "Ana let you in");
        assert_eq!(let_in.preview.len(), 3);
        Ok(())
    }

    #[test]
    fn home_converts_the_canvas_example() -> Result<(), CoreError> {
        let home = snapshot()?.home();
        assert_eq!(home.sharing.party_count, 3);
        assert!(matches!(home.sharing.sharing, Sharing::On));
        let live = home
            .live_meetup
            .ok_or_else(|| CoreError::invalid("no meetup"))?;
        assert_eq!(live.party_name, "Climbing crew");
        assert_eq!(live.spot_name, "Riverside Park");
        Ok(())
    }

    #[test]
    fn meetup_and_compass_keep_ids_and_distances() -> Result<(), CoreError> {
        let snapshot = snapshot()?;
        let id = crate::ids::MeetupId::from(fixtures::meetup_id());
        let meetup = snapshot.meetup(&id)?;
        assert_eq!(meetup.id, id);
        let shared = meetup
            .members
            .iter()
            .find(|member| matches!(member.location, PeerLocation::Shared { .. }))
            .ok_or_else(|| CoreError::invalid("nobody shares"))?;
        assert_eq!(shared.person.name, "Ben");
        let compass = snapshot.compass(
            &id,
            CompassSelection::Member {
                id: fixtures::ben().into(),
            },
        )?;
        assert!(compass.reading.is_some());
        Ok(())
    }

    #[test]
    fn settings_carry_the_times_and_name_an_editor_starts_from() -> Result<(), CoreError> {
        let snapshot = snapshot()?;
        let privacy = snapshot.privacy();
        assert_eq!(
            (privacy.quiet_hours_from, privacy.quiet_hours_to),
            (
                TimeOfDay {
                    hour: 23,
                    minute: 0
                },
                TimeOfDay { hour: 7, minute: 0 }
            )
        );
        let zones = snapshot.exclusion_zones().zones;
        let [home, work] = zones.as_slice() else {
            return Err(CoreError::invalid("two zones"));
        };
        assert_eq!(home.times, ZoneTimes::Always);
        assert_eq!(
            work.times,
            ZoneTimes::Window {
                days: ZoneDays::Weekdays,
                from: TimeOfDay { hour: 8, minute: 0 },
                to: TimeOfDay {
                    hour: 18,
                    minute: 0
                },
            }
        );
        // The drawn zone's row carries the same vertices as its fence, so the map draws it.
        let crate::Fence::Polygon { vertices } = &work.fence else {
            return Err(CoreError::invalid("a drawn zone"));
        };
        assert_eq!(
            work.shape,
            crate::ZoneShape::Area {
                points: vertices.clone()
            }
        );
        let name = snapshot.your_name();
        assert_eq!(name.name.as_deref(), Some("Sam"));
        assert_eq!(name.title, "Your name");
        Ok(())
    }

    #[test]
    fn unknown_ids_are_not_found() -> Result<(), CoreError> {
        let missing = crate::ids::PartyId {
            hex: "00".repeat(16),
        };
        assert!(matches!(
            snapshot()?.party(&missing),
            Err(CoreError::NotFound { .. })
        ));
        Ok(())
    }

    #[test]
    fn the_area_editor_closes_a_triangle() -> Result<(), CoreError> {
        let editor = AreaEditor::new();
        let at = |north: f64, east: f64| crate::geo::Coordinate {
            latitude: 51.5 + north,
            longitude: -0.1 + east,
        };
        editor.tap_map(at(0.0, 0.0))?;
        editor.tap_map(at(0.001, 0.0))?;
        editor.tap_map(at(0.0, 0.001))?;
        editor.tap_point(0);
        let view = editor.view("Park")?;
        assert!(view.closed);
        assert!(view.can_use);
        assert_eq!(editor.polygon()?.len(), 3);
        Ok(())
    }
}
