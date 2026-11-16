// SPDX-License-Identifier: AGPL-3.0-only
//! What the phone shows outside the app after a wake (design B.7): pushes arrive empty, the
//! core reads the mailbox or group log they name, and the difference between the snapshot
//! before the wake and the one after says what happened. A knock that was declined, a quiet
//! Party commit or a roster change shows nothing.
//!
//! Each notification has a [`NotificationKey`], so a later one about the same thing (the next
//! chat message of a Meetup) replaces the earlier one instead of piling up.

use std::collections::HashSet;

use pingers_proto::{
    ChatBody, ChatMessageId, GroupId, LeafIndex, LocationRequestId, MailboxId, PartyId,
};

use crate::common::Name;
use crate::home::{CoPresenceAlert, meetup_names};
use crate::snapshot::{ChatEntry, ClientSnapshot, Meetup};

/// Something that happened in a wake, for a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Notification {
    /// A Meetup started: "Ben is at The Anchor too."
    MeetupStarted(CoPresenceAlert),
    /// Someone opened this device's invite and waits to be let in.
    JoinRequest {
        /// The Party.
        party: PartyId,
        /// The invite.
        invite: MailboxId,
        /// The Party.
        party_name: Name,
        /// The name they gave.
        name: Name,
    },
    /// A chat message (or reply) from another member.
    ChatMessage {
        /// The Meetup.
        meetup: GroupId,
        /// The message.
        message: ChatMessageId,
        /// Its Spot.
        spot_name: Name,
        /// Who wrote it.
        author: Name,
        /// What it says.
        body: ChatBody,
    },
    /// A member asks to see this device's exact location.
    LocationRequest {
        /// The Meetup.
        meetup: GroupId,
        /// The request.
        request: LocationRequestId,
        /// Its Spot.
        spot_name: Name,
        /// Who asks.
        from: Name,
        /// Their leaf in the Meetup.
        asker: LeafIndex,
    },
}

/// What a notification is about: a later notification with the same key replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotificationKey {
    /// The Meetup's start.
    Meetup(GroupId),
    /// The Meetup's chat: one notification per Meetup, showing the latest message.
    Chat(GroupId),
    /// An invite's join request.
    Invite(MailboxId),
    /// A location request.
    LocationRequest(LocationRequestId),
}

impl NotificationKey {
    /// A number for the platform's notification id, the same for the same key: a tag for
    /// the kind in the top byte over the first three bytes of the id. Two keys of one kind
    /// collide only if their random ids share three bytes.
    #[must_use]
    pub fn stable_id(self) -> u32 {
        let (tag, bytes): (u8, &[u8]) = match &self {
            Self::Meetup(group) => (1, group.as_bytes()),
            Self::Chat(group) => (2, group.as_bytes()),
            Self::Invite(invite) => (3, invite.as_bytes()),
            Self::LocationRequest(request) => (4, request.as_bytes()),
        };
        let mut id = [tag, 0, 0, 0];
        for (target, source) in id.iter_mut().skip(1).zip(bytes) {
            *target = *source;
        }
        u32::from_be_bytes(id)
    }
}

impl Notification {
    /// What it is about.
    #[must_use]
    pub const fn key(&self) -> NotificationKey {
        match self {
            Self::MeetupStarted(alert) => NotificationKey::Meetup(alert.meetup),
            Self::JoinRequest { invite, .. } => NotificationKey::Invite(*invite),
            Self::ChatMessage { meetup, .. } => NotificationKey::Chat(*meetup),
            Self::LocationRequest { request, .. } => NotificationKey::LocationRequest(*request),
        }
    }
}

