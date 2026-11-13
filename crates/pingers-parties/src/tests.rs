// SPDX-License-Identifier: AGPL-3.0-only
//! Scenario tests across devices: create, invite, join, remove, Spots, settings.
#![expect(clippy::panic_in_result_fn, reason = "assertions are how tests fail")]

use core::time::Duration;

use pingers_geo::{Coordinate, Fence, GeoError, Meters};
use pingers_presence::{EpochTransition, RosterLookup as _};
use pingers_proto::{
    DisplayName, Epoch, GroupId, InviteUses, InviterFingerprint, LeafIndex, MailboxId,
    MlsMessageBytes, Padded, PartyId, PushHandle, PushRelayKeyId, RelayId, RelayRef, RelayUrl,
    SealedPush, SealedPushBlob, SealedSnapshot, ServerId, SpotId, Subscription, SubscriptionId,
    TimeToLive, Urgency,
};
use pingers_store::{
    LocationVisibility, MailboxPurpose, MemberName, PartyName, SpotName, Verification,
};
use pingers_time::UnixSeconds;
use rand_chacha::ChaCha20Rng;

use crate::test_support::{Device, NOW, SERVER, TestError, rng};
use crate::{
    ArrivedWelcome, CreatedInvite, INVITE_LIFETIME, Incoming, InviteId, InviteLink, InviteStatus,
    JoinDetails, JoinId, JoinRequestOutcome, Myself, NewParty, NewSpot, Outbound, OutgoingCommit,
    PartiesError, PartyLifecycle, PartySettings, PostQuantum, Refusal, RelayChoice, SafetyCode,
    SpotEdit, SpotShape,
};

const LEAF_ALICE: LeafIndex = LeafIndex::new(0);
const LEAF_BOB: LeafIndex = LeafIndex::new(1);

fn later(seconds: u64) -> Result<UnixSeconds, TestError> {
    NOW.checked_add(Duration::from_secs(seconds))
        .ok_or(TestError::Unexpected("time overflow"))
}

fn month() -> Result<UnixSeconds, TestError> {
    later(30 * 24 * 60 * 60)
}

fn subscription(fill: u8) -> Result<Subscription, TestError> {
    Ok(Subscription {
        id: SubscriptionId::from_bytes([fill; 16]),
        push: SealedPush {
            relay: RelayRef::Listed(RelayId::new(1)),
            key_id: PushRelayKeyId::new(0),
            blob: SealedPushBlob::from_bytes([fill; 320]),
            urgency: Urgency::Alert,
            handle: PushHandle::from_bytes([fill; 8]),
        },
        time_to_live: TimeToLive::new(Duration::from_hours(24))?,
    })
}

fn settings() -> Result<PartySettings, TestError> {
    Ok(PartySettings {
        server: ServerId::new(SERVER.into())?,
        relays: RelayChoice::ServerDefault,
        post_quantum: PostQuantum::Classical,
        location_visibility: None,
    })
}

fn new_party(
    settings: PartySettings,
    subscription: Option<Subscription>,
) -> Result<NewParty, TestError> {
    Ok(NewParty {
        name: PartyName::new("Friends".into())?,
        own_name: MemberName::new("Alice".into())?,
        settings,
        subscription,
    })
}

/// A Party a device created, and the creator's subscription in it.
struct Founded {
    party: PartyId,
    subscription: SubscriptionId,
}

fn create_party(device: &Device, rng: &mut ChaCha20Rng) -> Result<Founded, TestError> {
    let own = subscription(device.fill())?;
    let created = device
        .parties()
        .create_party(new_party(settings()?, Some(own.clone()))?, rng)?;
    let Outbound::CreateGroup(request) = created.outbound else {
        return Err(TestError::Unexpected("expected a group registration"));
    };
    assert_eq!(request.subscriptions, vec![own.clone()]);
    assert!(request.state.is_some());
    Ok(Founded {
        party: created.party,
        subscription: own.id,
    })
}

fn invite(
    device: &Device,
    party: &PartyId,
    uses: InviteUses,
    rng: &mut ChaCha20Rng,
) -> Result<CreatedInvite, TestError> {
    device.pool_mailbox(MailboxPurpose::Invite, month()?)?;
    Ok(device.parties().create_invite(party, uses, rng)?)
}

/// A join request posted by a joiner.
struct Posted {
    join: JoinId,
    mailbox: MailboxId,
    body: Padded,
    subscription: Subscription,
}

/// The joiner prepares a join for `link` at its clock's time.
fn post_join(joiner: &Device, link: &str, rng: &mut ChaCha20Rng) -> Result<Posted, TestError> {
    joiner.pool_mailbox(MailboxPurpose::Reply, month()?)?;
    let subscription = subscription(joiner.fill())?;
    let prepared = joiner.parties().prepare_join(
        &InviteLink::from_text(link.to_owned()),
        JoinDetails {
            display_name: DisplayName::new("Bob".into())?,
            subscription: subscription.clone(),
        },
        rng,
    )?;
    let Outbound::MailboxPost { mailbox, body } = prepared.outbound else {
        return Err(TestError::Unexpected("expected a mailbox post"));
    };
    Ok(Posted {
        join: prepared.join,
        mailbox,
        body,
        subscription,
    })
}

