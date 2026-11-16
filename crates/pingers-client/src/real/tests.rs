// SPDX-License-Identifier: AGPL-3.0-only
//! The adapters against a real store: the client records round-trip, region ids round-trip,
//! and a wake confirmed inside a Spot ends at pingers-sync's stub with `PortFailed`.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_crypto::{AccountSigningKey, account_id};
use pingers_gate::{FocusState, LocationSharing, MinuteOfDay, PauseState};
use pingers_geo::{
    Circle, Coordinate, Fence, Fix, GeoError, Meters, Polygon, PresenceSnapshot, PresenceState,
};
use pingers_meetup::LocationVisibility;
use pingers_presence::SpotRef;
use pingers_proto::{
    Epoch, KeyConfigHash, LeafIndex, Lease, MailboxId, PartyId, RecordRandomness, ServerId, SpotId,
};
use pingers_store::{Database, DatabaseKey, Identity, Session};
use pingers_time::{Bucket, Clock as _, ManualClock, UnixSeconds};
use pingers_wake::ports::{PartyView as _, RegionId};
use pingers_wake::{
    DecidedKnock, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, PeerActivity, PortError,
    RETRY_AFTER, RealRecord, SentKnock, SpotSnapshot, WakeEnding, WakeSnapshot, WakeStep,
    WakeTrigger,
};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

use super::meetups::Meetups;
use super::parties::Parties;
use super::sync::Net;
use super::wake::Wake;
use super::wake_ports::{Lens, region_id, region_text};
use super::{Backend, Outside, wake_codec};
use crate::ports::{
    AllowListChange, JoinDecision, PartiesPort as _, PartyDraft, SettingChange, SpotDraft,
    SyncPort as _, WakePort as _,
};
use crate::snapshot::{
    DailyWindow, Days, ExclusionZone, HideTrail, Name, PartyColour, PauseInFocus, PostQuantum,
    QuietHoursPreference, Toggle, ZoneId, ZoneParties, ZoneSchedule, ZoneTimes,
};
use crate::{
    ClientError, FixQuality, LocationMode, OsRegion, Platform, PlatformError, PushTokens,
    SecureStoreKey,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Store {
    path: PathBuf,
    backend: Arc<Backend>,
    clock: ManualClock,
}

impl Drop for Store {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

/// A store with an account, under a manual clock and seeded randomness.
fn store() -> Result<Store, Box<dyn std::error::Error>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "pingers-client-adapters-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    drop(std::fs::remove_dir_all(&path));
    std::fs::create_dir_all(&path)?;
    let database = Database::open(
        &path.join("pingers.db"),
        DatabaseKey::from_bytes(Zeroizing::new([5; 32])),
    )?;
    let signing_key = AccountSigningKey::generate(&mut ChaCha20Rng::seed_from_u64(7));
    database.session()?.identity().save(&Identity {
        account: account_id(&signing_key.public_key()),
        signing_key,
        server: ServerId::new("https://pingers.test".to_owned())?,
        server_pin: KeyConfigHash::from_bytes([1; 32]),
    })?;
    let clock = ManualClock::new(UnixSeconds::new(1_800_000_000));
    let seeds = Arc::new(AtomicU64::new(0));
    let backend = Arc::new(Backend {
        database: Arc::new(database),
        outside: Outside {
            prover_self_test: pingers_sync::SelfTestPlan::new(
                core::num::NonZeroU8::MIN,
                core::time::Duration::from_millis(1_500),
            ),
            clock: Arc::new(clock.clone()),
            timer: Arc::new(clock.clone()),
            rng: Arc::new(move || {
                Box::new(ChaCha20Rng::seed_from_u64(
                    seeds.fetch_add(1, Ordering::Relaxed),
                ))
            }),
        },
        transport: None,
    });
    Ok(Store {
        path,
        backend,
        clock,
    })
}

async fn session<T: Send + 'static>(
    store: &Store,
    job: impl FnOnce(&Session) -> Result<T, ClientError> + Send + 'static,
) -> Result<T, ClientError> {
    store
        .backend
        .store(move |session, _clock, _rng| job(session))
        .await
}

fn wake(store: &Store) -> Wake {
    Wake::new(
        Arc::clone(&store.backend),
        Arc::new(Quiet),
        Arc::new(Meetups::new(Arc::clone(&store.backend), offline(store))),
        offline(store),
    )
}

