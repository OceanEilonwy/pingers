// SPDX-License-Identifier: AGPL-3.0-only
//! The group routes, each one call into pingers-delivery after the Anon and Grp checks
//! (`credential.rs`); wakes go to the push jobs, fan-out targets get the message as a live
//! frame.
//!
//! Every route handler here may block (SQLite, proof verification): `dispatch.rs` runs them on
//! the blocking pool.
//!
//! Wire encodings: `POST /v1/groups?lifetime=meetup|party&epoch=n` (`lifetime` the lifetime
//! class, design A.2, Meetup when absent; `epoch` the MLS epoch the members are in at upload, decimal,
//! 0 when absent: a Meetup group is uploaded at epoch 1, after the commit its Welcome
//! carries, design G.4); `{id}` is the group id in hex, `{sub_id}` the subscription id; `?after=` is a
//! decimal sequence number (0 when absent). Messages forwarded on live channels are
//! pingers-proto `LiveFrame`s: commits and stored posts as `Stored`, ephemeral posts as
//! `Ephemeral`.

use axum::http::StatusCode;
use pingers_delivery::{
    CommitOutcome, Delivery, DeliveryError, GroupLifetime, GroupStart, SqliteDeliveryStore, Wake,
};
use pingers_ohttp::BinaryRequest;
use pingers_proto::{
    AppMessage, CommitRequest, CreateGroup, Delivery as Mode, Epoch, GroupId, LiveFrame,
    MessageAccepted, RlnScope, SequenceNumber, Subscription, SubscriptionId, WireDecode as _,
    WireEncode as _,
};
use tokio::sync::mpsc;

use crate::Shared;
use crate::credential::{GroupAnon, admit, admit_group, check_signature, start_session};
use crate::dispatch::Reply;
use crate::wire::{decode, encoded, hex_array, query, segment};

/// A live channel's handle in the fan-out registry: where frames for that member go.
pub(crate) type LiveSender = mpsc::Sender<crate::LiveFrame>;

/// The delivery service as the server holds it.
pub(crate) type GroupService = Delivery<SqliteDeliveryStore, LiveSender>;

pub(crate) fn status_of(error: &DeliveryError) -> StatusCode {
    match error {
        DeliveryError::UnknownGroup | DeliveryError::UnknownChannel => StatusCode::NOT_FOUND,
        DeliveryError::GroupExists => StatusCode::CONFLICT,
        DeliveryError::FutureEpoch { .. } | DeliveryError::SubscriptionTooLong => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        DeliveryError::CounterOverflow
        | DeliveryError::TimeOutOfRange
        | DeliveryError::Corrupt(_)
        | DeliveryError::CorruptField { .. }
        | DeliveryError::Poisoned
        | DeliveryError::Sqlite(_)
        | _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn refused(error: &DeliveryError) -> Reply {
    Reply::status(status_of(error))
}

/// The group id in the path (`/v1/groups/{id}/...`).
pub(crate) fn group_id(path: &str) -> Option<GroupId> {
    segment(path, 1)
        .and_then(hex_array::<{ GroupId::LENGTH }>)
        .map(GroupId::from_bytes)
}

fn bad_request() -> Reply {
    Reply::status(StatusCode::BAD_REQUEST)
}

/// The lifetime class named by `?lifetime=`. Absent, it is a Meetup, the shorter-lived and
/// so the more private class: pingers-sync does not send the class yet, and a Party created
/// without it expires after two idle hours rather than lingering for half a year.
fn lifetime(path: &str) -> Option<GroupLifetime> {
    match query(path, "lifetime") {
        None | Some("meetup") => Some(GroupLifetime::Meetup),
        Some("party") => Some(GroupLifetime::Party),
        Some(_) => None,
    }
}

/// The starting epoch named by `?epoch=` (0 when absent).
fn start_epoch(path: &str) -> Option<Epoch> {
    match query(path, "epoch") {
        None => Some(Epoch::new(0)),
        Some(text) => text.parse().ok().map(Epoch::new),
    }
}

/// Whether a group request needs an Anon credential besides its signature.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anon {
    /// A write: create, commit, post, put subscription.
    Required,
    /// A read, a delete or the live channel: the signature alone.
    NotRequired,
}

/// Checks a Grp request to the group in its path (signature first, so a bad signature spends
/// no credential), and returns the group and what the Anon half came to. Only the current
/// epoch's key is accepted.
pub(crate) fn authorise(
    shared: &Shared,
    request: &BinaryRequest,
    body: &[u8],
    anon: Anon,
) -> Result<(GroupId, GroupAnon), Reply> {
    let group = group_id(&request.path).ok_or_else(bad_request)?;
    let auth_key = shared
        .groups
        .auth_key(&group)
        .map_err(|error| refused(&error))?;
    check_signature(shared, request, &[auth_key], body)?;
    let anon = match anon {
        Anon::Required => admit_group(shared, request, group)?,
        Anon::NotRequired => GroupAnon::Session,
    };
    Ok((group, anon))
}

/// Which epoch's key signed a request that may come from a member behind the head.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signer {
    /// The current epoch's key.
    Current,
    /// An earlier epoch's: the member has not applied the later commits yet.
    Behind,
}