fn admitted(outcome: JoinRequestOutcome) -> Result<OutgoingCommit, TestError> {
    match outcome {
        JoinRequestOutcome::Admitted(commit) => Ok(commit),
        JoinRequestOutcome::AwaitingApproval(_) | JoinRequestOutcome::Refused(_) => {
            Err(TestError::Unexpected("expected an admitted join"))
        }
    }
}

fn state(commit: &OutgoingCommit) -> Result<SealedSnapshot, TestError> {
    commit
        .request
        .state
        .clone()
        .ok_or(TestError::Unexpected("commit without a snapshot"))
}

/// What the joiner receives for an accepted Add commit.
fn arrived(posted: &Posted, commit: &OutgoingCommit) -> Result<ArrivedWelcome, TestError> {
    let welcome = commit
        .welcome
        .as_ref()
        .ok_or(TestError::Unexpected("commit without a Welcome"))?;
    assert_eq!(welcome.mailbox, posted.join.reply_mailbox());
    Ok(ArrivedWelcome {
        join: posted.join,
        welcome: welcome.body.clone(),
        snapshot: state(commit)?,
    })
}

/// An Add commit and the join request it answers.
struct Admission<'admission> {
    posted: &'admission Posted,
    commit: &'admission OutgoingCommit,
}

/// The inviter's commit is accepted; the joiner takes the Welcome and publishes its profile,
/// which the inviter applies. Returns the joiner's Party id.
fn complete_join(
    inviter: &Device,
    joiner: &Device,
    admission: &Admission<'_>,
    rng: &mut ChaCha20Rng,
) -> Result<PartyId, TestError> {
    inviter.parties().commit_accepted(&admission.commit.party)?;
    let membership = joiner
        .parties()
        .accept_welcome(&arrived(admission.posted, admission.commit)?, rng)?;
    let mut outbound = membership.outbound.into_iter();
    let Some(Outbound::GroupMessage { group, message }) = outbound.next() else {
        return Err(TestError::Unexpected("expected the joiner's profile"));
    };
    assert!(matches!(
        outbound.next(),
        Some(Outbound::DeleteMailbox { mailbox }) if mailbox == admission.posted.join.reply_mailbox()
    ));
    let incoming = inviter
        .parties()
        .process_group_message(&group, &message.body)?;
    assert!(matches!(incoming, Incoming::ProfileUpdated { leaf, .. } if leaf == LEAF_BOB));
    Ok(membership.party)
}

/// Alice created a Party and admitted Bob through a single-use invite.
struct TwoMembers {
    alice: Device,
    bob: Device,
    party: PartyId,
    group: GroupId,
    alice_subscription: SubscriptionId,
    bob_subscription: SubscriptionId,
    rng: ChaCha20Rng,
}

fn two_members() -> Result<TwoMembers, TestError> {
    let mut rng = rng(1);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let founded = create_party(&alice, &mut rng)?;
    let party = founded.party;
    let created = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    let posted = post_join(&bob, created.link.as_str(), &mut rng)?;
    assert_eq!(posted.mailbox, created.invite.mailbox());
    let commit = admitted(alice.parties().receive_join_request(
        created.invite,
        &posted.body,
        &mut rng,
    )?)?;
    assert_eq!(
        commit.request.subscriptions_added,
        vec![posted.subscription.clone()]
    );
    assert_eq!(commit.request.epoch, Epoch::new(0));
    let admission = Admission {
        posted: &posted,
        commit: &commit,
    };
    let joined = complete_join(&alice, &bob, &admission, &mut rng)?;
    assert_eq!(joined, party);
    Ok(TwoMembers {
        alice,
        bob,
        party,
        group: commit.group,
        alice_subscription: founded.subscription,
        bob_subscription: posted.subscription.id,
        rng,
    })
}

fn names(parties: &PartyLifecycle<'_>, party: &PartyId) -> Result<Vec<String>, TestError> {
    Ok(parties
        .roster(party)?
        .into_iter()
        .map(|member| {
            member
                .name
                .map_or_else(String::new, |name| name.as_str().to_owned())
        })
        .collect())
}