/// pingers-sync with no instance: every network call is unavailable.
fn offline(store: &Store) -> Arc<Net> {
    Arc::new(Net::new(
        Arc::clone(&store.backend),
        None,
        super::push::Learned::new(None, None),
    ))
}

/// A platform with Focus on that ignores every request.
struct Quiet;

impl Platform for Quiet {
    fn secure_store_get(
        &self,
        _key: &SecureStoreKey,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, PlatformError> {
        Ok(None)
    }
    fn secure_store_put(&self, _key: &SecureStoreKey, _value: &[u8]) -> Result<(), PlatformError> {
        Ok(())
    }
    fn set_os_geofences(&self, _regions: &[OsRegion]) {}
    fn request_fix(&self, _quality: FixQuality, _timeout: Duration) {}
    fn set_location_mode(&self, _mode: LocationMode) {}
    fn focus_state(&self) -> FocusState {
        FocusState::On
    }
    fn schedule_wake(&self, _after: Duration) {}
    fn push_tokens(&self) -> Option<PushTokens> {
        None
    }
}

fn spot_ref(seed: u8) -> SpotRef {
    SpotRef {
        party: PartyId::from_bytes([seed; 16]),
        spot: SpotId::from_bytes([seed.wrapping_add(1); 16]),
    }
}

fn presence(spot: u8, state: PresenceState, since: u64) -> SpotSnapshot {
    SpotSnapshot {
        spot: spot_ref(spot),
        presence: PresenceSnapshot {
            state,
            since: UnixSeconds::new(since),
            last_fix_at: (since == 10).then_some(UnixSeconds::new(since.saturating_add(1))),
            confirmed_at: (state == PresenceState::Inside).then_some(UnixSeconds::new(since)),
        },
    }
}

#[tokio::test]
async fn a_full_wake_snapshot_round_trips_through_the_store() -> TestResult {
    let store = store()?;
    let mut slots = [LeaseSlot {
        randomness: RecordRandomness::from_bytes([2; 16]),
        real: None,
    }; Lease::SLOTS];
    if let Some(slot) = slots.last_mut() {
        *slot = LeaseSlot {
            randomness: RecordRandomness::from_bytes([3; 16]),
            real: Some(RealRecord {
                spot: spot_ref(1),
                epoch: Epoch::new(4),
                knock_mailbox: MailboxId::from_bytes([9; 16]),
            }),
        };
    }
    let knock = |seed: u8, status| SentKnock {
        spot: spot_ref(seed),
        epoch: Epoch::new(u64::from(seed)),
        knock_mailbox: MailboxId::from_bytes([seed; 16]),
        matched: RecordRandomness::from_bytes([seed.wrapping_add(1); 16]),
        matched_start: Bucket::containing(UnixSeconds::new(u64::from(seed).saturating_mul(900))),
        reply_mailbox: MailboxId::from_bytes([seed.wrapping_add(2); 16]),
        sent_at: UnixSeconds::new(u64::from(seed)),
        status,
    };
    let snapshot = WakeSnapshot {
        spots: vec![
            presence(1, PresenceState::Inside, 10),
            presence(5, PresenceState::Suppressed, 12),
            presence(6, PresenceState::Candidate, 13),
            presence(7, PresenceState::Outside, 14),
        ],
        last_in_zone: Some(UnixSeconds::new(20)),
        leases: vec![LiveLease {
            start: Bucket::from_index(1_000)?,
            slots,
        }],
        sent_knocks: vec![
            knock(4, KnockStatus::Waiting),
            knock(8, KnockStatus::Answered),
        ],
        decided_knocks: vec![DecidedKnock {
            reply_mailbox: MailboxId::from_bytes([11; 16]),
            at: UnixSeconds::new(40),
        }],
        last_upload: Some(UnixSeconds::new(50)),
        epochs: vec![KnownEpoch {
            party: PartyId::from_bytes([12; 16]),
            epoch: Epoch::new(8),
        }],
        peer_activity: vec![PeerActivity {
            spot: spot_ref(3),
            at: UnixSeconds::new(60),
        }],
    };
    let saved = snapshot.clone();
    let loaded = session(&store, move |session| {
        let empty = wake_codec::load(session)?;
        wake_codec::save(session, &saved)?;
        Ok((empty, wake_codec::load(session)?))
    })
    .await?;
    assert_eq!(loaded, (WakeSnapshot::default(), snapshot));
    Ok(())
}

#[test]
fn region_ids_round_trip_and_junk_is_refused() -> TestResult {
    for region in [RegionId::Horizon, RegionId::Spot(spot_ref(4))] {
        assert_eq!(region_id(&region_text(region))?, region);
    }
    let short = format!("spot:{}", "00".repeat(31));
    for junk in [
        "",
        "horizons",
        "spot:",
        "spot:zz",
        "place:00",
        short.as_str(),
    ] {
        assert!(region_id(junk).is_err(), "{junk:?}");
    }
    Ok(())
}

fn zones(window: DailyWindow) -> Result<Vec<ExclusionZone>, Box<dyn std::error::Error>> {
    let circle = |latitude, name| -> Result<ExclusionZone, Box<dyn std::error::Error>> {
        Ok(ExclusionZone {
            id: ZoneId::new(1),
            name: Name::new(name)?,
            fence: Fence::Circle(Circle::new(
                Coordinate::new(latitude, 2.0)?,
                Meters::new(80.0)?,
            )?),
            schedule: ZoneSchedule::ALWAYS,
        })
    };
    let windowed = |days, latitude, name| -> Result<ExclusionZone, Box<dyn std::error::Error>> {
        Ok(ExclusionZone {
            schedule: ZoneSchedule {
                times: ZoneTimes::Window { days, window },
                parties: ZoneParties::All,
            },
            ..circle(latitude, name)?
        })
    };
    Ok(vec![
        circle(1.0, "Home")?,
        ExclusionZone {
            id: ZoneId::new(2),
            name: Name::new("Office")?,
            fence: Fence::Polygon(Polygon::new(vec![
                Coordinate::new(0.0, 0.0)?,
                Coordinate::new(0.0, 0.001)?,
                Coordinate::new(0.001, 0.001)?,
            ])?),
            schedule: ZoneSchedule {
                times: ZoneTimes::Window {
                    days: Days::Weekdays,
                    window,
                },
                parties: ZoneParties::Only(vec![PartyId::from_bytes([3; 16])]),
            },
        },
        windowed(Days::Weekends, 3.0, "Gym")?,
        windowed(Days::EveryDay, 4.0, "Club")?,
    ])
}

#[tokio::test]
async fn settings_and_zones_round_trip_through_the_store() -> TestResult {
    let store = store()?;
    let wake = wake(&store);
    let first = wake.section().await?;
    assert_eq!(first.settings.location_sharing, LocationSharing::On);
    assert_eq!(first.settings.pause_in_focus, PauseInFocus::On);
    assert_eq!(first.settings.hide_trail, HideTrail::On);
    assert_eq!(first.settings.quiet_hours.state, Toggle::Off);
    assert_eq!(
        first.settings.quiet_hours.window,
        DailyWindow::new(MinuteOfDay::new(22 * 60)?, MinuteOfDay::new(7 * 60)?)?
    );
    assert_eq!(first.settings.focus, FocusState::On);
    assert!(first.zones.is_empty());
    assert_eq!(
        first.gate,
        pingers_gate::GateDecision::Suppress(pingers_gate::SuppressReason::Focus),
        "Focus pauses sharing by default"
    );

    let quiet = QuietHoursPreference {
        state: Toggle::On,
        window: DailyWindow::new(MinuteOfDay::new(60)?, MinuteOfDay::new(120)?)?,
    };
    let zones = zones(quiet.window)?;
    for change in [
        SettingChange::LocationSharing(LocationSharing::Off),
        SettingChange::Pause(PauseState::Paused),
        SettingChange::PauseInFocus(PauseInFocus::Off),
        SettingChange::QuietHours(quiet),
        SettingChange::HideTrail(HideTrail::Off),
        SettingChange::Visibility(LocationVisibility::Nobody),
        SettingChange::Zones(zones.clone()),
    ] {
        wake.change_setting(change).await?;
    }
    let after = wake.section().await?;
    assert_eq!(after.settings.location_sharing, LocationSharing::Off);
    assert_eq!(after.settings.pause_in_focus, PauseInFocus::Off);
    assert_eq!(after.settings.quiet_hours, quiet);
    assert_eq!(after.settings.hide_trail, HideTrail::Off);
    assert_eq!(after.settings.visibility, LocationVisibility::Nobody);
    assert_eq!(after.zones, zones);

    wake.change_setting(SettingChange::Visibility(LocationVisibility::Everyone))
        .await?;
    wake.change_setting(SettingChange::LocationSharing(LocationSharing::On))
        .await?;
    wake.change_setting(SettingChange::Pause(PauseState::NotPaused))
        .await?;
    let shared = wake.section().await?;
    assert_eq!(shared.settings.visibility, LocationVisibility::Everyone);
    assert_eq!(
        shared.gate,
        pingers_gate::GateDecision::Share,
        "Focus is ignored and 00:00 UTC is outside quiet hours"
    );
    Ok(())
}

#[tokio::test]
async fn a_wake_confirmed_inside_a_spot_ends_at_the_sync_stub() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    let party = parties
        .create_party(PartyDraft {
            name: Name::new("Crew")?,
            my_name: Name::new("Sam")?,
            colour: PartyColour::Plum,
            post_quantum: PostQuantum::Off,
        })
        .await?;
    let centre = Coordinate::new(48.8584, 2.2945)?;
    parties
        .save_spot(
            party,
            SpotDraft {
                id: None,
                name: Name::new("Tower")?,
                fence: Fence::Circle(Circle::new(centre, Meters::new(150.0)?)?),
            },
        )
        .await?;
    let wake = wake(&store);
    wake.change_setting(SettingChange::PauseInFocus(PauseInFocus::Off))
        .await?;
    let fix = |at| -> Result<Fix, GeoError> {
        Ok(Fix {
            coordinate: centre,
            accuracy: Meters::new(10.0)?,
            taken_at: at,
        })
    };

