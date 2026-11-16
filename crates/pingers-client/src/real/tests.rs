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
    BoardSnapshot, DecidedKnock, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, PeerActivity,
    PortError, RETRY_AFTER, RealRecord, SentKnock, SpotSnapshot, WakeEnding, WakeSnapshot,
    WakeStep, WakeTrigger,
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
    })?;
    let clock = ManualClock::new(UnixSeconds::new(1_800_000_000));
    let seeds = Arc::new(AtomicU64::new(0));
    let backend = Arc::new(Backend {
        database: Arc::new(database),
        self_test: pingers_sync::SelfTestSlot::default(),
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

/// The built-in server of these tests: pinned, behind a relay on a reserved name that never
/// resolves, so every request waits as it would with no network.
fn unreachable() -> Result<super::Instance, Box<dyn std::error::Error>> {
    Ok(super::Instance {
        server: ServerId::new("https://pingers.test".to_owned())?,
        relay: "https://relay.invalid".to_owned(),
        push_relay: None,
        gateway_pins: Some(pingers_ohttp::KeyPins::from(KeyConfigHash::from_bytes(
            [1; 32],
        ))),
        push_relay_pins: None,
        trust: super::Trust::Mozilla,
        registration_code: None,
    })
}

/// pingers-sync with its built-in server out of reach: every request waits.
fn offline(store: &Store) -> Arc<Net> {
    let instance = unreachable().ok();
    Arc::new(Net::new(
        Arc::clone(&store.backend),
        super::servers::Servers::new(Arc::clone(&store.backend), instance, None),
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
        boards: vec![
            BoardSnapshot {
                server: ServerId::new(String::from("https://one.example"))?,
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
            },
            BoardSnapshot {
                server: ServerId::new(String::from("https://two.example"))?,
                leases: Vec::new(),
                sent_knocks: vec![knock(9, KnockStatus::Waiting)],
                decided_knocks: Vec::new(),
                last_upload: None,
            },
        ],
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
        let empty = wake_codec::load(session, None)?;
        wake_codec::save(session, &saved)?;
        Ok((empty, wake_codec::load(session, None)?))
    })
    .await?;
    assert_eq!(loaded, (WakeSnapshot::default(), snapshot));
    Ok(())
}

/// A snapshot stored in the first layout (one board, no server) has its leases, decisions and
/// last upload put under the instance's server; without an instance they are dropped and the
/// rest is kept.
#[tokio::test]
async fn a_first_layout_wake_snapshot_moves_into_the_instance_servers_board() -> TestResult {
    let store = store()?;
    let lease = LiveLease {
        start: Bucket::from_index(1_000)?,
        slots: [LeaseSlot {
            randomness: RecordRandomness::from_bytes([2; 16]),
            real: None,
        }; Lease::SLOTS],
    };
    let decided = DecidedKnock {
        reply_mailbox: MailboxId::from_bytes([11; 16]),
        at: UnixSeconds::new(40),
    };
    let known = KnownEpoch {
        party: PartyId::from_bytes([12; 16]),
        epoch: Epoch::new(8),
    };
    let server = ServerId::new(String::from("https://one.example"))?;
    let legacy = server.clone();
    let (moved, dropped) = session(&store, move |session| {
        super::records::write(session, &super::records::Key::WakeSnapshot, |writer| {
            // No Spots, no exclusion trail.
            writer.len(0);
            writer.option(None::<UnixSeconds>, |_writer, _at| {});
            writer.len(1);
            writer.u64(lease.start.index());
            for slot in &lease.slots {
                writer.fixed(slot.randomness.as_bytes());
                writer.option(None::<RealRecord>, |_writer, _real| {});
            }
            // No knocks sent, one decided, the last upload.
            writer.len(0);
            writer.len(1);
            writer.fixed(decided.reply_mailbox.as_bytes());
            writer.u64(decided.at.get());
            writer.option(Some(UnixSeconds::new(50)), |writer, at| {
                writer.u64(at.get());
            });
            writer.len(1);
            writer.fixed(known.party.as_bytes());
            writer.u64(known.epoch.get());
            // No peer activity.
            writer.len(0);
        })?;
        Ok((
            wake_codec::load(session, Some(&legacy))?,
            wake_codec::load(session, None)?,
        ))
    })
    .await?;
    assert_eq!(
        moved.boards,
        vec![BoardSnapshot {
            server,
            leases: vec![lease],
            sent_knocks: Vec::new(),
            decided_knocks: vec![decided],
            last_upload: Some(UnixSeconds::new(50)),
        }]
    );
    assert_eq!(moved.epochs, vec![known]);
    assert!(dropped.boards.is_empty());
    assert_eq!(dropped.epochs, vec![known]);
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
            server: None,
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
            // The built-in server's relay never resolves.
            error: PortError::Unreachable,
        }
    );
    assert_eq!(outcome.location_mode, Some(LocationMode::Dwell));
    assert_eq!(outcome.next_wake, Some(RETRY_AFTER));
    assert!(outcome.events.is_empty());

    let stored = session(&store, |session| wake_codec::load(session, None)).await?;
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
async fn without_any_server_wake_pushes_cannot_go_on() -> TestResult {
    let store = store()?;
    let net = Arc::new(Net::new(
        Arc::clone(&store.backend),
        super::servers::Servers::new(Arc::clone(&store.backend), None, None),
    ));
    let sync = super::sync::Sync(net);
    let tokens = crate::PushTokens {
        platform: pingers_proto::PushPlatform::UnifiedPush,
        token: pingers_proto::PushToken::from_bytes(b"https://push.example/up".to_vec()),
    };
    assert!(matches!(
        sync.set_wake_pushes(crate::snapshot::WakePushes::On, Some(tokens))
            .await,
        Err(ClientError::Unavailable(crate::Unavailable::Sync))
    ));
    Ok(())
}

#[tokio::test]
async fn a_link_that_is_not_an_invite_starts_no_join() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    let link = crate::snapshot::InviteUrl::new("https://pingers.test/j#x")?;
    assert!(matches!(
        parties.open_invite(link, Name::new("Al")?).await,
        Err(ClientError::Parties(_))
    ));
    Ok(())
}

