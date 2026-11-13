// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-wake's own ports: [`Device`] over the shell's [`Platform`], [`Lens`] over
//! pingers-parties, and [`Time`] over the injected clock and timer.

use core::future::{Future, ready};
use core::time::Duration;
use std::sync::{Arc, Mutex, PoisonError};

use pingers_crypto::{AccountPublicKey, AccountSigningKey, PresenceRoot};
use pingers_gate::FocusState;
use pingers_geo::Fix;
use pingers_mls::MlsContext;
use pingers_parties::{PartiesError, PartyLifecycle, PostQuantum, RosterView};
use pingers_presence::{EpochState, EpochTransition, RosterLookup as _, SpotRef};
use pingers_proto::{Epoch, KeyPackageBytes, LeafIndex, PartyId, SizeClass, SpotId};
use pingers_store::Session;
use pingers_time::{Clock, MonotonicTime, Sleep, Timer, UnixSeconds};
use pingers_wake::PortError;
use pingers_wake::ports::{
    DevicePort, FixQuality as WakeFixQuality, LocationMode as WakeLocationMode, OsRegion,
    PartyInfo, PartyView, RegionId, SpotInfo,
};
use rand_core::CryptoRng;
use zeroize::Zeroizing;

use super::records::corrupt;
use crate::ports::{FixRequest, WakeOutcome};
use crate::{ClientError, FixQuality, LocationMode, OsRegion as ClientRegion, Platform};

/// The shell as a wake sees it. Focus is read fresh from the platform; everything the wake
/// asks of the shell is collected into the [`WakeOutcome`], which the client hands on.
pub(crate) struct Device {
    platform: Arc<dyn Platform>,
    /// "Pause during Focus" is off: the gate sees Focus as off.
    ignore_focus: bool,
    outcome: Mutex<WakeOutcome>,
}

impl Device {
    pub(crate) fn new(platform: Arc<dyn Platform>, ignore_focus: bool) -> Self {
        Self {
            platform,
            ignore_focus,
            outcome: Mutex::new(WakeOutcome::default()),
        }
    }