    let (first, first_outcome) = wake
        .pipeline(WakeTrigger::Fix(fix(store.clock.now())?))
        .await?;
    assert!(first.transitions.len() == 1, "the Spot became a candidate");
    assert_ne!(first_outcome.location_mode, Some(LocationMode::Dwell));

    store.clock.advance(Duration::from_secs(31));
    let (second, outcome) = wake
        .pipeline(WakeTrigger::Fix(fix(store.clock.now())?))
        .await?;
    assert_eq!(second.decision, pingers_gate::GateDecision::Share);
    assert_eq!(
        second.ending,
        WakeEnding::PortFailed {
            step: WakeStep::GroupLogs,
            error: PortError::Unavailable,
        }
    );
    assert_eq!(outcome.location_mode, Some(LocationMode::Dwell));
    assert_eq!(outcome.next_wake, Some(RETRY_AFTER));
    assert!(outcome.events.is_empty());

    let stored = session(&store, wake_codec::load).await?;
    assert_eq!(
        stored.spots.first().map(|spot| spot.presence.state),
        Some(PresenceState::Inside)
    );
    assert!(
        first_outcome.geofences.is_some() || outcome.geofences.is_some(),
        "the Spot's geofence reaches the shell"
    );

    // The party view hands out the current epoch's root and roster, nothing else.
    let (root, wrong_epoch, own_key, stranger, no_party) = session(&store, move |session| {
        let clock = ManualClock::new(UnixSeconds::new(1_800_000_000));
        let lens = Lens::load(session, &clock, &mut ChaCha20Rng::seed_from_u64(3))?;
        let info = lens
            .parties()
            .first()
            .cloned()
            .ok_or_else(super::records::corrupt)?;
        let epoch = info.epoch.current;
        let other = Epoch::new(epoch.get().saturating_add(1));
        Ok((
            lens.presence_root(party, epoch).is_some(),
            lens.presence_root(party, other).is_some(),
            lens.signature_key(party, info.own_leaf, epoch)
                == Some(lens.signing_key().public_key()),
            lens.signature_key(party, LeafIndex::new(9), epoch)
                .is_some(),
            lens.presence_root(PartyId::from_bytes([0; 16]), epoch)
                .is_some(),
        ))
    })
    .await?;
    assert!(root && own_key, "this Party's root and this device's key");
    assert!(!wrong_epoch && !stranger && !no_party);
    Ok(())
}