/// As [`authorise`] without the Anon half, accepting earlier epochs' keys too: for log reads
/// (a member that missed a commit can read it only with the key it still has) and for
/// commits (one made in an epoch that has moved on is answered with the conflict).
fn authorise_behind(
    shared: &Shared,
    request: &BinaryRequest,
    body: &[u8],
) -> Result<(GroupId, Signer), Reply> {
    let group = group_id(&request.path).ok_or_else(bad_request)?;
    let mut keys = vec![
        shared
            .groups
            .auth_key(&group)
            .map_err(|error| refused(&error))?,
    ];
    keys.extend(
        shared
            .groups
            .past_auth_keys(&group)
            .map_err(|error| refused(&error))?,
    );
    let signer = match check_signature(shared, request, &keys, body)? {
        0 => Signer::Current,
        _ => Signer::Behind,
    };
    Ok((group, signer))
}

/// Adds the new session, if one was started, to `reply`.
fn with_session(reply: Reply, anon: GroupAnon) -> Reply {
    match anon {
        GroupAnon::Session => reply,
        GroupAnon::NewSession((name, value)) => reply.with_header(name, value),
    }
}

/// Sends `frame` to each live channel; a channel whose buffer is full misses it (live frames
/// are superseded by the next, and stored ones are in the log).
fn fan_out(targets: Vec<LiveSender>, frame: &LiveFrame) {
    if targets.is_empty() {
        return;
    }
    let Ok(bytes) = frame.encode() else {
        return;
    };
    for target in targets {
        let _ = target.try_send(crate::LiveFrame::new(bytes.clone()));
    }
}

/// Hands each wake to the push jobs.
fn wake(shared: &Shared, group: GroupId, wakes: &[Wake]) {
    for wake in wakes {
        shared.tasks.wake(group, wake);
    }
}

/// `POST /v1/groups?lifetime=`: Anon (scope Group); starts a session for the new group.
pub(crate) fn create(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let _admission = admit(shared, request, RlnScope::Group)?;
    let lifetime = lifetime(&request.path).ok_or_else(bad_request)?;
    let epoch = start_epoch(&request.path).ok_or_else(bad_request)?;
    let create = decode::<CreateGroup>(&request.body)?;
    let group = create.group_id;
    shared
        .groups
        .create_group(create, GroupStart { lifetime, epoch })
        .map_err(|error| refused(&error))?;
    let (name, value) = start_session(shared, group);
    Ok(Reply::status(StatusCode::CREATED).with_header(name, value))
}

/// `POST /v1/groups/{id}/commit`: 200 with the sequence, or 409 with the head epoch.
pub(crate) fn commit(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, signer) = authorise_behind(shared, request, &request.body)?;
    if signer == Signer::Behind {
        // Signed with an earlier epoch's key, so made in an epoch another commit took:
        // nothing is appended and no credential is spent.
        let head_epoch = shared
            .groups
            .head_epoch(&group)
            .map_err(|error| refused(&error))?;
        return Ok(encoded(
            StatusCode::CONFLICT,
            &pingers_proto::CommitResponse::Conflict { head_epoch },
        ));
    }
    let anon = admit_group(shared, request, group)?;
    let commit = decode::<CommitRequest>(&request.body)?;
    let frame = LiveFrame {
        epoch: commit.epoch,
        body: commit.commit.clone(),
        delivery: Mode::Stored,
    };
    // Through the pusher, which first waits for any push to a removed subscription.
    let outcome = shared
        .tasks
        .pusher()
        .commit(&group, commit, &mut shared.forked_rng())
        .map_err(|error| refused(&error))?;
    let response = outcome.response();
    let status = match outcome {
        CommitOutcome::Accepted { wakes, targets, .. } => {
            wake(shared, group, &wakes);
            fan_out(targets, &frame);
            StatusCode::OK
        }
        CommitOutcome::Conflict { .. } => StatusCode::CONFLICT,
    };
    Ok(with_session(encoded(status, &response), anon))
}

/// The subscription id in the path (`/v1/groups/{id}/subs/{sub_id}`).
fn subscription_id(path: &str) -> Option<SubscriptionId> {
    segment(path, 3)
        .and_then(hex_array::<{ SubscriptionId::LENGTH }>)
        .map(SubscriptionId::from_bytes)
}