    /// What the wake asked of the shell.
    pub(crate) fn into_outcome(self) -> WakeOutcome {
        self.outcome
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn record(&self, change: impl FnOnce(&mut WakeOutcome)) {
        change(&mut self.outcome.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

impl DevicePort for Device {
    fn focus_state(&self) -> FocusState {
        if self.ignore_focus {
            FocusState::Off
        } else {
            self.platform.focus_state()
        }
    }

    /// The platform delivers fixes through `Client::on_location`, which runs the next wake, so
    /// this wake asks for one and goes on without it.
    fn request_fix(
        &self,
        quality: WakeFixQuality,
        timeout: Duration,
    ) -> impl Future<Output = Option<Fix>> + Send {
        self.record(|outcome| {
            outcome.fix = Some(FixRequest {
                quality: fix_quality(quality),
                timeout,
            });
        });
        ready(None)
    }

    fn set_os_geofences(&self, regions: &[OsRegion]) {
        let regions = regions
            .iter()
            .map(|region| ClientRegion {
                id: region_text(region.id),
                region: region.region,
            })
            .collect();
        self.record(|outcome| outcome.geofences = Some(regions));
    }

    fn set_location_mode(&self, mode: WakeLocationMode) {
        self.record(|outcome| outcome.location_mode = Some(location_mode(mode)));
    }

    fn schedule_wake(&self, after: Duration) {
        self.record(|outcome| outcome.next_wake = Some(after));
    }
}

pub(crate) const fn fix_quality(quality: WakeFixQuality) -> FixQuality {
    match quality {
        WakeFixQuality::Balanced => FixQuality::Balanced,
        WakeFixQuality::High => FixQuality::High,
    }
}

pub(crate) const fn location_mode(mode: WakeLocationMode) -> LocationMode {
    match mode {
        WakeLocationMode::Idle => LocationMode::Idle,
        WakeLocationMode::Dwell => LocationMode::Dwell,
    }
}

const HORIZON: &str = "horizon";
const SPOT: &str = "spot:";

/// The id the shell reports back: `horizon`, or `spot:` and the Party and Spot ids in hex.
pub(crate) fn region_text(region: RegionId) -> String {
    match region {
        RegionId::Horizon => HORIZON.to_owned(),
        RegionId::Spot(spot) => format!(
            "{SPOT}{}{}",
            hex::encode(spot.party.as_bytes()),
            hex::encode(spot.spot.as_bytes())
        ),
    }
}

/// The region an id from [`region_text`] stands for.
pub(crate) fn region_id(text: &str) -> Result<RegionId, ClientError> {
    if text == HORIZON {
        return Ok(RegionId::Horizon);
    }
    let ids = text.strip_prefix(SPOT).ok_or_else(corrupt)?;
    let mut bytes = [0_u8; PartyId::LENGTH + SpotId::LENGTH];
    hex::decode_to_slice(ids, &mut bytes).map_err(|_bad| corrupt())?;
    let (party, spot) = bytes
        .split_first_chunk::<{ PartyId::LENGTH }>()
        .ok_or_else(corrupt)?;
    Ok(RegionId::Spot(SpotRef {
        party: PartyId::from_bytes(*party),
        spot: SpotId::from_bytes(spot.try_into().map_err(|_bad| corrupt())?),
    }))
}

/// One Party's keys for the wake.
struct PartyKeys {
    party: PartyId,
    epoch: Epoch,
    root: PresenceRoot,
    rosters: RosterView,
}

/// pingers-parties as a wake sees it, read from the store in one go before the wake, so the
/// view's synchronous methods do no I/O.
pub(crate) struct Lens {
    parties: Vec<PartyInfo>,
    keys: Vec<PartyKeys>,
    signing_key: AccountSigningKey,
}

impl Lens {
    /// Reads every Party, its Spots, its current presence root and its kept rosters.
    ///
    /// Interim: pingers-parties does not record when an epoch was entered or what its Commit
    /// did, so `EpochState` says the epoch began when the Party was created and was reached by
    /// an ordinary Commit. That only shortens the grace period for older epochs (fails safe).
    /// The presence root comes from pingers-mls directly: pingers-parties does not expose it.
    pub(crate) fn load(
        session: &Session,
        clock: &dyn Clock,
        rng: &mut impl CryptoRng,
    ) -> Result<Self, ClientError> {
        let Some(identity) = session.identity().load()? else {
            // No account, so no Parties: the key signs nothing.
            return Ok(Self {
                parties: Vec::new(),
                keys: Vec::new(),
                signing_key: AccountSigningKey::generate(rng),
            });
        };
        let lifecycle = PartyLifecycle::new(session, clock);
        let mut parties = Vec::new();
        let mut keys = Vec::new();
        for row in session.parties().list()? {
            let group = MlsContext::new(session, &identity.signing_key)
                .party_group(&row.group)
                .map_err(PartiesError::from)?;
            let epoch = group.epoch();
            keys.push(PartyKeys {
                party: row.id,
                epoch,
                root: group.presence_root(&row.id).map_err(PartiesError::from)?,
                rosters: lifecycle.roster_view(&row.id)?,
            });
            let knock_class = match lifecycle.settings(&row.id)?.post_quantum {
                PostQuantum::Classical => SizeClass::Bytes1024,
                PostQuantum::QuantumResistant => SizeClass::Bytes4096,
            };
            let spots = lifecycle
                .spots(&row.id)?
                .into_iter()
                .map(|spot| SpotInfo {
                    spot: spot.id,
                    fence: spot.fence,
                })
                .collect();
            parties.push(PartyInfo {
                party: row.id,
                epoch: EpochState {
                    current: epoch,
                    entered_at: row.created_at,
                    transition: EpochTransition::Other,
                },
                own_leaf: row.own_leaf,
                knock_class,
                spots,
            });
        }
        Ok(Self {
            parties,
            keys,
            signing_key: identity.signing_key,
        })
    }
}

impl PartyView for Lens {
    fn parties(&self) -> Vec<PartyInfo> {
        self.parties.clone()
    }

    fn presence_root(&self, party: PartyId, epoch: Epoch) -> Option<PresenceRoot> {
        self.keys
            .iter()
            .find(|keys| keys.party == party && keys.epoch == epoch)
            .map(|keys| PresenceRoot::from_bytes(Zeroizing::new(*keys.root.expose_bytes())))
    }

    fn signature_key(
        &self,
        party: PartyId,
        leaf: LeafIndex,
        epoch: Epoch,
    ) -> Option<AccountPublicKey> {
        self.keys
            .iter()
            .find(|keys| keys.party == party)?
            .rosters
            .signature_key(leaf, epoch)
    }

    fn signing_key(&self) -> &AccountSigningKey {
        &self.signing_key
    }

    /// WAITING(contracts): pingers-meetup-session's `knock_key_package` is async and returns
    /// the grant key the later `join` needs, while this port is synchronous and drops it. A
    /// knock follows a board match, which needs pingers-sync, so nothing reaches this yet.
    fn key_package(&self, _party: PartyId) -> Result<KeyPackageBytes, PortError> {
        Err(PortError::Unavailable)
    }
}

/// The injected clock and timer as the one value pingers-wake takes.
pub(crate) struct Time {
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) timer: Arc<dyn Timer>,
}

impl Clock for Time {
    fn now(&self) -> UnixSeconds {
        self.clock.now()
    }
}

impl Timer for Time {
    fn now(&self) -> MonotonicTime {
        self.timer.now()
    }

    fn sleep_until(&self, deadline: MonotonicTime) -> Sleep<'_> {
        self.timer.sleep_until(deadline)
    }
}
