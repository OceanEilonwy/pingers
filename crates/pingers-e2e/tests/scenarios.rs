// SPDX-License-Identifier: AGPL-3.0-only
//! Design G.3 to G.7 end to end: simulated devices built from the protocol crates, the real
//! pingers-server, every anonymous request through the OHTTP relay.

mod common;

use common::{TestResult, World, check};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g3_entering_a_spot_alone_matches_nothing() -> TestResult {
    let mut world = World::new(300).await?;
    world.g3_alice_alone().await?;
    // Bob is in the Party but Outside: he holds no Spot keys and matches nothing (design B.4.3,
    // "match only where you are"), even though Alice's real record is on the board.
    let report = world.bob.sync(&world.stack, true).await?;
    check(
        !world.bob.holds_spot_keys(),
        "Bob holds no Spot keys outside",
    )?;
    check(
        report.matches.is_empty(),
        "a reader outside matches nothing",
    )?;
    check(report.records.len() == 8, "two leases on the board")?;
    check(
        world.recorder.exchanges().iter().all(|exchange| {
            exchange.path.starts_with("/v1/board/") || exchange.path.starts_with("/v1/mailboxes")
        }),
        "only anonymous board and mailbox routes were used",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g4_the_second_person_knocks_and_a_meetup_is_created() -> TestResult {
    let mut world = World::new(400).await?;
    world.g3_alice_alone().await?;
    world.g4_bob_meets_alice().await.map(drop)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g4_a_stale_poster_declines_and_nothing_forms() -> TestResult {
    let mut world = World::new(410).await?;
    world.g3_alice_alone().await?;
    world.bob.arrive(&world.stack, world.place).await?;
    let report = world.bob.sync(&world.stack, true).await?;
    check(
        world.bob.knock(&world.stack, &report.matches).await? == 1,
        "Bob knocks",
    )?;
    // Alice walked off before the knock reached her: her keys are gone, nothing is opened.
    world.alice.depart(&world.stack).await?;
    check(
        world.alice.answer().await?.is_empty(),
        "a departed poster answers nothing",
    )?;
    check(!world.bob.join_meetup().await?, "no Welcome arrives")?;
    check(
        world
            .stack
            .delivery()
            .messages(
                &pingers_proto::GroupId::from_bytes([0; 32]),
                pingers_proto::SequenceNumber::new(0),
                pingers_delivery::Reader::Current,
            )
            .is_err(),
        "no group exists",
    )
}

/// Finding: a Meetup group is at MLS epoch 1 once its creator merges the first commit (which
/// adds the knocker and injects the PSK), but pingers-delivery creates every group with its log
/// head at epoch 0 and `CreateGroup` has no epoch field. Following design G.4 as drawn
/// (`POST /groups` and the Welcome, no commit in the log), every later commit (G.5's Add,
/// G.6's and G.7's Remove) is refused with 409 for good. The other scenarios work around it by
/// posting the creation commit at epoch 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g5_after_g4_as_drawn_the_first_add_is_accepted() -> TestResult {
    let mut world = World::new(510).await?;
    world.g3_alice_alone().await?;
    world.bob.arrive(&world.stack, world.place).await?;
    let report = world.bob.sync(&world.stack, true).await?;
    world.bob.knock(&world.stack, &report.matches).await?;
    let answers = world.alice.answer().await?;
    let answer = answers
        .first()
        .ok_or_else(|| pingers_e2e::E2eError::step("no knock"))?;
    world
        .alice
        .create_meetup_as_drawn(&world.stack, answer)
        .await?;
    check(world.bob.join_meetup().await?, "Bob joins")?;

    world.alice.sync(&world.stack, true).await?;
    world.carol.arrive(&world.stack, world.place).await?;
    let found = world.carol.sync(&world.stack, true).await?;
    world.carol.knock(&world.stack, &found.matches).await?;
    let answers = world.alice.answer().await?;
    let answer = answers
        .last()
        .ok_or_else(|| pingers_e2e::E2eError::step("no knock"))?;
    check(
        world.alice.add_to_meetup(&world.stack, answer).await?
            == pingers_e2e::KnockOutcome::Accepted,
        "the only commit for the members' epoch is accepted",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g5_a_third_person_joins_and_one_commit_wins() -> TestResult {
    let mut world = World::new(500).await?;
    world.g3_alice_alone().await?;
    let group = world.g4_bob_meets_alice().await?;
    world.g5_carol_joins(group).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g6_someone_leaves_and_the_key_rotates() -> TestResult {
    let mut world = World::new(600).await?;
    world.g3_alice_alone().await?;
    let group = world.g4_bob_meets_alice().await?;
    world.g5_carol_joins(group).await?;
    world.g6_bob_leaves(group).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn g7_everyone_leaves_and_the_meetup_ends() -> TestResult {
    let mut world = World::new(700).await?;
    world.g3_alice_alone().await?;
    let group = world.g4_bob_meets_alice().await?;
    world.g5_carol_joins(group).await?;
    world.g6_bob_leaves(group).await?;
    world.g7_everyone_leaves(group).await?;
    check(
        world
            .stack
            .delivery()
            .messages(
                &group,
                pingers_proto::SequenceNumber::new(0),
                pingers_delivery::Reader::Current,
            )
            .is_err(),
        "the delivery service no longer knows the group",
    )
}
