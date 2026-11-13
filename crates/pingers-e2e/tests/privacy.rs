// SPDX-License-Identifier: AGPL-3.0-only
//! Design A.2 checked against what the server actually stored and counted after G.3 to G.7:
//! no device addresses on anonymous routes, a blind board of fixed-size unlinkable records, no
//! plaintext push tokens, mailboxes without accounts, no coordinates anywhere, and nothing of
//! the Meetup left after G.7.

mod common;

use std::collections::HashSet;
use std::time::Duration;

use common::{LATITUDE, LONGITUDE, TestResult, World, check};
use pingers_crypto::{PushTokenToOpen, open_push_token, push_bind_for_mailbox};
use pingers_e2e::inspect::{
    address_patterns, all_values, columns, contains, coordinate_patterns, database_bytes,
    row_count, tables,
};
use pingers_e2e::{DEVICE_ADDRESS, Device, E2eError, Exchange, RELAY_EGRESS};
use pingers_proto::{
    BoardPage, BoardSyncRequest, BoardSyncResponse, GroupId, MeetupHint, PushRelayKeyId,
    SealedPushBlob, WireDecode as _,
};

/// Runs G.3 to G.7 and returns the world and the Meetup's group id.
async fn played(seed: u64) -> Result<(World, GroupId), E2eError> {
    let mut world = World::new(seed).await?;
    world.g3_alice_alone().await?;
    let group = world.g4_bob_meets_alice().await?;
    world.g5_carol_joins(group).await?;
    world.g6_bob_leaves(group).await?;
    world.g7_everyone_leaves(group).await?;
    Ok((world, group))
}

fn devices(world: &World) -> [&Device; 3] {
    [&world.alice, &world.bob, &world.carol]
}

/// Every byte the devices sent to or got back from the server, through the relay.
fn traffic(exchanges: &[Exchange]) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    for exchange in exchanges {
        chunks.push(exchange.path.clone().into_bytes());
        for (name, value) in &exchange.headers {
            chunks.push(name.clone().into_bytes());
            chunks.push(value.clone());
        }
        chunks.extend(exchange.request_body.clone());
        chunks.extend(exchange.response_body.clone());
    }
    chunks
}

