// SPDX-License-Identifier: AGPL-3.0-only
//! The cast and the set for the scenarios: a stack, three devices in one Party, one Spot.

use std::sync::Arc;

use pingers_e2e::{Device, E2eError, Place, Recorder, Stack, form_party};
use pingers_presence::{Admission, KnockDecision};
use pingers_proto::{GroupId, SpotId};

pub(crate) type TestResult = Result<(), E2eError>;

/// The Anchor, a pub on the Cam.
pub(crate) const LATITUDE: f64 = 52.2097;
pub(crate) const LONGITUDE: f64 = 0.1167;

pub(crate) struct World {
    pub(crate) stack: Stack,
    pub(crate) recorder: Arc<Recorder>,
    pub(crate) place: Place,
    pub(crate) alice: Device,
    pub(crate) bob: Device,
    pub(crate) carol: Device,
}

pub(crate) fn check(condition: bool, what: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(E2eError::step(what))
    }
}

impl World {
    pub(crate) async fn new(seed: u64) -> Result<Self, E2eError> {
        let stack = Stack::start(seed).await?;
        let recorder = Recorder::keeping_bodies();
        let mut alice = Device::new("alice", seed.wrapping_add(1), &stack, Arc::clone(&recorder))?;
        let mut bob = Device::new("bob", seed.wrapping_add(2), &stack, Arc::clone(&recorder))?;
        let mut carol = Device::new("carol", seed.wrapping_add(3), &stack, Arc::clone(&recorder))?;
        let party = form_party(&mut [&mut alice, &mut bob, &mut carol], seed)?;
        let place = Place {
            party,
            spot: SpotId::from_bytes([0x5a; 16]),
            latitude: LATITUDE,
            longitude: LONGITUDE,
            radius_m: 60.0,
        };
        Ok(Self {
            stack,
            recorder,
            place,
            alice,
            bob,
            carol,
        })
    }

    /// G.3: Alice enters alone, posts a lease with one real record, reads, matches nothing.
    pub(crate) async fn g3_alice_alone(&mut self) -> TestResult {
        self.alice.arrive(&self.stack, self.place).await?;
        let report = self.alice.sync(&self.stack, true).await?;
        check(
            report.matches.is_empty(),
            "G.3: Alice matched something alone",
        )?;
        check(
            report.records.len() == 4,
            "G.3: the board holds her 4 records",
        )?;
        check(self.alice.lease_has_real_record(), "G.3: one real record")
    }

    /// G.4: Bob enters, matches Alice's record, knocks; Alice accepts and creates the Meetup;
    /// Bob joins from the Welcome.
    pub(crate) async fn g4_bob_meets_alice(&mut self) -> Result<GroupId, E2eError> {
        self.bob.arrive(&self.stack, self.place).await?;
        let report = self.bob.sync(&self.stack, true).await?;
        check(
            report.matches.len() == 1,
            "G.4: Bob matches exactly Alice's record",
        )?;
        let found = report
            .matches
            .first()
            .ok_or_else(|| E2eError::step("no match"))?;
        check(
            Some(found.beacon.knock_mailbox) == self.alice.knock_mailbox(),
            "G.4: the beacon names Alice's knock mailbox",
        )?;
        check(
            found.beacon.meetup_hint.is_none(),
            "G.4: no Meetup hint yet",
        )?;
        check(
            self.bob.knock(&self.stack, &report.matches).await? == 1,
            "G.4: one knock",
        )?;

        let answers = self.alice.answer().await?;
        let [answer] = answers.as_slice() else {
            return Err(E2eError::step("G.4: Alice should find one knock"));
        };
        check(
            answer.decision == KnockDecision::Accept(Admission::CreateMeetup),
            "G.4: Alice accepts and creates",
        )?;
        check(
            answer.knock.knocker_key == self.bob.account_key(),
            "G.4: the knock is signed by Bob",
        )?;
        let group = self.alice.create_meetup(&self.stack, answer).await?;
        check(
            self.bob.join_meetup().await?,
            "G.4: Bob joins from the Welcome",
        )?;
        check(self.bob.meetup() == Some(group), "G.4: same group")?;
        check(
            self.alice.meetup_epoch_and_size()? == self.bob.meetup_epoch_and_size()?,
            "G.4: same epoch and roster",
        )?;
        check(
            self.alice.meetup_state_key()? == self.bob.meetup_state_key()?,
            "G.4: same epoch secrets",
        )?;
        Ok(group)
    }

