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
    CommitOutcome, Delivery, DeliveryError, GroupLifetime, GroupStart, Reader, SqliteDeliveryStore,
    Wake,
};
use pingers_ohttp::BinaryRequest;
use pingers_proto::{
    AppMessage, CommitRequest, CreateGroup, Delivery as Mode, Epoch, GroupId, LiveFrame,
    MessageAccepted, RlnScope, SequenceNumber, Subscription, SubscriptionId, WireDecode as _,
    WireEncode as _,
};
use tokio::sync::mpsc;

use crate::Shared;
use crate::credential::{
    GroupAnon, admit, admit_group, check_owner_signature, check_signature, start_session,
};
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
        DeliveryError::LaterEpoch
        | DeliveryError::EpochEnded
        | DeliveryError::SubscriptionOwned
        | DeliveryError::PartyGroupKept => StatusCode::FORBIDDEN,
        DeliveryError::FutureEpoch { .. }
        | DeliveryError::SubscriptionTooLong
        | DeliveryError::TooManySubscriptions => StatusCode::UNPROCESSABLE_ENTITY,
        DeliveryError::TooManyChannels | DeliveryError::LiveRateLimited => {
            StatusCode::TOO_MANY_REQUESTS
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
    let (group, _epoch) = authorise_current(shared, request, body)?;
    let anon = match anon {
        Anon::Required => admit_group(shared, request, group)?,
        Anon::NotRequired => GroupAnon::Session,
    };
    Ok((group, anon))
}

/// Checks the signature of a Grp request against the current epoch's key, and returns the
/// group and that epoch (read with the key, so the two belong together): a live channel
/// records the epoch, and lasts until the commit that ends it.
pub(crate) fn authorise_current(
    shared: &Shared,
    request: &BinaryRequest,
    body: &[u8],
) -> Result<(GroupId, Epoch), Reply> {
    let group = group_id(&request.path).ok_or_else(bad_request)?;
    let current = shared
        .groups
        .current_auth(&group)
        .map_err(|error| refused(&error))?;
    check_signature(shared, request, &[current.key], body)?;
    Ok((group, current.epoch))
}

/// As [`authorise`] without the Anon half, accepting earlier epochs' keys too: for log reads
/// (a member that missed a commit can read it only with the key it still has, and reads only
/// up to the commit that ended that key's epoch: pingers-delivery's [`Reader`]) and for
/// commits (one made in an epoch that has moved on is answered with the conflict).
fn authorise_behind(
    shared: &Shared,
    request: &BinaryRequest,
    body: &[u8],
) -> Result<(GroupId, Reader), Reply> {
    let group = group_id(&request.path).ok_or_else(bad_request)?;
    let current = shared
        .groups
        .auth_key(&group)
        .map_err(|error| refused(&error))?;
    let past = shared
        .groups
        .past_auth_keys(&group)
        .map_err(|error| refused(&error))?;
    let keys: Vec<_> = core::iter::once(current)
        .chain(past.iter().map(|past| past.key))
        .collect();
    let position = check_signature(shared, request, &keys, body)?;
    let reader = match position.checked_sub(1) {
        None => Reader::Current,
        Some(index) => past
            .get(index)
            .map(|past| Reader::Behind(past.epoch))
            .ok_or_else(|| Reply::status(StatusCode::INTERNAL_SERVER_ERROR))?,
    };
    Ok((group, reader))
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
    let (group, reader) = authorise_behind(shared, request, &request.body)?;
    if let Reader::Behind(_) = reader {
        // Signed with an earlier epoch's key, so made in an epoch another commit took:
        // nothing is appended and no credential is spent.
        let head_epoch = shared
            .groups
            .head_epoch(&group, reader)
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

/// A request and the body its signatures cover: its own, or for a split channel's uplink the
/// empty body of the live head whose headers it carries.
#[derive(Clone, Copy)]
struct Signed<'request> {
    request: &'request BinaryRequest,
    body: &'request [u8],
}

/// Checks the owner's signature when the group holds subscription `id` (it is bound to the
/// owner key it was first stored with); a subscription the group does not hold needs none.
fn owner_may_act(
    shared: &Shared,
    signed: Signed<'_>,
    group: &GroupId,
    id: &SubscriptionId,
) -> Result<Option<Subscription>, Reply> {
    let Signed { request, body } = signed;
    let held = shared
        .groups
        .subscription(group, id)
        .map_err(|error| refused(&error))?;
    if let Some(held) = &held {
        check_owner_signature(request, &held.owner, body)?;
    }
    Ok(held)
}

/// `PUT /v1/groups/{id}/subs/{sub_id}`: the body's id must be the path's. Replacing a
/// subscription the group holds needs its owner's signature too, checked before a credential
/// is spent.
pub(crate) fn put_subscription(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, _epoch) = authorise_current(shared, request, &request.body)?;
    let id = subscription_id(&request.path).ok_or_else(bad_request)?;
    let subscription = decode::<Subscription>(&request.body)?;
    if subscription.id != id {
        return Err(bad_request());
    }
    owner_may_act(
        shared,
        Signed {
            request,
            body: &request.body,
        },
        &group,
        &id,
    )?;
    let anon = admit_group(shared, request, group)?;
    shared
        .groups
        .put_subscription(&group, subscription)
        .map_err(|error| refused(&error))?;
    Ok(with_session(Reply::status(StatusCode::NO_CONTENT), anon))
}

/// `DELETE /v1/groups/{id}/subs/{sub_id}`: needs the owner's signature when the group holds
/// it (deleting one it does not hold changes nothing).
pub(crate) fn delete_subscription(
    shared: &Shared,
    request: &BinaryRequest,
) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &request.body, Anon::NotRequired)?;
    let id = subscription_id(&request.path).ok_or_else(bad_request)?;
    owner_may_act(
        shared,
        Signed {
            request,
            body: &request.body,
        },
        &group,
        &id,
    )?;
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
    let (group, reader) = authorise_behind(shared, request, &request.body)?;
    let anon = GroupAnon::Session;
    let after = match query(&request.path, "after") {
        None => SequenceNumber::new(0),
        Some(text) => SequenceNumber::new(text.parse().map_err(|_| bad_request())?),
    };
    let messages = shared
        .groups
        .messages(&group, after, reader)
        .map_err(|error| refused(&error))?;
    Ok(with_session(encoded(StatusCode::OK, &messages), anon))
}

/// `GET /v1/groups/{id}/state`: 204 when no snapshot was uploaded.
pub(crate) fn state(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, reader) = authorise_behind(shared, request, &request.body)?;
    let anon = GroupAnon::Session;
    let reply = match shared
        .groups
        .state(&group, reader)
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
/// channel, from the member whose downlink was opened with the same `sub`, signed by that
/// subscription's owner as well (another member knows the id, not the key). An uplink carries
/// the live head's headers (pingers-transport), so both signatures cover the head's empty body,
/// not the frame.
pub(crate) fn uplink(shared: &Shared, request: &BinaryRequest) -> Result<Reply, Reply> {
    let (group, anon) = authorise(shared, request, &[], Anon::NotRequired)?;
    let subscription = query(&request.path, "sub")
        .and_then(hex_array::<{ SubscriptionId::LENGTH }>)
        .map(SubscriptionId::from_bytes)
        .ok_or_else(bad_request)?;
    let signed = Signed { request, body: &[] };
    if owner_may_act(shared, signed, &group, &subscription)?.is_none() {
        return Err(Reply::status(StatusCode::NOT_FOUND));
    }
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