fn metric(metrics: &str, channel: &str, outcome: &str) -> Option<u64> {
    let prefix = format!("pingers_requests_total{{channel=\"{channel}\",outcome=\"{outcome}\"}} ");
    metrics
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .and_then(|count| count.trim().parse().ok())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anonymous_routes_never_see_a_device_address() -> TestResult {
    let (world, _group) = played(800).await?;
    let tap = world.stack.tap();
    check(tap.connections() > 0, "traffic went through the relay")?;
    check(
        tap.egress().iter().all(|address| *address == RELAY_EGRESS),
        "every connection the server saw on its gateway came from the relay's address",
    )?;
    check(
        tap.sources()
            .iter()
            .all(|address| *address == DEVICE_ADDRESS),
        "only the relay connected to the tap",
    )?;

    // The server counted every anonymous request on the OHTTP channel and none direct.
    let metrics = world.stack.metrics().await?;
    let oblivious_ok = metric(&metrics, "ohttp", "ok").unwrap_or(0);
    let requests = u64::try_from(world.recorder.exchanges().len()).unwrap_or(0);
    check(
        oblivious_ok == requests,
        &format!("{oblivious_ok} OHTTP requests counted, {requests} sent"),
    )?;
    // The only direct requests are the relay fetching the public key config, once per device
    // transport (from the relay's address, through the tap).
    check(
        metric(&metrics, "direct", "ok") == Some(3),
        &format!("direct requests other than the key config: {metrics}"),
    )?;
    for outcome in ["client_error", "not_implemented", "server_error"] {
        check(
            metric(&metrics, "direct", outcome) == Some(0),
            &format!("refused direct requests: {metrics}"),
        )?;
    }
    // The metrics are counters only: no addresses, ids or paths.
    for line in metrics.lines() {
        check(
            line.starts_with('#') || line.starts_with("pingers_requests_total{channel="),
            &format!("unexpected metrics line {line}"),
        )?;
    }
    for pattern in address_patterns() {
        check(
            !contains(metrics.as_bytes(), &pattern),
            "an address in the metrics",
        )?;
    }

    // A device that skipped the relay is refused, on the route itself and on the gateway.
    let probe = world
        .stack
        .direct(
            "POST",
            "/v1/board/sync",
            Some("application/cbor"),
            vec![0; 8],
        )
        .await?;
    check(
        probe.0 == 421,
        &format!("direct board sync answered {}", probe.0),
    )?;
    let probe = world
        .stack
        .direct(
            "POST",
            "/v1/mailboxes",
            Some("application/cbor"),
            vec![0; 8],
        )
        .await?;
    check(
        probe.0 == 421,
        &format!("direct mailbox registration answered {}", probe.0),
    )?;
    let probe = world
        .stack
        .direct("POST", "/ohttp", Some("message/ohttp-req"), vec![0; 64])
        .await?;
    check(
        (400..500).contains(&probe.0),
        &format!("the gateway answered a device directly with {}", probe.0),
    )?;

    // No address in anything the server keeps.
    for database in [
        world.stack.mailbox_database(),
        world.stack.delivery_database(),
    ] {
        for value in all_values(&database)? {
            for pattern in address_patterns() {
                check(!contains(&value, &pattern), "an address in a stored value")?;
            }
        }
        let raw = database_bytes(&database)?;
        for address in [DEVICE_ADDRESS, RELAY_EGRESS] {
            check(
                !contains(&raw, address.to_string().as_bytes()),
                "an address in a database file",
            )?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn board_records_are_fixed_size_and_unlinkable() -> TestResult {
    let (world, group) = played(810).await?;
    let exchanges = world.recorder.exchanges();
    let mut leases = Vec::new();
    let mut fresh_upload_sizes = HashSet::new();
    let mut replacing_upload_sizes = HashSet::new();
    let mut lease_ids = HashSet::new();
    let mut read_records = Vec::new();
    for exchange in &exchanges {
        let (Some(request), Some(response)) = (&exchange.request_body, &exchange.response_body)
        else {
            continue;
        };
        if exchange.path == "/v1/board/sync" {
            let sync = BoardSyncRequest::decode(request)?;
            if sync.replaces.is_some() {
                replacing_upload_sizes.insert(request.len());
            } else {
                fresh_upload_sizes.insert(request.len());
            }
            leases.push(sync.lease);
            let answer = BoardSyncResponse::decode(response)?;
            check(lease_ids.insert(answer.lease_id), "a lease id repeated")?;
            read_records.extend(answer.page.map(|page| page.records).unwrap_or_default());
        } else if exchange.path.starts_with("/v1/board/page") {
            read_records.extend(BoardPage::decode(response)?.records);
        }
    }
    check(
        leases.len() >= 7,
        &format!("the scenarios posted {} leases", leases.len()),
    )?;
    // Uniform behaviour (design B.4.6): every upload has the same size whether its records are
    // real or dummy, Meetup hint or not.
    check(
        fresh_upload_sizes.len() == 1 && replacing_upload_sizes.len() <= 1,
        &format!("upload sizes vary: {fresh_upload_sizes:?} {replacing_upload_sizes:?}"),
    )?;

    // Every record is exactly 96 bytes, and no two share their randomness or tag: nothing ties
    // one record to another, not even two records of the same device or Spot.
    let mut randomness = HashSet::new();
    let mut tags = HashSet::new();
    for lease in &leases {
        check(lease.records.len() == 4, "a lease of four records")?;
        for record in &lease.records {
            check(record.to_bytes().len() == 96, "a 96-byte record")?;
            check(
                randomness.insert(record.randomness),
                "record randomness repeated",
            )?;
            check(tags.insert(record.tag), "record tag repeated")?;
        }
    }

    // The board names nobody: no mailbox, group, hint, Party, Spot or account in any record
    // anyone read.
    let mut names: Vec<Vec<u8>> = vec![
        group.as_bytes().to_vec(),
        MeetupHint::for_group(&group).as_bytes().to_vec(),
        world.place.party.as_bytes().to_vec(),
        world.place.spot.as_bytes().to_vec(),
    ];
    for device in devices(&world) {
        names.push(device.account_key().as_bytes().to_vec());
        for mailbox in device.registered_mailboxes() {
            names.push(mailbox.as_bytes().to_vec());
        }
    }
    check(!read_records.is_empty(), "records were read")?;
    for record in &read_records {
        let bytes = record.to_bytes();
        for name in &names {
            check(!contains(&bytes, name), "a board record names something")?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_holds_no_plaintext_push_tokens() -> TestResult {
    let mut world = World::new(820).await?;
    world.g3_alice_alone().await?;
    let group = world.g4_bob_meets_alice().await?;
    world.g5_carol_joins(group).await?;

    // Mid-Meetup: mailboxes and subscriptions are live.
    let mailboxes = world.stack.mailbox_database();
    let delivery = world.stack.delivery_database();
    let stored = [database_bytes(&mailboxes)?, database_bytes(&delivery)?];
    let sent = traffic(&world.recorder.exchanges());
    for device in devices(&world) {
        let token = device.push_token();
        for bytes in stored.iter().chain(&sent) {
            check(
                !contains(bytes, token),
                &format!("{}'s push token in plaintext", device.name()),
            )?;
        }
    }

    // What is stored is a real sealed token that only the push relay opens, bound to its mailbox.
    let alice_mailbox = world
        .alice
        .knock_mailbox()
        .ok_or_else(|| E2eError::step("Alice has a knock mailbox"))?;
    let connection = rusqlite::Connection::open_with_flags(
        &mailboxes,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let (blob, key_id): (Vec<u8>, i64) = connection.query_row(
        "SELECT push_blob, relay_key_id FROM mailbox WHERE mailbox_id = ?1",
        [alice_mailbox.as_bytes().as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let blob = SealedPushBlob::from_bytes(
        blob.try_into()
            .map_err(|_| E2eError::step("a 320-byte blob"))?,
    );
    let key_id = u8::try_from(key_id).map_err(|_| E2eError::step("key id"))?;
    let opened = open_push_token(
        world.stack.push_relay_private_key(),
        &PushTokenToOpen {
            key_id,
            bind: push_bind_for_mailbox(&alice_mailbox),
            blob: &blob,
        },
    )?;
    check(
        opened.token.as_bytes() == world.alice.push_token(),
        "the relay opens Alice's token",
    )?;
    check(
        PushRelayKeyId::new(key_id) == PushRelayKeyId::new(1),
        "sealed to the pinned relay key",
    )?;
    let other = world
        .bob
        .knock_mailbox()
        .ok_or_else(|| E2eError::step("Bob has a knock mailbox"))?;
    check(
        open_push_token(
            world.stack.push_relay_private_key(),
            &PushTokenToOpen {
                key_id,
                bind: push_bind_for_mailbox(&other),
                blob: &blob,
            },
        )
        .is_err(),
        "the blob does not open for another mailbox",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailboxes_have_no_accounts() -> TestResult {
    let mut world = World::new(830).await?;
    world.g3_alice_alone().await?;
    let group = world.g4_bob_meets_alice().await?;
    world.g5_carol_joins(group).await?;
    let mailboxes = world.stack.mailbox_database();
    check(
        tables(&mailboxes)? == ["mailbox", "mailbox_msg"],
        "only the mailbox tables",
    )?;
    let allowed = [
        "mailbox_id",
        "expires_at",
        "last_seq",
        "relay_id",
        "relay_url",
        "relay_key_hash",
        "relay_key_id",
        "push_blob",
        "urgency",
        "handle",
        "seq",
        "body",
    ];
    for table in ["mailbox", "mailbox_msg"] {
        for column in columns(&mailboxes, table)? {
            check(
                allowed.contains(&column.as_str()),
                &format!("unexpected mailbox column {column}"),
            )?;
        }
    }
    check(
        row_count(&mailboxes, "mailbox")? > 0,
        "mailboxes are stored",
    )?;
    // No account key, in any table of either database, in plaintext.
    let stored = [
        database_bytes(&mailboxes)?,
        database_bytes(&world.stack.delivery_database())?,
    ];
    for device in devices(&world) {
        let account = device.account_key();
        for bytes in &stored {
            check(
                !contains(bytes, account.as_bytes()),
                &format!("{}'s account key is stored", device.name()),
            )?;
        }
    }
    // Mailboxes registered by one device cannot be grouped: nothing but their ids differ in
    // kind, and every sealed blob is distinct.
    let connection = rusqlite::Connection::open_with_flags(
        &mailboxes,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = connection.prepare("SELECT push_blob, handle FROM mailbox")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let blobs: HashSet<&Vec<u8>> = rows.iter().map(|(blob, _)| blob).collect();
    let handles: HashSet<&Vec<u8>> = rows.iter().map(|(_, handle)| handle).collect();
    check(
        blobs.len() == rows.len() && handles.len() == rows.len(),
        "a sealed blob or handle repeats across mailboxes",
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_coordinates_anywhere() -> TestResult {
    let (world, _group) = played(840).await?;
    let mut patterns = coordinate_patterns(LATITUDE, LONGITUDE);
    // Where everyone walked off to (`Place::away`).
    patterns.extend(coordinate_patterns(LATITUDE + 0.02, LONGITUDE));
    let mut haystacks = traffic(&world.recorder.exchanges());
    haystacks.push(database_bytes(&world.stack.mailbox_database())?);
    haystacks.push(database_bytes(&world.stack.delivery_database())?);
    haystacks.push(world.stack.metrics().await?.into_bytes());
    for haystack in &haystacks {
        for pattern in &patterns {
            check(
                !contains(haystack, pattern),
                "a coordinate left the devices",
            )?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_of_the_meetup_is_left_after_g7() -> TestResult {
    let (mut world, group) = played(850).await?;

    // The group is gone from the delivery service: no row in any table names it.
    let delivery = world.stack.delivery_database();
    for table in tables(&delivery)? {
        check(
            row_count(&delivery, &table)? == 0,
            &format!("delivery table {table} still has rows"),
        )?;
    }
    for value in all_values(&delivery)? {
        check(
            !contains(&value, group.as_bytes()),
            "a stored value names the group",
        )?;
    }

    // Every mailbox was deleted, with its posts (knocks and Welcomes).
    let mailboxes = world.stack.mailbox_database();
    check(row_count(&mailboxes, "mailbox")? == 0, "mailboxes left")?;
    check(
        row_count(&mailboxes, "mailbox_msg")? == 0,
        "mailbox posts left",
    )?;

    // A newcomer inside the Spot finds no record of anyone.
    let mut dave = Device::new(
        "dave",
        851,
        &world.stack,
        std::sync::Arc::clone(&world.recorder),
    )?;
    pingers_e2e::form_party(&mut [&mut dave], 852)?;
    let fresh = dave.sync(&world.stack, true).await?;
    check(
        fresh.records.len() == 8,
        &format!(
            "only Bob's dummy lease and Dave's own remain, not {}",
            fresh.records.len()
        ),
    )?;

    // Four buckets on, the sweep has dropped every lease but the newest.
    world.stack.advance(Duration::from_mins(61)).await;
    let later = dave.sync(&world.stack, true).await?;
    check(later.records.len() == 4, "only Dave's new lease is live")?;
    dave.delete_mailboxes().await?;
    for device in [&mut world.alice, &mut world.bob, &mut world.carol] {
        check(
            !device.has_group_state(&group)?,
            "a device kept group state",
        )?;
    }
    Ok(())
}

/// Deleted Meetup rows must not linger in the database files (design G.7: "drop `grp`, `grp_msg`,
/// `grp_sub`, `grp_state` for G").
///
/// Finding: pingers-mailbox opens its database with `PRAGMA secure_delete = ON`, but
/// pingers-delivery's `SqliteDeliveryStore` sets no pragmas, so a deleted group's id, log
/// entries and sealed subscriptions stay in free pages of the file until overwritten.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "finding: SqliteDeliveryStore does not set secure_delete; deleted groups linger on disk"]
async fn deleted_meetup_bytes_do_not_linger_on_disk() -> TestResult {
    let (world, group) = played(860).await?;
    let raw = database_bytes(&world.stack.delivery_database())?;
    check(
        !contains(&raw, group.as_bytes()),
        "the deleted group's id is still in the delivery database file",
    )?;
    let raw = database_bytes(&world.stack.mailbox_database())?;
    for device in devices(&world) {
        for mailbox in device.registered_mailboxes() {
            check(
                !contains(&raw, mailbox.as_bytes()),
                "a deleted mailbox id is still in the mailbox database file",
            )?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_posts_come_in_fixed_size_classes() -> TestResult {
    let (world, _group) = played(870).await?;
    let mut sizes = HashSet::new();
    let mut registrations = HashSet::new();
    for exchange in world.recorder.exchanges() {
        if exchange.method == "POST" && exchange.path.ends_with("/msgs") {
            sizes.insert(exchange.request_len);
        } else if exchange.method == "POST" && exchange.path == "/v1/mailboxes" {
            registrations.insert(exchange.request_len);
        }
    }
    // Knocks are padded to 1 KiB and Welcomes to a mailbox class: the server sees only sizes
    // from the fixed list (design D), never the content's length.
    check(!sizes.is_empty(), "mailbox posts were sent")?;
    check(
        sizes.iter().all(|size| {
            pingers_proto::SizeClass::MAILBOX_POST
                .iter()
                .any(|class| class.size() == *size)
        }),
        &format!("a mailbox post outside the size classes: {sizes:?}"),
    )?;
    // Knock and reply mailboxes look alike when registered.
    check(
        registrations.len() == 1,
        &format!("mailbox registrations differ in size: {registrations:?}"),
    )
}