#[test]
fn create_invite_join_verify_then_remove_rotates_keys() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        bob,
        party,
        group,
        bob_subscription,
        mut rng,
        ..
    } = two_members()?;
    let alice_parties = alice.parties();
    let bob_parties = bob.parties();

    // Both see both, with names; the joiner learned Alice's name from the snapshot.
    assert_eq!(names(&alice_parties, &party)?, ["Alice", "Bob"]);
    assert_eq!(names(&bob_parties, &party)?, ["Alice", "Bob"]);
    let selves: Vec<Myself> = bob_parties
        .roster(&party)?
        .iter()
        .map(|member| member.is_self)
        .collect();
    assert_eq!(selves, [Myself::No, Myself::Yes]);

    // Safety codes agree on both sides; a wrong code changes nothing, the right one verifies.
    let code = alice_parties.safety_code(&party, LEAF_BOB)?;
    assert_eq!(code, bob_parties.safety_code(&party, LEAF_ALICE)?);
    let wrong = alice_parties.safety_code(&party, LEAF_ALICE)?;
    assert!(matches!(
        alice_parties.verify_member(&party, LEAF_BOB, &wrong),
        Err(PartiesError::SafetyCodeMismatch)
    ));
    let verification = |parties: &PartyLifecycle<'_>| -> Result<_, TestError> {
        Ok(parties
            .roster(&party)?
            .get(1)
            .map(|member| member.verification))
    };
    assert_eq!(
        verification(&alice_parties)?,
        Some(Verification::Unverified)
    );
    let typed = SafetyCode::parse(&code.to_string()).ok_or(TestError::Unexpected("parse"))?;
    alice_parties.verify_member(&party, LEAF_BOB, &typed)?;
    assert_eq!(verification(&alice_parties)?, Some(Verification::Verified));
    assert!(matches!(
        alice_parties.safety_code(&party, LeafIndex::new(7)),
        Err(PartiesError::UnknownMember)
    ));

    // Removal: the commit carries the next epoch's log-auth key and drops Bob's subscription.
    let old_key = alice_parties.log_auth_key(&party)?.public_key();
    assert_eq!(bob_parties.log_auth_key(&party)?.public_key(), old_key);
    let commit = alice_parties.remove_member(&party, LEAF_BOB, &mut rng)?;
    assert_eq!(commit.request.epoch, Epoch::new(1));
    assert_eq!(commit.request.subscriptions_removed, vec![bob_subscription]);
    assert!(commit.welcome.is_none());
    let applied = alice_parties.commit_accepted(&party)?;
    assert_eq!(applied.removed, vec![LEAF_BOB]);
    assert_eq!(applied.transition, EpochTransition::Removal);
    let new_key = alice_parties.log_auth_key(&party)?.public_key();
    assert_ne!(new_key, old_key);
    assert_eq!(commit.request.next_auth_key, new_key);
    assert_eq!(names(&alice_parties, &party)?, ["Alice"]);

    // Bob processes his removal and forgets the Party and its keys.
    let incoming = bob_parties.process_group_message(&group, &commit.request.commit)?;
    assert!(matches!(incoming, Incoming::Removed { party: gone, .. } if gone == party));
    assert!(matches!(
        bob_parties.log_auth_key(&party),
        Err(PartiesError::UnknownParty)
    ));
    assert!(bob.session.parties().list()?.is_empty());

    // Alice's next message is for an epoch whose keys Bob never had.
    let spot = alice_parties.create_spot(&party, new_spot("Pub", circle(150.0)?)?, &mut rng)?;
    let Outbound::GroupMessage { message, .. } = spot.outbound else {
        return Err(TestError::Unexpected("expected a group message"));
    };
    assert_eq!(message.epoch, Epoch::new(2));
    assert!(
        bob_parties
            .process_group_message(&group, &message.body)
            .is_err()
    );
    Ok(())
}

#[test]
fn roster_lookup_epoch_tracks_own_and_incoming_commits() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        bob,
        party,
        group,
        mut rng,
        ..
    } = two_members()?;
    let alice_parties = alice.parties();
    let view = alice_parties.roster_view(&party)?;
    assert_eq!(view.current_epoch(), Epoch::new(1));
    let bob_key = alice_parties
        .roster(&party)?
        .get(1)
        .map(|member| member.signature_key);
    assert!(bob_key.is_some());
    assert_eq!(view.signature_key(LEAF_BOB, Epoch::new(1)), bob_key);
    // Epoch 0's roster is kept as the previous epoch: Bob was not in it.
    assert_eq!(view.signature_key(LEAF_BOB, Epoch::new(0)), None);
    assert!(view.signature_key(LEAF_ALICE, Epoch::new(0)).is_some());

    // A pending commit does not move the epoch; merging does.
    let commit = alice_parties.self_update(&party, &mut rng)?;
    assert_eq!(
        alice_parties.roster_view(&party)?.current_epoch(),
        Epoch::new(1)
    );
    let applied = alice_parties.commit_accepted(&party)?;
    assert_eq!(applied.transition, EpochTransition::Other);
    let view = alice_parties.roster_view(&party)?;
    assert_eq!(view.current_epoch(), Epoch::new(2));
    // Only the current and previous epochs are kept.
    assert_eq!(view.signature_key(LEAF_ALICE, Epoch::new(0)), None);
    assert_eq!(view.signature_key(LEAF_BOB, Epoch::new(1)), bob_key);

    // Bob's view follows the commit he receives.
    let bob_parties = bob.parties();
    assert_eq!(
        bob_parties.roster_view(&party)?.current_epoch(),
        Epoch::new(1)
    );
    let incoming = bob_parties.process_group_message(&group, &commit.request.commit)?;
    assert!(matches!(
        incoming,
        Incoming::RosterChanged(ref applied) if applied.epoch == Epoch::new(2)
    ));
    assert_eq!(
        bob_parties.roster_view(&party)?.current_epoch(),
        Epoch::new(2)
    );
    Ok(())
}

#[test]
fn single_use_invite_admits_once() -> Result<(), TestError> {
    let mut rng = rng(2);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let carol = Device::new(3)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Open
    );
    let bob_post = post_join(&bob, created.link.as_str(), &mut rng)?;
    let carol_post = post_join(&carol, created.link.as_str(), &mut rng)?;
    admitted(
        alice
            .parties()
            .receive_join_request(created.invite, &bob_post.body, &mut rng)?,
    )?;
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Used
    );
    assert!(matches!(
        alice
            .parties()
            .receive_join_request(created.invite, &carol_post.body, &mut rng)?,
        JoinRequestOutcome::Refused(Refusal::AlreadyUsed)
    ));
    Ok(())
}