#[tokio::test]
async fn join_decisions_and_withdrawals_need_a_request() -> TestResult {
    let store = store()?;
    let parties = Parties::new(Arc::clone(&store.backend), offline(&store));
    let draft = PartyDraft {
        server: None,
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
            server: None,
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
        session
            .server(
                &ServerId::new("https://pingers.test".to_owned())
                    .map_err(|_bad| ClientError::Internal)?,
            )?
            .mailboxes()
            .insert(&pingers_store::Mailbox {
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
            server: None,
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

/// The self-test is of this device's prover, not of a server: with Parties on two servers, the
/// second server's engine finds the first one's test running instead of starting another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engines_of_two_servers_run_one_self_test() -> TestResult {
    let store = store()?;
    let mut engines = Vec::new();
    for origin in ["https://a.test", "https://b.test"] {
        let server = ServerId::new(origin.to_owned())?;
        let record = pingers_store::ServerRecord {
            server: server.clone(),
            ohttp_relays: vec!["https://relay.invalid".to_owned()],
            push_relay: None,
            gateway_pins: vec![KeyConfigHash::from_bytes([1; 32])],
            push_relay_pins: Vec::new(),
            trust: pingers_store::ServerTrust::Confirmed,
        };
        session(&store, move |session| Ok(session.servers().put(&record)?)).await?;
        let relays = ["https://relay.invalid".to_owned()];
        let transport = super::network::router(
            super::network::Route {
                server: &server,
                relays: &relays,
                pin: pingers_transport::KeyPin::Pinned(pingers_ohttp::KeyPins::from(
                    KeyConfigHash::from_bytes([1; 32]),
                )),
            },
            &super::Trust::Mozilla,
            &store.backend.outside,
        )?;
        engines.push(super::network::engine(&server, transport, &store.backend)?);
    }
    let [first, second] = engines.as_slice() else {
        return Err("two engines".into());
    };
    assert_eq!(
        first.start_prover_self_test().await?,
        pingers_sync::SelfTestStart::Started
    );
    // A proof takes far longer than this call: the first engine's test is still running.
    assert_eq!(
        second.start_prover_self_test().await?,
        pingers_sync::SelfTestStart::AlreadyRunning
    );
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
        // A build that names no built-in server.
        Arc::new(Net::new(
            Arc::clone(&store.backend),
            super::servers::Servers::new(Arc::clone(&store.backend), None, None),
        )),
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

/// The parameters read while a server was looked at are kept: nothing fetches them again in
/// this process (one request fewer through its relay), and its operator shows at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parameters_learned_from_a_look_are_not_fetched_again() -> TestResult {
    use pingers_proto::{
        AnonymousCredential, CoverReadRatio, CustomPushRelays, Params, RelayUrl, Slashing,
        TokenAllowance,
    };

    let store = store()?;
    let net = offline(&store);
    let link = net.servers.built_in_link().await?;
    link.learn_params(Params {
        anonymous_credentials: vec![AnonymousCredential::PrivacyPass],
        rln_slashing: Slashing::Off,
        cover_read_ratio: CoverReadRatio::ALWAYS,
        board_page_records: 64,
        ohttp_relays: vec![RelayUrl::new("https://relay.invalid".to_owned())?],
        push_relays: Vec::new(),
        custom_push_relays: CustomPushRelays::NotAllowed,
        server_operator: "Flat 7".to_owned(),
        server_time: store.clock.now(),
        map_host: None,
        privacy_pass_daily_limit: TokenAllowance::new(300),
    })
    .await?;
    // The relay never resolves: a fetch would fail, not answer `None`.
    assert!(matches!(link.fetch_params_once().await, Ok(None)));
    assert_eq!(
        link.operators()
            .map(|known| known.server.as_str().to_owned())
            .as_deref(),
        Some("Flat 7")
    );
    Ok(())
}

/// A relay that answers its key config endpoint with an error (here: its gateway is gone) fails
/// the check with a transport error, not "no relay": the Party's settings must still show the
/// relay as unreachable, not the last good answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_relay_answering_with_an_error_shows_as_unreachable() -> TestResult {
    use crate::snapshot::RelayStatus;

    let store = store()?;
    // A port nothing listens on any more, for the relay's gateway.
    let gone = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let relay = tokio::spawn(pingers_testkit::harness::run_ohttp_relay(
        listener,
        gone,
        async move {
            drop(stopped.await);
        },
    ));
    let instance = super::Instance {
        relay: format!("http://{address}/ohttp"),
        ..unreachable()?
    };
    let net = Arc::new(Net::new(
        Arc::clone(&store.backend),
        super::servers::Servers::new(Arc::clone(&store.backend), Some(instance), None),
    ));
    let link = net.servers.built_in_link().await?;
    link.observe(&Ok::<(), pingers_sync::SyncError>(()));
    assert_eq!(link.relay_status(), RelayStatus::Reachable);

    let answer = super::sync::Sync(Arc::clone(&net))
        .check_reachability()
        .await;
    // The relay may have stopped on its own; either way it is told to.
    let _sent = stop.send(());
    relay.await?;
    assert_eq!(answer?, RelayStatus::Unreachable);
    assert_eq!(link.relay_status(), RelayStatus::Unreachable);
    Ok(())
}

/// What a server's traffic said becomes one notice each, once: a clock off by more than the
/// correction allows, and a key mismatch, whether the registry or the link noticed it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_server_notice_is_raised_once() -> TestResult {
    use pingers_sync::{ClockOffset, SyncError};

    use crate::Notice;

    let store = store()?;
    let net = offline(&store);
    let link = net.servers.built_in_link().await?;
    let now = store.clock.now();
    let skewed = UnixSeconds::new(now.get().saturating_add(3_600));
    link.observe(&Err::<(), _>(SyncError::ClockSkew {
        offset: ClockOffset::between(skewed, now),
    }));
    link.observe(&Err::<(), _>(SyncError::RelayKeyMismatch));
    net.servers.raise_key_mismatch();
    assert_eq!(
        net.take_notices(),
        [Notice::RelayKeyMismatch, Notice::ClockSkew]
    );
    assert!(net.take_notices().is_empty());
    Ok(())
}