/// What the user should hear about: what `after` holds that `before` did not.
#[must_use]
pub fn notifications(before: &ClientSnapshot, after: &ClientSnapshot) -> Vec<Notification> {
    let mut found = Vec::new();
    let known: HashSet<GroupId> = before.meetups.iter().map(|meetup| meetup.id).collect();
    for meetup in &after.meetups {
        let Some(names) = meetup_names(after, meetup) else {
            continue;
        };
        if !known.contains(&meetup.id) {
            found.push(Notification::MeetupStarted(CoPresenceAlert {
                meetup: meetup.id,
                party_name: names.party.clone(),
                others: meetup.others.iter().map(|peer| peer.name.clone()).collect(),
                spot_name: names.spot.clone(),
            }));
        }
        let earlier = before.meetups.iter().find(|old| old.id == meetup.id);
        let seen = earlier.map(message_ids).unwrap_or_default();
        for (id, author, body) in messages(meetup) {
            if seen.contains(&id) || author == meetup.me.leaf {
                continue;
            }
            found.push(Notification::ChatMessage {
                meetup: meetup.id,
                message: id,
                spot_name: names.spot.clone(),
                author: member_name(meetup, author),
                body: body.clone(),
            });
        }
    }
    let asked: HashSet<LocationRequestId> = before
        .location_requests
        .iter()
        .map(|request| request.id)
        .collect();
    for request in &after.location_requests {
        if asked.contains(&request.id) {
            continue;
        }
        let Some(meetup) = after.meetup(request.meetup).ok() else {
            continue;
        };
        let Some(names) = meetup_names(after, meetup) else {
            continue;
        };
        found.push(Notification::LocationRequest {
            meetup: meetup.id,
            request: request.id,
            spot_name: names.spot,
            from: member_name(meetup, request.from),
            asker: request.from,
        });
    }
    let asked: HashSet<MailboxId> = before
        .parties
        .iter()
        .flat_map(|party| party.join_requests.iter())
        .map(|request| request.id)
        .collect();
    for party in &after.parties {
        for request in &party.join_requests {
            if !asked.contains(&request.id) {
                found.push(Notification::JoinRequest {
                    party: party.id,
                    invite: request.id,
                    party_name: party.name.clone(),
                    name: request.name.clone(),
                });
            }
        }
    }
    found
}

/// Everything a wake changed that the app is told about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changes {
    /// What to show outside the app.
    pub notifications: Vec<Notification>,
    /// Meetups that ended (their content is gone).
    pub meetups_ended: Vec<GroupId>,
    /// Whether Parties, rosters, Spots or invites changed.
    pub parties_changed: bool,
}

/// What changed from `before` to `after`.
#[must_use]
pub fn changes(before: &ClientSnapshot, after: &ClientSnapshot) -> Changes {
    let live: HashSet<GroupId> = after.meetups.iter().map(|meetup| meetup.id).collect();
    Changes {
        notifications: notifications(before, after),
        meetups_ended: before
            .meetups
            .iter()
            .map(|meetup| meetup.id)
            .filter(|meetup| !live.contains(meetup))
            .collect(),
        parties_changed: before.parties != after.parties,
    }
}

/// Every message and reply in a Meetup's chat, in order.
fn messages(meetup: &Meetup) -> Vec<(ChatMessageId, LeafIndex, &ChatBody)> {
    let mut all = Vec::new();
    for entry in &meetup.chat {
        if let ChatEntry::Message(message) = entry {
            all.push((message.id, message.author, &message.body));
            for reply in &message.replies {
                all.push((reply.id, reply.author, &reply.body));
            }
        }
    }
    all
}

fn message_ids(meetup: &Meetup) -> HashSet<ChatMessageId> {
    messages(meetup).into_iter().map(|(id, _, _)| id).collect()
}

/// A member's name, or this device's own for its own leaf.
fn member_name(meetup: &Meetup, leaf: LeafIndex) -> Name {
    meetup
        .others
        .iter()
        .find(|peer| peer.leaf == leaf)
        .map_or_else(|| meetup.me.name.clone(), |peer| peer.name.clone())
}

#[cfg(test)]
mod tests;