#[test]
fn invites_expire_after_24_hours() -> Result<(), TestError> {
    let mut rng = rng(3);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Multiple, &mut rng)?;
    let deadline = NOW
        .checked_add(INVITE_LIFETIME)
        .ok_or(TestError::Unexpected("time overflow"))?;
    assert_eq!(created.expires, deadline);
    assert_eq!(created.link.payload()?.expires, deadline);
    let just_before = later(INVITE_LIFETIME.as_secs().saturating_sub(1))?;
    alice.clock.set_wall(just_before);
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Open
    );
    alice.clock.set_wall(deadline);
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Expired
    );

    // The joiner refuses an expired link before posting anything.
    bob.clock.set_wall(deadline);
    assert!(matches!(
        post_join(&bob, created.link.as_str(), &mut rng),
        Err(TestError::Parties(PartiesError::InviteExpired))
    ));
    // A request posted in time but read late is refused by the inviter.
    bob.clock.set_wall(just_before);
    let posted = post_join(&bob, created.link.as_str(), &mut rng)?;
    assert!(matches!(
        alice
            .parties()
            .receive_join_request(created.invite, &posted.body, &mut rng)?,
        JoinRequestOutcome::Refused(Refusal::Expired)
    ));
    let statuses: Vec<InviteStatus> = alice
        .parties()
        .invites(&party)?
        .iter()
        .map(|summary| summary.status)
        .collect();
    assert_eq!(statuses, [InviteStatus::Expired]);
    Ok(())
}

#[test]
fn invite_expires_with_a_shorter_lived_mailbox() -> Result<(), TestError> {
    let mut rng = rng(4);
    let alice = Device::new(1)?;
    let party = create_party(&alice, &mut rng)?.party;
    let mailbox_end = later(3_600)?;
    alice.pool_mailbox(MailboxPurpose::Invite, mailbox_end)?;
    let created = alice
        .parties()
        .create_invite(&party, InviteUses::Single, &mut rng)?;
    assert_eq!(created.expires, mailbox_end);
    // No pooled mailbox left: pingers-sync must register one.
    assert!(matches!(
        alice
            .parties()
            .create_invite(&party, InviteUses::Single, &mut rng),
        Err(PartiesError::NoPooledMailbox)
    ));
    Ok(())
}

#[test]
fn revoked_invite_refuses_and_deletes_its_mailbox() -> Result<(), TestError> {
    let mut rng = rng(5);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Multiple, &mut rng)?;
    let posted = post_join(&bob, created.link.as_str(), &mut rng)?;
    let outbound = alice.parties().revoke_invite(created.invite)?;
    assert!(matches!(
        outbound,
        Outbound::DeleteMailbox { mailbox } if mailbox == created.invite.mailbox()
    ));
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Revoked
    );
    assert!(matches!(
        alice
            .parties()
            .receive_join_request(created.invite, &posted.body, &mut rng)?,
        JoinRequestOutcome::Refused(Refusal::Revoked)
    ));
    Ok(())
}

#[test]
fn join_request_under_another_secret_is_rejected() -> Result<(), TestError> {
    let mut rng = rng(6);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let party = create_party(&alice, &mut rng)?.party;
    let first = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    let second = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    let posted = post_join(&bob, first.link.as_str(), &mut rng)?;
    assert!(matches!(
        alice
            .parties()
            .receive_join_request(second.invite, &posted.body, &mut rng),
        Err(PartiesError::Crypto(_))
    ));
    // The failed request did not use up the second invite.
    assert_eq!(
        alice.parties().invite_status(second.invite)?,
        InviteStatus::Open
    );
    let unknown = InviteId::for_mailbox(MailboxId::from_bytes([0; 16]));
    assert!(matches!(
        alice.parties().invite_status(unknown),
        Err(PartiesError::UnknownInvite)
    ));
    Ok(())
}

#[test]
fn multi_use_invite_waits_for_approval() -> Result<(), TestError> {
    let mut rng = rng(7);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let carol = Device::new(3)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Multiple, &mut rng)?;
    let bob_post = post_join(&bob, created.link.as_str(), &mut rng)?;
    let carol_post = post_join(&carol, created.link.as_str(), &mut rng)?;
    for posted in [&bob_post, &carol_post] {
        let outcome =
            alice
                .parties()
                .receive_join_request(created.invite, &posted.body, &mut rng)?;
        assert!(matches!(
            outcome,
            JoinRequestOutcome::AwaitingApproval(ref pending)
                if pending.join == posted.join && pending.name.as_str() == "Bob"
        ));
    }
    assert_eq!(alice.parties().pending_joins(&party)?.len(), 2);
    alice.parties().decline_join(&party, carol_post.join)?;
    let waiting: Vec<JoinId> = alice
        .parties()
        .pending_joins(&party)?
        .iter()
        .map(|pending| pending.join)
        .collect();
    assert_eq!(waiting, [bob_post.join]);
    assert!(matches!(
        alice.parties().decline_join(&party, carol_post.join),
        Err(PartiesError::UnknownJoin)
    ));
    let commit = alice
        .parties()
        .approve_join(&party, bob_post.join, &mut rng)?;
    assert!(alice.parties().pending_joins(&party)?.is_empty());
    // A multi-use invite stays open after admitting someone.
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Open
    );
    let admission = Admission {
        posted: &bob_post,
        commit: &commit,
    };
    complete_join(&alice, &bob, &admission, &mut rng)?;
    assert_eq!(names(&alice.parties(), &party)?, ["Alice", "Bob"]);
    Ok(())
}

