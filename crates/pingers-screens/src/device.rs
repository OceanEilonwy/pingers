// SPDX-License-Identifier: AGPL-3.0-only
//! Device settings, the Home cog (canvas `Settings`): settings for this phone only, the same in
//! every Party. Your name, quiet hours, exclusion zones, wake-ups, downloaded maps and the
//! privacy explainer; anything about a server lives in that Party's settings.
//!
//! Some of what the hub shows is the phone's, not the core's: which push app wakes Pingers and
//! how much map data is downloaded. The shell passes those in as a [`DevicePlatform`].

use crate::common::{Initials, Name};
use crate::settings::QuietHoursRow;
use crate::snapshot::{ClientSnapshot, WakePushes, ZoneTimes};

/// The name of an app on the phone, as Android labels it ("ntfy").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppLabel(String);

impl AppLabel {
    /// An app's label. Blank labels are refused: the screen then names no app.
    #[must_use]
    pub fn new(label: &str) -> Option<Self> {
        let label = label.trim();
        (!label.is_empty()).then(|| Self(label.to_owned()))
    }

    /// The label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What can wake Pingers on this phone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PushService {
    /// A UnifiedPush app, such as ntfy.
    PushApp(AppLabel),
    /// Several push apps and none chosen yet.
    NotChosen,
    /// Google's push service (Firebase), in builds that have it and with no push app.
    Google,
    /// Nothing: no push app and no Google push service.
    Nothing,
}

/// A number of bytes on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(u64);

impl Bytes {
    /// So many bytes.
    #[must_use]
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    /// The count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The map areas downloaded to this phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapsOnPhone {
    /// How many areas.
    pub areas: usize,
    /// Their total size.
    pub size: Bytes,
}

/// What the hub needs from the phone rather than the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevicePlatform {
    /// What can wake Pingers.
    pub push_service: PushService,
    /// The downloaded maps.
    pub maps: MapsOnPhone,
}

/// "Ben · Your name in every Party", or the prompt to set one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YouRow {
    /// The name, once set.
    pub name: Option<Name>,
    /// Its initials, once set.
    pub initials: Option<Initials>,
}

/// An exclusion zone in the hub's one-line summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneSummary {
    /// Its name.
    pub name: Name,
    /// When it applies.
    pub times: ZoneTimes,
}

/// The Wake-ups row: whether pushes are on, and through what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeupsSummary {
    /// "No wake-up pushes", or on.
    pub pushes: WakePushes,
    /// What wakes Pingers.
    pub service: PushService,
}

/// The device Settings screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceSettings {
    /// Your name.
    pub you: YouRow,
    /// Quiet hours.
    pub quiet_hours: QuietHoursRow,
    /// Exclusion zones.
    pub zones: Vec<ZoneSummary>,
    /// Wake-ups.
    pub wakeups: WakeupsSummary,
    /// Downloaded maps.
    pub maps: MapsOnPhone,
}

/// The device Settings screen.
#[must_use]
pub fn device_settings(snapshot: &ClientSnapshot, platform: &DevicePlatform) -> DeviceSettings {
    let privacy = crate::settings::privacy(snapshot);
    DeviceSettings {
        you: YouRow {
            name: snapshot.display_name.clone(),
            initials: snapshot.display_name.as_ref().map(Initials::of),
        },
        quiet_hours: privacy.quiet_hours,
        zones: snapshot
            .zones
            .iter()
            .map(|zone| ZoneSummary {
                name: zone.name.clone(),
                times: zone.schedule.times,
            })
            .collect(),
        wakeups: WakeupsSummary {
            pushes: snapshot.settings.wake_pushes,
            service: platform.push_service.clone(),
        },
        maps: platform.maps,
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

    use super::*;
    use crate::fixtures::{self, FixtureError};
    use crate::snapshot::Toggle;

    fn platform() -> Result<DevicePlatform, FixtureError> {
        Ok(DevicePlatform {
            push_service: PushService::PushApp(AppLabel::new("ntfy").ok_or(FixtureError::Missing)?),
            maps: MapsOnPhone {
                areas: 2,
                size: Bytes::new(246_000_000),
            },
        })
    }

    #[test]
    fn the_hub_reads_like_the_canvas() -> Result<(), FixtureError> {
        let snapshot = fixtures::snapshot()?;
        let view = device_settings(&snapshot, &platform()?);
        assert_eq!(view.you.name.as_ref().map(Name::as_str), Some("Sam"));
        assert_eq!(view.you.initials.as_ref().map(Initials::as_str), Some("SA"));
        assert_eq!(view.quiet_hours.state, Toggle::Off);
        let zones: Vec<_> = view.zones.iter().map(|zone| zone.name.as_str()).collect();
        assert_eq!(zones, ["Home", "Work"]);
        assert_eq!(view.wakeups.pushes, WakePushes::On);
        assert_eq!(view.maps.areas, 2);
        Ok(())
    }

    #[test]
    fn without_a_name_the_row_asks_for_one() -> Result<(), FixtureError> {
        let mut snapshot = fixtures::snapshot()?;
        snapshot.display_name = None;
        let view = device_settings(&snapshot, &platform()?);
        assert_eq!(
            view.you,
            YouRow {
                name: None,
                initials: None
            }
        );
        Ok(())
    }

    #[test]
    fn blank_app_labels_name_no_app() {
        assert_eq!(AppLabel::new("  "), None);
        assert_eq!(
            AppLabel::new(" ntfy ").map(|label| label.as_str().to_owned()),
            Some("ntfy".to_owned())
        );
    }
}