#[tokio::test]
async fn join_decisions_and_withdrawals_need_a_request() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    let draft = PartyDraft {
        name: Name::new("Crew")?,
        my_name: Name::new("Sam")?,
        colour: PartyColour::Blue,
        post_quantum: PostQuantum::Off,
    };
    let party = parties.create_party(draft).await?;
    // Nobody asked to join, and this device asked nothing.
    let unknown = [
        parties
            .decide_join(party, MailboxId::from_bytes([1; 16]), JoinDecision::Admit)
            .await,
        parties
            .decide_join(party, MailboxId::from_bytes([1; 16]), JoinDecision::Refuse)
            .await,
        parties.withdraw_join().await,
    ];
    for result in unknown {
        assert!(matches!(
            result,
            Err(ClientError::Parties(
                pingers_parties::PartiesError::UnknownJoin
            ))
        ));
    }
    // Nothing to show while no join is under way.
    assert_eq!(parties.section().await?.joining, None);
    Ok(())
}

#[tokio::test]
async fn party_commands_reach_pingers_parties_and_the_store() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    let mut ids = Vec::new();
    for colour in PartyColour::ALL {
        let draft = PartyDraft {
            name: Name::new("Crew")?,
            my_name: Name::new("Sam")?,
            colour,
            post_quantum: PostQuantum::Off,
        };
        ids.push(parties.create_party(draft).await?);
    }
    let section = parties.section().await?;
    for (id, colour) in ids.iter().zip(PartyColour::ALL) {
        let shown = section.parties.iter().find(|party| party.id == *id);
        assert_eq!(shown.map(|party| party.colour), Some(colour));
    }
    let party = *ids.first().ok_or("a party")?;
    let me = section
        .parties
        .iter()
        .find(|shown| shown.id == party)
        .and_then(|shown| shown.members.first())
        .map(|member| member.leaf)
        .ok_or("a member")?;

    let key = session(&store, |session| {
        Ok(session
            .identity()
            .load()?
            .map(|identity| identity.signing_key.public_key()))
    })
    .await?
    .ok_or("an identity")?;
    for (change, expected) in [
        (AllowListChange::Add, true),
        (AllowListChange::Remove, false),
    ] {
        parties.set_always_allow(party, me, change).await?;
        let allowed = session(&store, move |session| {
            Ok(session.always_allow().contains(&party, &key)?)
        })
        .await?;
        assert_eq!(allowed, expected);
    }
    assert!(matches!(
        parties
            .set_always_allow(party, LeafIndex::new(9), AllowListChange::Add)
            .await,
        Err(ClientError::Parties(
            pingers_parties::PartiesError::UnknownMember
        ))
    ));
    assert!(
        parties
            .remove_member(party, LeafIndex::new(9))
            .await
            .is_err()
    );

    let link = crate::snapshot::InviteUrl::new("https://pingers.test/j#x")?;
    assert!(matches!(
        parties.open_invite(link, Name::new("Al")?).await,
        Err(ClientError::Unavailable(crate::Unavailable::Sync))
    ));
    // Without an instance nothing is sent: the relay is unreachable by definition.
    let sync = super::sync::Sync(offline(&store));
    assert_eq!(
        sync.check_reachability().await?,
        crate::snapshot::RelayStatus::Unreachable
    );
    assert_eq!(
        sync.section().await?.relay,
        crate::snapshot::RelayStatus::Unreachable
    );
    assert!(matches!(
        sync.set_wake_pushes(crate::snapshot::WakePushes::On, None)
            .await,
        Err(ClientError::Unavailable(crate::Unavailable::NoPushToken))
    ));

    parties.leave_party(party).await?;
    let colour_kept = session(&store, move |session| {
        super::records::read(session, &super::records::Key::Colour(&party), |reader| {
            reader.byte()
        })
    })
    .await?;
    assert_eq!(colour_kept, None, "leaving deletes the colour record");
    Ok(())
}