#[test]
fn approval_after_the_invite_expired_is_refused() -> Result<(), TestError> {
    let mut rng = rng(12);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Multiple, &mut rng)?;
    let posted = post_join(&bob, created.link.as_str(), &mut rng)?;
    alice
        .parties()
        .receive_join_request(created.invite, &posted.body, &mut rng)?;
    alice.clock.set_wall(created.expires);
    assert!(matches!(
        alice.parties().approve_join(&party, posted.join, &mut rng),
        Err(PartiesError::InviteExpired)
    ));
    // Still waiting, so the inviter can decline it.
    assert_eq!(alice.parties().pending_joins(&party)?.len(), 1);
    Ok(())
}

#[test]
fn rejected_commit_returns_the_join_to_approval() -> Result<(), TestError> {
    let mut rng = rng(8);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    let posted = post_join(&bob, created.link.as_str(), &mut rng)?;
    admitted(
        alice
            .parties()
            .receive_join_request(created.invite, &posted.body, &mut rng)?,
    )?;
    // One commit at a time.
    assert!(matches!(
        alice.parties().self_update(&party, &mut rng),
        Err(PartiesError::CommitInFlight)
    ));
    alice.parties().commit_rejected(&party)?;
    assert!(matches!(
        alice.parties().commit_rejected(&party),
        Err(PartiesError::NoPendingCommit)
    ));
    assert!(matches!(
        alice.parties().commit_accepted(&party),
        Err(PartiesError::NoPendingCommit)
    ));
    assert_eq!(
        alice.parties().roster_view(&party)?.current_epoch(),
        Epoch::new(0)
    );
    let pending: Vec<JoinId> = alice
        .parties()
        .pending_joins(&party)?
        .iter()
        .map(|join| join.join)
        .collect();
    assert_eq!(pending, [posted.join]);
    // The single-use invite is used up, but its claimed joiner can still be approved.
    assert_eq!(
        alice.parties().invite_status(created.invite)?,
        InviteStatus::Used
    );
    let commit = alice
        .parties()
        .approve_join(&party, posted.join, &mut rng)?;
    let admission = Admission {
        posted: &posted,
        commit: &commit,
    };
    complete_join(&alice, &bob, &admission, &mut rng)?;
    Ok(())
}

#[test]
fn welcome_from_a_group_without_the_inviter_is_refused() -> Result<(), TestError> {
    let mut rng = rng(9);
    let alice = Device::new(1)?;
    let bob = Device::new(2)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    // Bob holds a link naming someone else as the inviter.
    let mut payload = created.link.payload()?;
    payload.inviter_fingerprint = InviterFingerprint::from_bytes([0; 8]);
    let forged = InviteLink::new(&payload)?;
    let posted = post_join(&bob, forged.as_str(), &mut rng)?;
    let commit = admitted(alice.parties().receive_join_request(
        created.invite,
        &posted.body,
        &mut rng,
    )?)?;
    alice.parties().commit_accepted(&party)?;
    assert!(matches!(
        bob.parties()
            .accept_welcome(&arrived(&posted, &commit)?, &mut rng),
        Err(PartiesError::InviterNotInGroup)
    ));
    assert!(bob.session.parties().list()?.is_empty());
    Ok(())
}

#[test]
fn leaving_proposes_removal_and_forgets_the_party() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        bob,
        party,
        group,
        bob_subscription,
        mut rng,
        ..
    } = two_members()?;
    let outbound = bob.parties().leave_party(&party, &mut rng)?;
    let Some(Outbound::Leave(leave)) = outbound.into_iter().next() else {
        return Err(TestError::Unexpected("expected a leave request"));
    };
    assert_eq!(leave.group, group);
    assert_eq!(leave.subscription, Some(bob_subscription));
    assert!(bob.session.parties().list()?.is_empty());

    let alice_parties = alice.parties();
    assert_eq!(
        leave.auth.public_key(),
        alice_parties.log_auth_key(&party)?.public_key()
    );
    let incoming = alice_parties.process_group_message(&group, &leave.proposal.body)?;
    assert!(matches!(incoming, Incoming::LeaveProposed { leaf, .. } if leaf == LEAF_BOB));
    alice_parties.commit_pending_proposals(&party, &mut rng)?;
    let applied = alice_parties.commit_accepted(&party)?;
    assert_eq!(applied.removed, vec![LEAF_BOB]);
    assert_eq!(names(&alice_parties, &party)?, ["Alice"]);
    Ok(())
}

#[test]
fn leaving_deletes_the_party_invites() -> Result<(), TestError> {
    let mut rng = rng(13);
    let alice = Device::new(1)?;
    let party = create_party(&alice, &mut rng)?.party;
    let created = invite(&alice, &party, InviteUses::Multiple, &mut rng)?;
    let outbound = alice.parties().leave_party(&party, &mut rng)?;
    assert!(outbound.iter().any(|request| matches!(
        request,
        Outbound::DeleteMailbox { mailbox } if *mailbox == created.invite.mailbox()
    )));
    assert!(matches!(
        alice.parties().invite_status(created.invite),
        Err(PartiesError::UnknownInvite)
    ));
    Ok(())
}

fn new_spot(name: &str, shape: SpotShape) -> Result<NewSpot, TestError> {
    Ok(NewSpot {
        name: SpotName::new(name.into())?,
        shape,
    })
}

