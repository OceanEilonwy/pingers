// SPDX-License-Identifier: AGPL-3.0-only
//! A group log round trip, and commits that lose the race for their epoch.

use pingers_crypto::LogAuthKeyPair;
use pingers_ohttp::BinaryRequest;
use pingers_proto::{
    AppMessage, CommitRequest, ContentType, CreateGroup, Delivery, Epoch, GroupId, GroupLogEntry,
    MlsMessageBytes, SequenceNumber, Urgency,
};
use pingers_sync::{
    ApiRequest, CommitOwner, Committed, Delivered, GroupCall, GroupError, GroupLogOwner,
    GroupSigner, Sending, WakeBudget,
};
use zeroize::Zeroizing;

use crate::support::{Device, TestError, TestResult, World};

/// The log-auth key of `epoch` (all members derive the same one from the MLS exporter).
pub(crate) fn key_for(epoch: Epoch) -> LogAuthKeyPair {
    let seed = u8::try_from(epoch.get() % 250)
        .unwrap_or(0)
        .saturating_add(1);
    LogAuthKeyPair::from_bytes(Zeroizing::new([seed; 32]))
}

fn next(epoch: Epoch) -> Epoch {
    epoch.checked_next().unwrap_or(epoch)
}

#[derive(Debug, thiserror::Error)]
#[error("member refused an entry")]
pub(crate) struct Refused;

/// A stand-in for the MLS owner: tracks the epoch and what it was handed.
pub(crate) struct Member {
    name: u8,
    pub(crate) epoch: Epoch,
    key: LogAuthKeyPair,
    applied: Option<SequenceNumber>,
    entries: Vec<GroupLogEntry>,
    rejections: Vec<Epoch>,
    refuse_at: Option<SequenceNumber>,
}

impl Member {
    pub(crate) fn new(name: u8) -> Self {
        Self {
            name,
            epoch: Epoch::new(0),
            key: key_for(Epoch::new(0)),
            applied: None,
            entries: Vec::new(),
            rejections: Vec::new(),
            refuse_at: None,
        }
    }

    fn enter(&mut self, epoch: Epoch) {
        self.epoch = epoch;
        self.key = key_for(epoch);
    }
}

fn commit_for(name: u8, epoch: Epoch) -> CommitRequest {
    CommitRequest {
        epoch,
        commit: MlsMessageBytes::from_bytes(vec![name, u8::try_from(epoch.get()).unwrap_or(0)]),
        next_auth_key: key_for(next(epoch)).public_key(),
        state: None,
        subscriptions_added: Vec::new(),
        subscriptions_removed: Vec::new(),
        wake: Urgency::Quiet,
    }
}

impl GroupLogOwner for Member {
    type Error = Refused;

    fn applied_through(&self) -> Option<SequenceNumber> {
        self.applied
    }

    fn log_auth_key(&self) -> &LogAuthKeyPair {
        &self.key
    }

    fn apply(&mut self, entry: GroupLogEntry) -> Result<(), Refused> {
        if self.refuse_at == Some(entry.sequence) {
            return Err(Refused);
        }
        if entry.content_type == ContentType::Commit {
            // Applying a commit (ours or another's) moves to the epoch after it.
            self.enter(next(entry.epoch));
        }
        self.applied = Some(entry.sequence);
        self.entries.push(entry);
        Ok(())
    }
}

impl CommitOwner for Member {
    fn prepare_commit(&mut self) -> Result<Option<CommitRequest>, Refused> {
        Ok(Some(commit_for(self.name, self.epoch)))
    }

    fn commit_accepted(&mut self, _sequence: SequenceNumber) -> Result<(), Refused> {
        self.enter(next(self.epoch));
        Ok(())
    }

    fn commit_rejected(&mut self, head_epoch: Epoch) -> Result<(), Refused> {
        self.rejections.push(head_epoch);
        Ok(())
    }
}

/// An owner with nothing to commit.
struct Idle(Member);

impl GroupLogOwner for Idle {
    type Error = Refused;

