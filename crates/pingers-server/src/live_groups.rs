// SPDX-License-Identifier: AGPL-3.0-only
//! The live channel on pingers-delivery's fan-out registry (design B.4.9): each chunked
//! exchange to `/v1/groups/{id}/live?sub=` is attached with `open_live` after the Grp and Anon
//! checks, its frames go through `live_frame` to the group's other channels, and it is
//! detached with `close_live` when the member's stream ends. Deleting or expiring the group
//! drops the registry's handle, which ends the exchange; so does the commit that ends the epoch
//! whose key the channel attached with (it is the channel's last frame, and the member attaches
//! again with the next key).
//!
//! Each request chunk carries one pingers-proto `LiveFrame` in its canonical encoding; a chunk
//! that does not decode is dropped. The same bytes are forwarded to the other members.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::http::StatusCode;
use pingers_delivery::ChannelId;
use pingers_ohttp::BinaryRequest;
use pingers_proto::{GroupId, SubscriptionId, WireDecode as _};
use tokio::sync::mpsc;

use crate::credential::check_owner_signature;
use crate::groups::{FromChannel, authorise_current, forward_frame, status_of};
use crate::wire::{hex_array, query};
use crate::{LIVE_BUFFER, LiveFrame, Shared};

/// Which open channel each `(group, sub)` is, so a split channel's one-shot uplink frames
/// (which name their `sub`) are recorded as coming from the member's downlink.
#[derive(Default)]
pub(crate) struct LiveChannels {
    by_subscription: Mutex<HashMap<(GroupId, SubscriptionId), ChannelId>>,
}

impl LiveChannels {
    fn lock(&self) -> MutexGuard<'_, HashMap<(GroupId, SubscriptionId), ChannelId>> {
        // Single inserts and removes: a panic cannot leave the map half-written.
        self.by_subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The channel opened for `subscription` in `group`.
    pub(crate) fn find(&self, group: GroupId, subscription: SubscriptionId) -> Option<ChannelId> {
        self.lock().get(&(group, subscription)).copied()
    }

    fn insert(&self, group: GroupId, subscription: SubscriptionId, channel: ChannelId) {
        self.lock().insert((group, subscription), channel);
    }

    /// Forgets the entry if it is still `channel` (a reconnect may have replaced it).
    fn remove(&self, group: GroupId, subscription: SubscriptionId, channel: ChannelId) {
        let mut map = self.lock();
        if map.get(&(group, subscription)) == Some(&channel) {
            map.remove(&(group, subscription));
        }
    }
}

/// What an attached channel is.
struct Attached {
    group: GroupId,
    subscription: Option<SubscriptionId>,
    channel: ChannelId,
}

/// Checks and attaches a live exchange; blocking (SQLite, proof verification).
fn attach(
    shared: &Shared,
    head: &BinaryRequest,
) -> Result<(Attached, mpsc::Receiver<LiveFrame>), StatusCode> {
    // The head of a live exchange has no body of its own: it signs an empty one. The channel
    // is attached under the epoch whose key signed it, and ends with that epoch.
    let (group, epoch) = authorise_current(shared, head, &[]).map_err(|reply| reply.status)?;
    let named = match query(&head.path, "sub") {
        None => None,
        Some(text) => Some(SubscriptionId::from_bytes(
            hex_array::<{ SubscriptionId::LENGTH }>(text).ok_or(StatusCode::BAD_REQUEST)?,
        )),
    };
    // A channel speaks for a subscription (holds back its pushes, takes its split-channel
    // uplinks) only with that subscription owner's signature. One the group does not hold
    // (expired, or never put) is attached as a channel without one.
    let subscription = match named {
        None => None,
        Some(id) => {
            let held = shared
                .groups
                .subscription(&group, &id)
                .map_err(|error| status_of(&error))?;
            match held {
                Some(held) => {
                    check_owner_signature(head, &held.owner, &[]).map_err(|reply| reply.status)?;
                    Some(id)
                }
                None => None,
            }
        }
    };
    let (sender, outbound) = mpsc::channel(LIVE_BUFFER);
    let channel = shared
        .groups
        .open_live(
            &group,
            pingers_delivery::LiveAttach {
                epoch,
                subscription,
            },
            sender,
        )
        .map_err(|error| status_of(&error))?;
    if let Some(subscription) = subscription {
        shared.live_channels.insert(group, subscription, channel);
    }
    Ok((
        Attached {
            group,
            subscription,
            channel,
        },
        outbound,
    ))
}

/// Opens a live exchange for `head`: the frames the member sends arrive on `inbound`; the
/// returned receiver yields the other members' frames. A refusal says why, for the counters.
pub(crate) async fn open(
    shared: &Arc<Shared>,
    head: BinaryRequest,
    inbound: mpsc::Receiver<LiveFrame>,
) -> Result<mpsc::Receiver<LiveFrame>, StatusCode> {
    let for_attach = Arc::clone(shared);
    let (attached, outbound) = tokio::task::spawn_blocking(move || attach(&for_attach, &head))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)??;
    let for_reader = Arc::downgrade(shared);
    shared.tasks.spawn(async move {
        read_frames(for_reader, attached, inbound).await;
    });
    Ok(outbound)
}

/// Records and forwards each frame until the member's stream ends, then detaches the channel.
async fn read_frames(
    shared: std::sync::Weak<Shared>,
    attached: Attached,
    mut inbound: mpsc::Receiver<LiveFrame>,
) {
    let attached = Arc::new(attached);
    while let Some(frame) = inbound.recv().await {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let attached = Arc::clone(&attached);
        let delivered = tokio::task::spawn_blocking(move || {
            let Ok(decoded) = pingers_proto::LiveFrame::decode(frame.as_bytes()) else {
                // An undecodable frame is dropped; the channel stays open.
                return true;
            };
            let from = FromChannel {
                group: attached.group,
                channel: attached.channel,
            };
            match forward_frame(&shared, from, decoded, frame.as_bytes()) {
                Ok(()) => true,
                // The group or the channel is gone: nothing more can be delivered.
                Err(reply) => reply.status != StatusCode::NOT_FOUND,
            }
        })
        .await
        .unwrap_or(false);
        if !delivered {
            break;
        }
    }
    let Some(shared) = shared.upgrade() else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(subscription) = attached.subscription {
            shared
                .live_channels
                .remove(attached.group, subscription, attached.channel);
        }
        // Already detached if the group was deleted or expired.
        let _ = shared.groups.close_live(&attached.group, attached.channel);
    })
    .await;
}