fn circle(radius: f64) -> Result<SpotShape, TestError> {
    Ok(SpotShape::Circle {
        center: Coordinate::new(51.5, -0.12)?,
        radius: Meters::new(radius)?,
    })
}

fn square() -> Result<Vec<Coordinate>, TestError> {
    Ok(vec![
        Coordinate::new(51.500, -0.120)?,
        Coordinate::new(51.502, -0.120)?,
        Coordinate::new(51.502, -0.117)?,
        Coordinate::new(51.500, -0.117)?,
    ])
}

fn group_message(outbound: Outbound) -> Result<MlsMessageBytes, TestError> {
    if let Outbound::GroupMessage { message, .. } = outbound {
        Ok(message.body)
    } else {
        Err(TestError::Unexpected("expected a group message"))
    }
}

#[test]
fn polygon_spot_is_validated_by_geo_and_reaches_other_members() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        bob,
        party,
        group,
        mut rng,
        ..
    } = two_members()?;
    let alice_parties = alice.parties();
    let bob_parties = bob.parties();

    // Invalid shapes never become Spots.
    let mut bow_tie = square()?;
    bow_tie.swap(1, 2);
    assert!(matches!(
        alice_parties.create_spot(
            &party,
            new_spot("Bad", SpotShape::Polygon(bow_tie))?,
            &mut rng
        ),
        Err(PartiesError::Geo(GeoError::SelfIntersecting))
    ));
    let too_many: Vec<Coordinate> = (0..101_u32)
        .map(|index| {
            let angle = f64::from(index) * core::f64::consts::TAU / 101.0;
            Coordinate::new(
                0.01_f64.mul_add(angle.sin(), 51.5),
                0.01_f64.mul_add(angle.cos(), -0.12),
            )
        })
        .collect::<Result<_, _>>()?;
    assert!(matches!(
        alice_parties.create_spot(
            &party,
            new_spot("Bad", SpotShape::Polygon(too_many))?,
            &mut rng
        ),
        Err(PartiesError::Geo(GeoError::TooManyVertices))
    ));
    assert!(matches!(
        alice_parties.create_spot(&party, new_spot("Small", circle(99.0)?)?, &mut rng),
        Err(PartiesError::SpotTooSmall)
    ));
    assert!(alice_parties.spots(&party)?.is_empty());
    // Exactly the minimum radius is allowed.
    alice_parties.create_spot(&party, new_spot("Edge", circle(100.0)?)?, &mut rng)?;

    // A valid polygon is stored and the other member applies the same Spot.
    let created = alice_parties.create_spot(
        &party,
        new_spot("Park", SpotShape::Polygon(square()?))?,
        &mut rng,
    )?;
    let incoming = bob_parties.process_group_message(&group, &group_message(created.outbound)?)?;
    assert!(matches!(incoming, Incoming::SpotChanged { spot, .. } if spot == created.spot));
    let park = |parties: &PartyLifecycle<'_>| -> Result<Option<pingers_store::Spot>, TestError> {
        Ok(parties
            .spots(&party)?
            .into_iter()
            .find(|spot| spot.id == created.spot))
    };
    let mine = park(&alice_parties)?;
    assert_eq!(park(&bob_parties)?, mine);
    assert!(matches!(
        mine.map(|spot| spot.fence),
        Some(Fence::Polygon(_))
    ));

    // Bob edits it into a circle; Alice applies it.
    let edited = bob_parties.edit_spot(
        &party,
        SpotEdit {
            spot: created.spot,
            name: Some(SpotName::new("Big park".into())?),
            shape: Some(circle(300.0)?),
        },
        &mut rng,
    )?;
    alice_parties.process_group_message(&group, &group_message(edited.outbound)?)?;
    let after_edit = park(&alice_parties)?;
    assert_eq!(after_edit, park(&bob_parties)?);
    assert_eq!(
        after_edit.as_ref().map(|spot| spot.name.as_str()),
        Some("Big park")
    );
    assert!(matches!(
        after_edit.map(|spot| spot.fence),
        Some(Fence::Circle(_))
    ));

    // Alice deletes it; Bob applies the tombstone, and nobody can edit it any more.
    let deleted = alice_parties.delete_spot(&party, &created.spot, &mut rng)?;
    bob_parties.process_group_message(&group, &group_message(deleted.outbound)?)?;
    assert_eq!(park(&bob_parties)?, None);
    assert!(matches!(
        bob_parties.delete_spot(&party, &created.spot, &mut rng),
        Err(PartiesError::SpotDeleted)
    ));
    assert!(matches!(
        bob_parties.delete_spot(&party, &SpotId::from_bytes([0; 16]), &mut rng),
        Err(PartiesError::UnknownSpot)
    ));
    Ok(())
}

