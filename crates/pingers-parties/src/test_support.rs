// SPDX-License-Identifier: AGPL-3.0-only
//! Shared helpers for this crate's tests.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pingers_crypto::{AccountSigningKey, MailboxReadKey, account_id};
use pingers_geo::{Circle, Coordinate, Fence, Meters};
use pingers_proto::ServerId;
use pingers_store::{
    Database, DatabaseKey, Identity, Mailbox, MailboxPurpose, MailboxState, ServerRecord,
    ServerTrust, Session,
};
use pingers_time::{ManualClock, UnixSeconds};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng as _;
use zeroize::Zeroizing;

use crate::PartyLifecycle;

/// Everything a test can fail with.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TestError {
    #[error(transparent)]
    Parties(#[from] crate::PartiesError),
    #[error(transparent)]
    Store(#[from] pingers_store::StoreError),
    #[error(transparent)]
    Geo(#[from] pingers_geo::GeoError),
    #[error(transparent)]
    Proto(#[from] pingers_proto::ProtoError),
    #[error(transparent)]
    Mls(#[from] pingers_mls::MlsError),
    #[error(transparent)]
    Crypto(#[from] pingers_crypto::CryptoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Unexpected(&'static str),
}

/// A circle of `radius` metres in central London.
pub(crate) fn circle_fence(radius: f64) -> Result<Fence, TestError> {
    Ok(Fence::Circle(Circle::new(
        Coordinate::new(51.5, -0.12)?,
        Meters::new(radius)?,
    )?))
}

pub(crate) fn rng(seed: u64) -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(seed)
}

pub(crate) const SERVER: &str = "https://pingers.example";

/// The wall time every test starts at.
pub(crate) const NOW: UnixSeconds = UnixSeconds::new(1_800_000_000);

/// A device: a fresh database with an account, its own clock (starting at [`NOW`]), and a
/// counter for the bytes of the mailboxes and subscriptions it makes.
pub(crate) struct Device {
    // Field order: the session closes before the database and the directory go.
    pub(crate) session: Session,
    pub(crate) clock: ManualClock,
    next_fill: Cell<u8>,
    _database: Database,
    directory: PathBuf,
}

impl Device {
    pub(crate) fn new(seed: u8) -> Result<Self, TestError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "pingers-parties-test-{}-{unique}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&directory));
        std::fs::create_dir_all(&directory)?;
        let database = Database::open(
            &directory.join("store.db"),
            DatabaseKey::from_bytes(Zeroizing::new([seed; 32])),
        )?;
        let session = database.session()?;
        let signing_key = AccountSigningKey::from_bytes(Zeroizing::new([seed; 32]));
        session.identity().save(&Identity {
            account: account_id(&signing_key.public_key()),
            signing_key,
        })?;
        session.servers().put(&ServerRecord {
            server: ServerId::new(SERVER.into())?,
            ohttp_relays: Vec::new(),
            push_relay: None,
            gateway_pins: Vec::new(),
            push_relay_pins: Vec::new(),
            trust: ServerTrust::BuiltIn,
        })?;
        Ok(Self {
            session,
            clock: ManualClock::new(NOW),
            next_fill: Cell::new(seed.wrapping_mul(16)),
            _database: database,
            directory,
        })
    }

    /// Party operations on this device.
    pub(crate) fn parties(&self) -> PartyLifecycle<'_> {
        PartyLifecycle::new(&self.session, &self.clock)
    }

    /// A byte no earlier call on this device returned, for distinct ids.
    pub(crate) fn fill(&self) -> u8 {
        let fill = self.next_fill.get();
        self.next_fill.set(fill.wrapping_add(1));
        fill
    }

    /// Pools a mailbox for `purpose` that expires at `expires_at`.
    pub(crate) fn pool_mailbox(
        &self,
        purpose: MailboxPurpose,
        expires_at: UnixSeconds,
    ) -> Result<(), TestError> {
        let fill = self.fill();
        self.session
            .server(&ServerId::new(SERVER.into())?)?
            .mailboxes()
            .insert(&Mailbox {
                read_key: MailboxReadKey::from_bytes(Zeroizing::new([fill; 32])),
                purpose,
                state: MailboxState::Pooled,
                push: None,
                expires_at,
            })?;
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.directory));
    }
}