    /// G.5: both re-post with the hint; Carol matches both, knocks both; both try to add her,
    /// one commit wins, the other gets a conflict; Carol joins at the new epoch.
    pub(crate) async fn g5_carol_joins(&mut self, group: GroupId) -> TestResult {
        self.alice.sync(&self.stack, true).await?;
        let bob_sees = self.bob.sync(&self.stack, true).await?;
        check(
            self.bob.knock(&self.stack, &bob_sees.matches).await? == 0,
            "G.5: fellow members do not knock each other",
        )?;

        self.carol.arrive(&self.stack, self.place).await?;
        let found = self.carol.sync(&self.stack, true).await?;
        check(found.matches.len() == 2, "G.5: Carol matches two records")?;
        check(
            found.matches.iter().all(|found| {
                found.beacon.meetup_hint == Some(pingers_proto::MeetupHint::for_group(&group))
            }),
            "G.5: both carry the Meetup hint",
        )?;
        check(
            self.carol.knock(&self.stack, &found.matches).await? == 2,
            "G.5: Carol knocks both",
        )?;

        let expected = KnockDecision::Accept(Admission::AddToMeetup(group));
        let alice_answers = self.alice.answer().await?;
        let bob_answers = self.bob.answer().await?;
        let (Some(alice_answer), Some(bob_answer)) = (alice_answers.last(), bob_answers.last())
        else {
            return Err(E2eError::step("G.5: both should find Carol's knock"));
        };
        check(
            alice_answer.decision == expected && bob_answer.decision == expected,
            "G.5: both offer to add Carol",
        )?;
        let (epoch_before, _) = self.alice.meetup_epoch_and_size()?;
        check(
            self.alice.add_to_meetup(&self.stack, alice_answer).await?
                == pingers_e2e::KnockOutcome::Accepted,
            "G.5: Alice's commit wins",
        )?;
        check(
            self.bob.add_to_meetup(&self.stack, bob_answer).await?
                == pingers_e2e::KnockOutcome::Conflict,
            "G.5: Bob's commit loses (409)",
        )?;
        self.bob.catch_up(&self.stack)?;
        check(self.carol.join_meetup().await?, "G.5: Carol joins")?;
        let alice_view = self.alice.meetup_epoch_and_size()?;
        check(alice_view.1 == 3, "G.5: three members")?;
        check(
            alice_view.0.get() == epoch_before.get().saturating_add(1),
            "G.5: one new epoch",
        )?;
        check(
            self.bob.meetup_epoch_and_size()? == alice_view
                && self.carol.meetup_epoch_and_size()? == alice_view,
            "G.5: everyone at the same epoch",
        )?;
        check(
            self.carol.meetup_state_key()? == self.alice.meetup_state_key()?,
            "G.5: Carol shares the new epoch secrets",
        )
    }

    /// G.6: Bob leaves: proposal, lease revoked, all-dummy lease, state deleted; Alice commits
    /// the removal; Carol follows; the key rotates.
    pub(crate) async fn g6_bob_leaves(&mut self, group: GroupId) -> TestResult {
        let key_before = self.alice.meetup_state_key()?;
        self.bob.depart(&self.stack).await?;
        check(
            !self.bob.holds_spot_keys(),
            "G.6: Bob dropped the Spot keys",
        )?;
        self.bob.leave_meetup(&self.stack)?;
        self.bob.revoke_lease().await?;
        let after = self.bob.sync(&self.stack, true).await?;
        check(
            after.matches.is_empty(),
            "G.6: Bob matches nothing without keys",
        )?;
        check(
            !self.bob.lease_has_real_record(),
            "G.6: Bob's lease is all dummies",
        )?;
        check(
            !self.bob.has_group_state(&group)?,
            "G.6: Bob kept no group state",
        )?;

        check(
            self.alice.commit_removals(&self.stack)? == pingers_e2e::KnockOutcome::Accepted,
            "G.6: Alice commits Remove(Bob)",
        )?;
        self.carol.catch_up(&self.stack)?;
        let view = self.alice.meetup_epoch_and_size()?;
        check(view.1 == 2, "G.6: two members left")?;
        check(
            self.carol.meetup_epoch_and_size()? == view,
            "G.6: Carol follows to the new epoch",
        )?;
        let key_after = self.alice.meetup_state_key()?;
        check(key_after != key_before, "G.6: the epoch secrets rotated")?;
        check(
            self.carol.meetup_state_key()? == key_after,
            "G.6: Carol holds the new secrets",
        )?;

        // A newcomer's view: only Alice's and Carol's real records are left for the Spot.
        let carol_sees = self.carol.sync(&self.stack, true).await?;
        check(
            carol_sees.matches.len() == 1
                && carol_sees
                    .matches
                    .first()
                    .map(|found| found.beacon.knock_mailbox)
                    == self.alice.knock_mailbox(),
            "G.6: Carol sees only Alice's record",
        )
    }

    /// G.7: Carol leaves, Alice commits, Alice is alone, Alice leaves and deletes the group.
    pub(crate) async fn g7_everyone_leaves(&mut self, group: GroupId) -> TestResult {
        self.carol.depart(&self.stack).await?;
        self.carol.leave_meetup(&self.stack)?;
        self.carol.revoke_lease().await?;
        check(
            self.alice.commit_removals(&self.stack)? == pingers_e2e::KnockOutcome::Accepted,
            "G.7: Alice commits Remove(Carol)",
        )?;
        check(
            self.alice.meetup_epoch_and_size()?.1 == 1,
            "G.7: Alice is alone; the Meetup stays open",
        )?;
        self.alice.depart(&self.stack).await?;
        self.alice.end_meetup(&self.stack)?;
        self.alice.revoke_lease().await?;
        for device in [&mut self.alice, &mut self.bob, &mut self.carol] {
            check(
                !device.has_group_state(&group)?,
                &format!("G.7: {} kept no group state", device.name()),
            )?;
            check(device.meetup().is_none(), "G.7: nobody is in a Meetup")?;
            device.delete_mailboxes().await?;
        }
        Ok(())
    }
}