#[test]
fn concurrent_spot_edits_settle_on_the_later_one() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        bob,
        party,
        group,
        mut rng,
        ..
    } = two_members()?;
    let alice_parties = alice.parties();
    let bob_parties = bob.parties();
    let created = alice_parties.create_spot(&party, new_spot("Pub", circle(150.0)?)?, &mut rng)?;
    bob_parties.process_group_message(&group, &group_message(created.outbound)?)?;

    // Alice edits twice; Bob edits once from the older state, so his edit has the lower clock.
    let rename = |name: &str| -> Result<SpotEdit, TestError> {
        Ok(SpotEdit {
            spot: created.spot,
            name: Some(SpotName::new(name.into())?),
            shape: None,
        })
    };
    let first = alice_parties.edit_spot(&party, rename("A1")?, &mut rng)?;
    let second = alice_parties.edit_spot(&party, rename("A2")?, &mut rng)?;
    let bobs = bob_parties.edit_spot(&party, rename("B")?, &mut rng)?;
    // The log orders Alice's two edits before Bob's.
    bob_parties.process_group_message(&group, &group_message(first.outbound)?)?;
    let incoming = bob_parties.process_group_message(&group, &group_message(second.outbound)?)?;
    assert!(matches!(incoming, Incoming::SpotChanged { .. }));
    let incoming = alice_parties.process_group_message(&group, &group_message(bobs.outbound)?)?;
    assert!(matches!(incoming, Incoming::Superseded { .. }));
    let name = |parties: &PartyLifecycle<'_>| -> Result<Option<String>, TestError> {
        Ok(parties
            .spots(&party)?
            .first()
            .map(|spot| spot.name.as_str().to_owned()))
    };
    assert_eq!(name(&alice_parties)?.as_deref(), Some("A2"));
    assert_eq!(name(&bob_parties)?.as_deref(), Some("A2"));
    Ok(())
}

#[test]
fn per_party_settings_are_kept_and_changed() -> Result<(), TestError> {
    let mut rng = rng(10);
    let alice = Device::new(1)?;
    let relays = RelayChoice::Custom(vec![RelayUrl::new("https://relay.example".into())?]);
    let created = alice.parties().create_party(
        new_party(
            PartySettings {
                relays: relays.clone(),
                location_visibility: Some(LocationVisibility::Nobody),
                ..settings()?
            },
            None,
        )?,
        &mut rng,
    )?;
    let party = created.party;
    let parties = alice.parties();
    let stored = parties.settings(&party)?;
    assert_eq!(stored.server.as_str(), SERVER);
    assert_eq!(stored.relays, relays);
    assert_eq!(stored.post_quantum, PostQuantum::Classical);
    assert_eq!(stored.location_visibility, Some(LocationVisibility::Nobody));
    assert_eq!(
        parties.effective_location_visibility(&party)?,
        LocationVisibility::Nobody
    );

    parties.set_relays(&party, RelayChoice::ServerDefault)?;
    parties.set_location_visibility(&party, None)?;
    alice
        .session
        .settings()
        .set_location_visibility(LocationVisibility::Everyone)?;
    let stored = parties.settings(&party)?;
    assert_eq!(stored.relays, RelayChoice::ServerDefault);
    assert_eq!(stored.location_visibility, None);
    assert_eq!(
        parties.effective_location_visibility(&party)?,
        LocationVisibility::Everyone
    );

    let unknown = PartyId::from_bytes([0; 16]);
    assert!(matches!(
        parties.settings(&unknown),
        Err(PartiesError::UnknownParty)
    ));
    assert!(matches!(
        parties.set_relays(&unknown, RelayChoice::ServerDefault),
        Err(PartiesError::UnknownParty)
    ));
    assert!(matches!(
        parties.set_location_visibility(&unknown, None),
        Err(PartiesError::UnknownParty)
    ));
    Ok(())
}

#[test]
fn quantum_resistant_party_needs_the_x_wing_build() -> Result<(), TestError> {
    let mut rng = rng(11);
    let alice = Device::new(1)?;
    let result = alice.parties().create_party(
        new_party(
            PartySettings {
                post_quantum: PostQuantum::QuantumResistant,
                ..settings()?
            },
            None,
        )?,
        &mut rng,
    );
    assert!(matches!(
        result,
        Err(PartiesError::Mls(pingers_mls::MlsError::UnsupportedSuite))
    ));
    // Nothing of the failed Party was kept.
    assert!(alice.session.parties().list()?.is_empty());
    Ok(())
}

#[test]
fn joiner_settings_come_from_the_link() -> Result<(), TestError> {
    let TwoMembers { bob, party, .. } = two_members()?;
    let settings = bob.parties().settings(&party)?;
    assert_eq!(settings.server.as_str(), SERVER);
    assert_eq!(settings.relays, RelayChoice::ServerDefault);
    assert_eq!(settings.post_quantum, PostQuantum::Classical);
    Ok(())
}

#[test]
fn third_member_learns_every_name_from_the_snapshot() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        bob,
        party,
        group,
        mut rng,
        ..
    } = two_members()?;
    let carol = Device::new(3)?;
    let created = invite(&alice, &party, InviteUses::Single, &mut rng)?;
    let posted = post_join(&carol, created.link.as_str(), &mut rng)?;
    let commit = admitted(alice.parties().receive_join_request(
        created.invite,
        &posted.body,
        &mut rng,
    )?)?;
    alice.parties().commit_accepted(&party)?;
    bob.parties()
        .process_group_message(&group, &commit.request.commit)?;
    carol
        .parties()
        .accept_welcome(&arrived(&posted, &commit)?, &mut rng)?;
    // Every leaf's stored profile carries that leaf's own key.
    let roster = carol.parties().roster(&party)?;
    let keys: Vec<_> = roster.iter().map(|member| member.signature_key).collect();
    let expected: Vec<_> = alice
        .parties()
        .roster(&party)?
        .iter()
        .map(|member| member.signature_key)
        .collect();
    assert_eq!(keys, expected);
    let named: Vec<bool> = roster.iter().map(|member| member.name.is_some()).collect();
    assert_eq!(named, [true, true, true]);
    let stored: Vec<_> = carol
        .session
        .members()
        .list(&party)?
        .into_iter()
        .map(|member| member.signature_key)
        .collect();
    assert_eq!(stored, keys);
    Ok(())
}