/// What one store session costs (a SQLCipher connection with a raw key, and the lock), since
/// every adapter and sync step opens its own. Run with `--ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
#[expect(clippy::disallowed_methods, reason = "measures real elapsed time")]
fn measure_a_session_open() -> TestResult {
    use std::io::Write as _;
    let store = store()?;
    let rounds: u32 = 500;
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        drop(store.backend.database.session()?.parties().list()?);
    }
    let each = started.elapsed() / rounds;
    writeln!(
        std::io::stderr(),
        "one session open and a small read: {each:?}"
    )?;
    Ok(())
}

/// Pools an invite mailbox, as pingers-sync will.
async fn pool_invite_mailbox(store: &Store, fill: u8) -> Result<(), ClientError> {
    session(store, move |session| {
        session.mailboxes().insert(&pingers_store::Mailbox {
            read_key: pingers_crypto::MailboxReadKey::from_bytes(Zeroizing::new([fill; 32])),
            purpose: pingers_store::MailboxPurpose::Invite,
            state: pingers_store::MailboxState::Pooled,
            push: None,
            seal: None,
            expires_at: UnixSeconds::new(1_900_000_000),
        })?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn cancelling_an_invite_revokes_it_and_withdraws_its_link() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    let draft = |name: &str| -> Result<PartyDraft, Box<dyn std::error::Error>> {
        Ok(PartyDraft {
            name: Name::new(name)?,
            my_name: Name::new("Sam")?,
            colour: PartyColour::Blue,
            post_quantum: PostQuantum::Off,
        })
    };
    let crew = parties.create_party(draft("Crew")?).await?;
    let flat = parties.create_party(draft("Flat")?).await?;
    for fill in [1, 2, 3] {
        pool_invite_mailbox(&store, fill).await?;
    }
    parties.create_invite(crew).await?;
    parties.create_invite(crew).await?;
    parties.create_invite(flat).await?;
    let invites = |section: &crate::ports::PartiesSection, party: PartyId| {
        section
            .parties
            .iter()
            .find(|shown| shown.id == party)
            .map(|shown| {
                shown
                    .invites
                    .iter()
                    .map(|invite| invite.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let before = parties.section().await?;
    // Newest first: the second invite made is the one whose link is on show.
    let [newest, oldest] = invites(&before, crew)[..] else {
        return Err("two invites".into());
    };
    let [flats] = invites(&before, flat)[..] else {
        return Err("one invite".into());
    };

    // Another Party's invite cannot be cancelled through this one.
    assert!(matches!(
        parties.cancel_invite(crew, flats).await,
        Err(ClientError::Parties(
            pingers_parties::PartiesError::UnknownInvite
        ))
    ));
    // Cancelling the older invite keeps the newer link on show.
    parties.cancel_invite(crew, oldest).await?;
    let after = parties.section().await?;
    assert_eq!(invites(&after, crew), [newest]);
    let link_of = |section: &crate::ports::PartiesSection, party: PartyId| {
        section
            .parties
            .iter()
            .find(|shown| shown.id == party)
            .and_then(|shown| shown.invite_link.clone())
    };
    assert!(link_of(&after, crew).is_some());
    // Cancelling the invite on show withdraws its link; the other Party's stays.
    parties.cancel_invite(crew, newest).await?;
    let last = parties.section().await?;
    assert!(invites(&last, crew).is_empty());
    assert_eq!(link_of(&last, crew), None);
    assert!(link_of(&last, flat).is_some());
    Ok(())
}

#[tokio::test]
async fn the_display_name_is_kept_in_the_store() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    assert_eq!(parties.section().await?.display_name, None);
    parties.set_display_name(Name::new("Sam")?).await?;
    parties.set_display_name(Name::new("Samira")?).await?;
    // A fresh adapter on the same store reads it back.
    let again = Parties::new(Arc::clone(&store.backend), offline(&store));
    assert_eq!(
        again.section().await?.display_name,
        Some(Name::new("Samira")?)
    );
    Ok(())
}

/// An admission the client cannot carry out is reported, not swallowed: without an instance
/// there is no transport for the Meetup's live channel, and an add names no live Meetup. The
/// wake then ends with `PortFailed` and retries instead of believing the knocker was admitted.
#[tokio::test]
async fn an_admission_the_client_cannot_carry_out_is_reported() -> TestResult {
    use pingers_meetup_session::knock_key_package;
    use pingers_mls::CipherSuitePolicy;
    use pingers_presence::{ActiveSpot, Admission, SpotStatus, VerifiedKnock};
    use pingers_proto::{GroupId, Knock, KnockSignature};
    use pingers_wake::ports::{MeetupAdmission, MeetupPort as _};

    let store = store()?;
    let meetups = Meetups::new(Arc::clone(&store.backend), offline(&store));
    let key_package = knock_key_package(
        &store.backend.database,
        CipherSuitePolicy::Classical,
        Box::new(ChaCha20Rng::seed_from_u64(4)),
    )
    .await?
    .key_package;
    let status = SpotStatus {
        spot: spot_ref(1),
        epoch: Epoch::new(1),
        state: PresenceState::Inside,
    };
    let root = pingers_crypto::PresenceRoot::from_bytes(Zeroizing::new([3; 32]));
    let start = Bucket::containing(store.clock.now());
    for admission in [
        Admission::CreateMeetup,
        Admission::AddToMeetup(GroupId::from_bytes([7; 32])),
    ] {
        let knocker = AccountSigningKey::generate(&mut ChaCha20Rng::seed_from_u64(8));
        let outcome = meetups
            .admit(MeetupAdmission {
                key: ActiveSpot::derive(&root, &status)?.meetup_key(start)?,
                admission,
                knock: VerifiedKnock {
                    knocker: LeafIndex::new(1),
                    knocker_key: knocker.public_key(),
                    knock: Knock {
                        matched_randomness: RecordRandomness::from_bytes([1; 16]),
                        own_randomness: None,
                        leaf_index: LeafIndex::new(1),
                        party_epoch: Epoch::new(1),
                        key_package: key_package.clone(),
                        reply_mailbox: MailboxId::from_bytes([2; 16]),
                        meetup_subscription: None,
                        signature: KnockSignature::from_bytes([0; 64]),
                    },
                },
            })
            .await;
        assert_eq!(outcome, Err(PortError::Unavailable));
    }
    Ok(())
}

#[test]
fn a_direct_answer_holds_one_piece_and_its_headers() -> TestResult {
    let piece = usize::try_from(pingers_map_downloads::fetch::DIRECT_PIECE)?;
    assert!(super::maps::DIRECT_LIMIT >= piece + 64 * 1024);
    assert!(super::maps::DIRECT_LIMIT <= piece + 2 * 1024 * 1024);
    assert_eq!(super::maps::DIRECT_LIMIT, 5_242_880);
    Ok(())
}

#[tokio::test]
async fn map_host_pins_are_kept_for_their_host_and_forgotten_on_request() -> TestResult {
    use pingers_map_downloads::fetch::KeyPinStore as _;

    use super::maps::{Maps, StoredPins};
    use crate::ports::MapsPort as _;

    let store = store()?;
    let pins = StoredPins(Arc::clone(&store.backend));
    let host = pingers_proto::MapHostUrl::new("https://maps.example".to_owned())?;
    let other = pingers_proto::MapHostUrl::new("https://other.example".to_owned())?;
    assert_eq!(pins.pinned(&host).await?, None);
    pins.pin(&host, KeyConfigHash::from_bytes([3; 32])).await?;
    assert_eq!(
        pins.pinned(&host).await?,
        Some(KeyConfigHash::from_bytes([3; 32]))
    );
    // A pin for one host says nothing about another.
    assert_eq!(pins.pinned(&other).await?, None);
    let maps = Maps::new(
        Arc::clone(&store.backend),
        offline(&store),
        pingers_transport::TlsRoots::mozilla(),
    )?;
    maps.forget_key_pin().await?;
    assert_eq!(pins.pinned(&host).await?, None);
    Ok(())
}

#[tokio::test]
async fn without_an_instance_relayed_maps_have_no_route_and_send_nothing() -> TestResult {
    use crate::ports::{MapHosts, MapRoute, MapsPort as _};

    let store = store()?;
    let maps = super::maps::Maps::new(
        Arc::clone(&store.backend),
        offline(&store),
        pingers_transport::TlsRoots::mozilla(),
    )?;
    let hosts = MapHosts {
        user_override: Some(pingers_proto::MapHostUrl::new(
            "https://mine.invalid".to_owned(),
        )?),
        publisher: Some(pingers_proto::MapHostUrl::new(
            "https://publisher.invalid".to_owned(),
        )?),
    };
    let catalog = maps.catalog(hosts, MapRoute::Relay).await?;
    assert!(catalog.offers.is_empty());
    assert_eq!(
        catalog
            .failed
            .iter()
            .map(|failed| failed.reason)
            .collect::<Vec<_>>(),
        [
            pingers_map_downloads::catalog::HostFailure::NoRelayRoute,
            pingers_map_downloads::catalog::HostFailure::NoRelayRoute
        ]
    );
    Ok(())
}