    fn applied_through(&self) -> Option<SequenceNumber> {
        self.0.applied_through()
    }

    fn log_auth_key(&self) -> &LogAuthKeyPair {
        self.0.log_auth_key()
    }

    fn apply(&mut self, entry: GroupLogEntry) -> Result<(), Refused> {
        self.0.apply(entry)
    }
}

impl CommitOwner for Idle {
    fn prepare_commit(&mut self) -> Result<Option<CommitRequest>, Refused> {
        Ok(None)
    }

    fn commit_accepted(&mut self, _sequence: SequenceNumber) -> Result<(), Refused> {
        Err(Refused)
    }

    fn commit_rejected(&mut self, _head_epoch: Epoch) -> Result<(), Refused> {
        Err(Refused)
    }
}

pub(crate) const GROUP: GroupId = GroupId::from_bytes([0x42; 32]);

pub(crate) async fn create(device: &Device) -> TestResult {
    device
        .engine
        .create_group(
            &CreateGroup {
                group_id: GROUP,
                auth_key: key_for(Epoch::new(0)).public_key(),
                subscriptions: Vec::new(),
                state: None,
            },
            WakeBudget::UNLIMITED,
        )
        .await?;
    Ok(())
}

/// Another member's commit for `epoch`, as it would arrive at the server.
fn rival(name: u8, epoch: Epoch, world: &World) -> Result<BinaryRequest, TestError> {
    let key = key_for(epoch);
    let request = ApiRequest::commit(
        &GROUP,
        &commit_for(name, epoch),
        &GroupSigner {
            key: &key,
            now: pingers_time::Clock::now(&world.clock),
        },
    )?;
    Ok(BinaryRequest {
        method: request.method().to_owned(),
        path: request.path().to_owned(),
        headers: request.headers().to_vec(),
        body: request.body().to_vec(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_log_round_trip_with_a_commit_conflict() -> TestResult {
    let world = World::start()?;
    let alice = world.device()?;
    let bob = world.device()?;
    let _ = alice.stock_tokens().await?;
    let _ = bob.stock_tokens().await?;
    create(&alice).await?;

    let message = AppMessage {
        epoch: Epoch::new(0),
        body: MlsMessageBytes::from_bytes(vec![1, 2, 3]),
        delivery: Delivery::Stored,
        wake: None,
    };
    let alice_key = key_for(Epoch::new(0));
    let posted = alice
        .engine
        .post_group_message(
            GroupCall {
                group: &GROUP,
                key: &alice_key,
            },
            &message,
            Sending::or_drop(WakeBudget::UNLIMITED),
        )
        .await?;
    let Delivered::Done(Some(first)) = posted else {
        return Err(TestError::Unexpected("message not stored"));
    };

    let mut bob_state = Member::new(b'B');
    let pulled = bob
        .engine
        .pull_group(&GROUP, &mut bob_state, WakeBudget::UNLIMITED)
        .await
        .map_err(|_| TestError::Unexpected("pull failed"))?;
    assert_eq!(pulled.applied, 1);
    assert_eq!(pulled.head_epoch, Some(Epoch::new(0)));
    assert_eq!(bob_state.applied, Some(first));
    assert_eq!(
        bob_state
            .entries
            .first()
            .map(|entry| entry.body.as_bytes().to_vec()),
        Some(vec![1, 2, 3])
    );

    // Alice's commit for epoch 0 reaches the server just before Bob's.
    bob.link
        .race_next_commit(rival(b'A', Epoch::new(0), &world)?);
    let committed = bob
        .engine
        .commit(&GROUP, &mut bob_state, WakeBudget::UNLIMITED)
        .await
        .map_err(|_| TestError::Unexpected("commit failed"))?;
    assert_eq!(bob_state.rejections, [Epoch::new(1)], "lost epoch 0 once");
    let Committed::Accepted(bob_commit) = committed else {
        return Err(TestError::Unexpected("nothing committed"));
    };
    assert_eq!(
        bob_state.epoch,
        Epoch::new(2),
        "re-applied Alice's, then merged its own"
    );
    let applied: Vec<Vec<u8>> = bob_state
        .entries
        .iter()
        .map(|entry| entry.body.as_bytes().to_vec())
        .collect();
    assert_eq!(applied, [vec![1, 2, 3], vec![b'A', 0]]);

    // Alice sees everything in order, ending with Bob's commit for epoch 1.
    let mut alice_state = Member::new(b'A');
    let pulled = alice
        .engine
        .pull_group(&GROUP, &mut alice_state, WakeBudget::UNLIMITED)
        .await
        .map_err(|_| TestError::Unexpected("pull failed"))?;
    assert_eq!(pulled.applied, 3);
    assert_eq!(pulled.head_epoch, Some(Epoch::new(2)));
    assert_eq!(alice_state.applied, Some(bob_commit));
    assert_eq!(
        alice_state
            .entries
            .last()
            .map(|entry| (entry.epoch, entry.body.as_bytes().to_vec())),
        Some((Epoch::new(1), vec![b'B', 1]))
    );
    // Every group write carried a credential; reads only the group signature.
    for seen in bob.link.seen() {
        let write = seen.method == "POST";
        assert_eq!(
            seen.authorization.is_some(),
            write,
            "{} {}",
            seen.method,
            seen.path
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_commit_that_keeps_losing_gives_up_after_its_attempts() -> TestResult {
    let world = World::start()?;
    let alice = world.device()?;
    let bob = world.device()?;
    let _ = alice.stock_tokens().await?;
    let _ = bob.stock_tokens().await?;
    create(&alice).await?;

    let mut bob_state = Member::new(b'B');
    for epoch in 0..3 {
        bob.link
            .race_next_commit(rival(b'A', Epoch::new(epoch), &world)?);
    }
    let outcome = bob
        .engine
        .commit(&GROUP, &mut bob_state, WakeBudget::UNLIMITED)
        .await;
    assert!(matches!(
        outcome,
        Err(GroupError::ConflictsExhausted { attempts: 3 })
    ));
    assert_eq!(
        bob_state.rejections,
        [Epoch::new(1), Epoch::new(2), Epoch::new(3)]
    );
    // Bob applied each winner before trying again; the last one waits for the next pull.
    assert_eq!(bob_state.epoch, Epoch::new(2));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_to_commit_after_catching_up_posts_nothing() -> TestResult {
    let world = World::start()?;
    let alice = world.device()?;
    let _ = alice.stock_tokens().await?;
    create(&alice).await?;
    let mut idle = Idle(Member::new(b'A'));
    let committed = alice
        .engine
        .commit(&GROUP, &mut idle, WakeBudget::UNLIMITED)
        .await
        .map_err(|_| TestError::Unexpected("commit failed"))?;
    assert_eq!(committed, Committed::NothingToCommit);
    assert!(
        alice
            .link
            .seen()
            .iter()
            .all(|seen| !seen.path.ends_with("/commit"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_the_owner_refuses_stops_the_pull_there() -> TestResult {
    let world = World::start()?;
    let alice = world.device()?;
    let _ = alice.stock_tokens().await?;
    create(&alice).await?;
    let key = key_for(Epoch::new(0));
    for body in [1_u8, 2, 3] {
        let _ = alice
            .engine
            .post_group_message(
                GroupCall {
                    group: &GROUP,
                    key: &key,
                },
                &AppMessage {
                    epoch: Epoch::new(0),
                    body: MlsMessageBytes::from_bytes(vec![body]),
                    delivery: Delivery::Stored,
                    wake: None,
                },
                Sending::or_drop(WakeBudget::UNLIMITED),
            )
            .await?;
    }
    let mut member = Member::new(b'B');
    member.refuse_at = Some(SequenceNumber::new(2));
    let pulled = alice
        .engine
        .pull_group(&GROUP, &mut member, WakeBudget::UNLIMITED)
        .await;
    assert!(matches!(pulled, Err(GroupError::Owner(Refused))));
    assert_eq!(member.applied, Some(SequenceNumber::new(1)));
    Ok(())
}