#[test]
fn creator_keeps_its_subscription_when_members_join() -> Result<(), TestError> {
    let TwoMembers {
        alice,
        party,
        alice_subscription,
        mut rng,
        ..
    } = two_members()?;
    let outbound = alice.parties().leave_party(&party, &mut rng)?;
    let Some(Outbound::Leave(leave)) = outbound.into_iter().next() else {
        return Err(TestError::Unexpected("expected a leave request"));
    };
    assert_eq!(leave.subscription, Some(alice_subscription));
    Ok(())
}

#[test]
fn leaving_one_party_keeps_the_others_records() -> Result<(), TestError> {
    use crate::records::{Key, read};

    let mut rng = rng(14);
    let alice = Device::new(1)?;
    let first = create_party(&alice, &mut rng)?.party;
    let second = create_party(&alice, &mut rng)?.party;
    assert_ne!(first, second);
    alice.parties().leave_party(&first, &mut rng)?;
    for key in [
        Key::Settings(&first),
        Key::Epochs(&first),
        Key::Subscriptions(&first),
    ] {
        assert!(read(&alice.session, &key)?.is_none());
    }
    assert_eq!(alice.parties().settings(&second)?.server.as_str(), SERVER);
    assert_eq!(
        alice.parties().roster_view(&second)?.current_epoch(),
        Epoch::new(0)
    );
    Ok(())
}

/// A Spot with `created_by` set to `creator`, as another member might send it.
fn foreign_spot(
    party: PartyId,
    creator: pingers_crypto::AccountPublicKey,
) -> Result<pingers_store::Spot, TestError> {
    Ok(pingers_store::Spot {
        id: SpotId::from_bytes([9; 16]),
        party,
        name: SpotName::new("Pub".into())?,
        fence: crate::test_support::circle_fence(150.0)?,
        created_by: creator,
        lamport: pingers_proto::Lamport::new(1),
        status: pingers_store::SpotStatus::Active,
    })
}

#[test]
fn operations_that_misattribute_spots_or_profiles_are_refused() -> Result<(), TestError> {
    use crate::lifecycle::{Sender, apply_operation};
    use crate::ops::PartyOperation;

    let TwoMembers {
        alice,
        party,
        mut rng,
        ..
    } = two_members()?;
    let session = &alice.session;
    let row = session
        .parties()
        .get(&party)?
        .ok_or(TestError::Unexpected("party"))?;
    let roster = alice.parties().roster(&party)?;
    let alice_key = roster
        .first()
        .map(|member| member.signature_key)
        .ok_or(TestError::Unexpected("alice"))?;
    let bob_key = roster
        .get(1)
        .map(|member| member.signature_key)
        .ok_or(TestError::Unexpected("bob"))?;
    let bob = Sender {
        leaf: LEAF_BOB,
        key: bob_key,
    };
    let malformed = |result: Result<Incoming, PartiesError>| {
        matches!(result, Err(PartiesError::Malformed { .. }))
    };

    // A new Spot must be created by its sender.
    let spot = foreign_spot(party, alice_key)?;
    assert!(malformed(apply_operation(
        session,
        &row,
        bob,
        PartyOperation::Spot(spot.clone())
    )));
    // An edit may not change the creator.
    let mut owned = spot;
    owned.created_by = bob_key;
    apply_operation(session, &row, bob, PartyOperation::Spot(owned.clone()))?;
    let mut stolen = owned.clone();
    stolen.created_by = alice_key;
    stolen.lamport = pingers_proto::Lamport::new(5);
    assert!(malformed(apply_operation(
        session,
        &row,
        bob,
        PartyOperation::Spot(stolen)
    )));
    // A Spot filed under another Party is not this Party's.
    let other = create_party(&alice, &mut rng)?.party;
    let other_row = session
        .parties()
        .get(&other)?
        .ok_or(TestError::Unexpected("party"))?;
    let mut moved = owned;
    moved.party = other;
    moved.lamport = pingers_proto::Lamport::new(6);
    assert!(malformed(apply_operation(
        session,
        &other_row,
        bob,
        PartyOperation::Spot(moved)
    )));

    // A profile keeps the verification of the same key, and drops it for a new key.
    let code = alice.parties().safety_code(&party, LEAF_BOB)?;
    alice.parties().verify_member(&party, LEAF_BOB, &code)?;
    let profile = || -> Result<PartyOperation, TestError> {
        Ok(PartyOperation::Profile {
            name: MemberName::new("Robert".into())?,
            subscription: None,
        })
    };
    apply_operation(session, &row, bob, profile()?)?;
    let verification = || -> Result<_, TestError> {
        Ok(session
            .members()
            .get(&party, LEAF_BOB)?
            .map(|member| member.verification))
    };
    assert_eq!(verification()?, Some(Verification::Verified));
    let someone_else = Sender {
        leaf: LEAF_BOB,
        key: pingers_crypto::AccountPublicKey::from_bytes([7; 32]),
    };
    apply_operation(session, &row, someone_else, profile()?)?;
    assert_eq!(verification()?, Some(Verification::Unverified));
    Ok(())
}