/// `PUT /v1/groups/{id}/subs/{sub_id}`: the body's id must be the path's.
pub(crate) fn put_subscription(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &request.body, Anon::Required)?;
    let id = subscription_id(&request.path).ok_or_else(bad_request)?;
    let subscription = decode::<Subscription>(&request.body)?;
    if subscription.id != id {
        return Err(bad_request());
    }
    shared
        .groups
        .put_subscription(&group, subscription)
        .map_err(|error| refused(&error))?;
    Ok(with_session(Reply::status(StatusCode::NO_CONTENT), anon))
}

/// `DELETE /v1/groups/{id}/subs/{sub_id}`.
pub(crate) fn delete_subscription(
    shared: &Shared,
    request: &BinaryRequest,
) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &request.body, Anon::NotRequired)?;
    let id = subscription_id(&request.path).ok_or_else(bad_request)?;
    // Through the pusher, which first waits for a push to this subscription already in
    // flight: once the 204 is sent, the relay hears nothing more for it.
    shared
        .tasks
        .pusher()
        .remove_subscription(&group, &id)
        .map_err(|error| refused(&error))?;
    Ok(with_session(Reply::status(StatusCode::NO_CONTENT), anon))
}

/// `POST /v1/groups/{id}/msgs`: 200 with the sequence for a stored message, 202 for an
/// ephemeral one.
pub(crate) fn post(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &request.body, Anon::Required)?;
    let message = decode::<AppMessage>(&request.body)?;
    let frame = LiveFrame {
        epoch: message.epoch,
        body: message.body.clone(),
        delivery: message.delivery,
    };
    let posted = shared
        .groups
        .post(&group, message, &mut shared.forked_rng())
        .map_err(|error| refused(&error))?;
    wake(shared, group, &posted.wakes);
    fan_out(posted.targets, &frame);
    let reply = match posted.sequence {
        Some(sequence) => encoded(StatusCode::OK, &MessageAccepted { sequence }),
        None => Reply::status(StatusCode::ACCEPTED),
    };
    Ok(with_session(reply, anon))
}

/// `GET /v1/groups/{id}/msgs?after=seq`.
pub(crate) fn read(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, _signer) = authorise_behind(shared, request, &request.body)?;
    let anon = GroupAnon::Session;
    let after = match query(&request.path, "after") {
        None => SequenceNumber::new(0),
        Some(text) => SequenceNumber::new(text.parse().map_err(|_| bad_request())?),
    };
    let messages = shared
        .groups
        .messages(&group, after)
        .map_err(|error| refused(&error))?;
    Ok(with_session(encoded(StatusCode::OK, &messages), anon))
}

/// `GET /v1/groups/{id}/state`: 204 when no snapshot was uploaded.
pub(crate) fn state(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, _signer) = authorise_behind(shared, request, &request.body)?;
    let anon = GroupAnon::Session;
    let reply = match shared
        .groups
        .state(&group)
        .map_err(|error| refused(&error))?
    {
        Some(state) => encoded(StatusCode::OK, &state),
        None => Reply::status(StatusCode::NO_CONTENT),
    };
    Ok(with_session(reply, anon))
}

/// `DELETE /v1/groups/{id}`.
pub(crate) fn delete(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &request.body, Anon::NotRequired)?;
    // Through the pusher, which first waits for every push to the group in flight.
    shared
        .tasks
        .pusher()
        .delete_group(&group)
        .map_err(|error| refused(&error))?;
    Ok(with_session(Reply::status(StatusCode::NO_CONTENT), anon))
}

/// `POST /v1/groups/{id}/live?sub=` as a one-shot request: one uplink frame of a split
/// channel, from the member whose downlink was opened with the same `sub`.
pub(crate) fn uplink(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &request.body, Anon::NotRequired)?;
    let subscription = query(&request.path, "sub")
        .and_then(hex_array::<{ SubscriptionId::LENGTH }>)
        .map(SubscriptionId::from_bytes)
        .ok_or_else(bad_request)?;
    let channel = shared
        .live_channels
        .find(group, subscription)
        .ok_or(Reply::status(StatusCode::NOT_FOUND))?;
    let frame = LiveFrame::decode(&request.body).map_err(|_| bad_request())?;
    forward_frame(shared, FromChannel { group, channel }, frame, &request.body)?;
    Ok(with_session(Reply::status(StatusCode::ACCEPTED), anon))
}

/// Which live channel of which group a frame came from.
#[derive(Clone, Copy)]
pub(crate) struct FromChannel {
    pub(crate) group: GroupId,
    pub(crate) channel: pingers_delivery::ChannelId,
}

/// Records a frame from a channel and forwards its bytes to the group's other channels.
pub(crate) fn forward_frame(
    shared: &Shared,
    from: FromChannel,
    frame: LiveFrame,
    bytes: &[u8],
) -> Result<(), Reply> {
    let forwarded = shared
        .groups
        .live_frame(&from.group, from.channel, frame)
        .map_err(|error| refused(&error))?;
    for target in forwarded.targets {
        let _ = target.try_send(crate::LiveFrame::new(bytes.to_vec()));
    }
    Ok(())
}
