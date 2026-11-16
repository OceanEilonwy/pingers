// SPDX-License-Identifier: AGPL-3.0-only
//! Joining a Party from an invite link (design D invite flow, join flow A), both halves:
//!
//! - Joiner. `open_invite` ("Ask to join"): a pooled reply mailbox (registered now if the pool
//!   is empty) and, with wake-up pushes on, a Party subscription sealed to the push relay's
//!   pinned key go into pingers-parties' join request, which is posted to the invite mailbox.
//!   The join is recorded, so it survives a restart, and its code shows at once. `withdraw`
//!   ("Withdraw request") posts the withdrawal and forgets the join. `poll` finds the Welcome
//!   in the reply mailbox once the inviter lets this device in: pingers-parties says which
//!   group and log-auth key it gives, pingers-sync fetches the group's state snapshot with
//!   them, and pingers-parties joins ("You're in").
//! - Inviter. `poll` reads the mailbox of every open invite after its read position and hands
//!   each post to pingers-parties ("Ben asked to join", or a withdrawal), and keeps one invite
//!   mailbox pooled for the next link. `decide` lets a waiting joiner in (the commit with its
//!   Welcome, and the invite mailbox's deletion) or turns them away (nothing is sent).
//!
//! Every domain decision is pingers-parties'; this module only moves values between it and
//! pingers-sync.

use core::time::Duration;

use pingers_parties::{
    ArrivedWelcome, InviteId, InviteLink, InviteStatus, JoinDetails, JoinId, JoinRequestOutcome,
    PartiesError, PartyLifecycle,
};
use pingers_proto::{
    DisplayName, MailboxId, MailboxMessage, PartyId, SequenceNumber, Subscription, TimeToLive,
    WireDecode as _, WireEncode,
};
use pingers_store::MailboxPurpose;
use pingers_sync::{GroupCall, PoolTarget, SyncError, WakeBudget};

use super::records::{self, Key, corrupt};
use super::sync::Net;
use crate::ports::{ClientEvent, JoinDecision};
use crate::snapshot::{InviteUrl, Name};
use crate::{ClientError, Unavailable};

/// A Party subscription lives as long as a sealed token: 30 days, renewed by the member
/// (design B.4.9).
const PARTY_SUBSCRIPTION: Duration = super::push::TOKEN_LIFETIME;

/// A reply mailbox lives a day: as long as the invite it answers.
const REPLY_LIFETIME: Duration = Duration::from_hours(24);

/// An invite mailbox lives as long as the invite it serves.
const INVITE_MAILBOX_LIFETIME: Duration = pingers_parties::INVITE_LIFETIME;

/// Posts a join request for `link` under `name` ("Ask to join"). With wake-up pushes off the
/// request carries no subscription: the join still works, without wake-ups.
///
/// # Errors
///
/// [`Unavailable::NoPushToken`] with pushes on but no token yet; [`ClientError::Parties`] for
/// a bad or expired link; [`ClientError::Sync`].
pub(crate) async fn open_invite(net: &Net, link: InviteUrl, name: Name) -> Result<(), ClientError> {
    let engine = net.engine()?;
    // A registration code in the link is for the first registration on the link's own server
    // (design B.1): offered before the reply mailbox needs a credential.
    if let Ok(payload) = InviteLink::from_text(link.as_str().to_owned()).payload()
        && let Some(code) = payload.registration_code
        && payload.server == engine.server()
    {
        net.offer_invite_code(code).await?;
    } else {
        net.ready().await?;
    }
    let subscription = match net.ensure_wake_push().await {
        Ok(()) => engine
            .party_subscription(TimeToLive::new(PARTY_SUBSCRIPTION).map_err(|_never| corrupt())?)?,
        Err(ClientError::Unavailable(Unavailable::WakePushesOff)) => None,
        Err(error) => return Err(error),
    };
    let link = InviteLink::from_text(link.as_str().to_owned());
    let display_name = DisplayName::new(name.as_str().to_owned()).map_err(PartiesError::Proto)?;
    let prepared = match prepare(net, &link, &display_name, subscription.as_ref()).await {
        Err(ClientError::Parties(PartiesError::NoPooledMailbox)) => {
            refill(net, MailboxPurpose::Reply, REPLY_LIFETIME).await?;
            prepare(net, &link, &display_name, subscription.as_ref()).await?
        }
        other => other?,
    };
    net.push([prepared]);
    net.flush().await
}

/// Registers an invite mailbox if none is pooled, for a new invite link.
///
/// # Errors
///
/// [`ClientError::Sync`].
pub(crate) async fn refill_invite_pool(net: &Net) -> Result<(), ClientError> {
    refill(net, MailboxPurpose::Invite, INVITE_MAILBOX_LIFETIME).await
}

/// Registers one mailbox for `purpose`'s pool if it is empty.
async fn refill(net: &Net, purpose: MailboxPurpose, lifetime: Duration) -> Result<(), ClientError> {
    let target = PoolTarget {
        purpose,
        size: 1,
        lifetime: TimeToLive::new(lifetime).map_err(|_never| corrupt())?,
    };
    let refilled = net
        .engine()?
        .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
        .await;
    net.observe(&refilled);
    let _registered: pingers_sync::PoolRefill = refilled?;
    Ok(())
}

/// pingers-parties' join request, and the join recorded with its subscription.
async fn prepare(
    net: &Net,
    link: &InviteLink,
    name: &DisplayName,
    subscription: Option<&Subscription>,
) -> Result<pingers_parties::Outbound, ClientError> {
    let link = InviteLink::from_text(link.as_str().to_owned());
    let name = name.clone();
    let subscription = subscription.cloned();
    net.backend
        .store(move |session, clock, rng| {
            session.transaction(|session| {
                let encoded = subscription
                    .as_ref()
                    .map(WireEncode::encode)
                    .transpose()
                    .map_err(PartiesError::Proto)?;
                let prepared = PartyLifecycle::new(session, clock).prepare_join(
                    &link,
                    JoinDetails {
                        display_name: name,
                        subscription,
                    },
                    rng,
                )?;
                records::write(session, &Key::PendingJoin, |writer| {
                    writer.fixed(prepared.join.reply_mailbox().as_bytes());
                    writer.option(encoded.as_deref(), super::records::Writer::fixed);
                })?;
                Ok(prepared.outbound)
            })
        })
        .await
}

/// The join this device started, and its subscription.
struct Pending {
    join: JoinId,
    subscription: Option<Subscription>,
}

async fn pending(net: &Net) -> Result<Option<Pending>, ClientError> {
    net.backend
        .store(|session, _clock, _rng| {
            records::read(session, &Key::PendingJoin, |reader| {
                let reply = MailboxId::from_bytes(reader.fixed()?);
                let subscription = reader.option(|reader| {
                    Subscription::decode(&reader.rest()).map_err(|_bad| corrupt())
                })?;
                Ok(Pending {
                    join: JoinId::for_reply_mailbox(reply),
                    subscription,
                })
            })
        })
        .await
}

/// Withdraws the join this device asked for ("Withdraw request").
///
/// # Errors
///
/// [`ClientError::Parties`] (`UnknownJoin`) with no join under way; a storage failure.
pub(crate) async fn withdraw(net: &Net) -> Result<(), ClientError> {
    let join = pending(net).await?.ok_or(PartiesError::UnknownJoin)?.join;
    let outbound = net
        .backend
        .store(move |session, clock, rng| {
            session.transaction(|session| {
                let outbound = PartyLifecycle::new(session, clock).withdraw_join(join, rng)?;
                records::delete(session, &Key::PendingJoin)?;
                Ok(outbound)
            })
        })
        .await?;
    net.push(outbound);
    net.flush().await
}

/// Forgets the "You're in" state once the user opened the Party.
///
/// # Errors
///
/// A storage failure.
pub(crate) async fn dismiss(net: &Net) -> Result<(), ClientError> {
    net.backend
        .store(|session, _clock, _rng| records::delete(session, &Key::LetIn))
        .await
}

/// Lets a waiting joiner in, or turns them away.
///
/// # Errors
///
/// [`ClientError::Parties`] (`UnknownJoin`, `InviteExpired`, `CommitInFlight`); a storage
/// failure.
pub(crate) async fn decide(
    net: &Net,
    party: PartyId,
    request: MailboxId,
    decision: JoinDecision,
) -> Result<(), ClientError> {
    let join = JoinId::for_reply_mailbox(request);
    match decision {
        JoinDecision::Admit => {
            let admission = net
                .backend
                .store(move |session, clock, rng| {
                    Ok(PartyLifecycle::new(session, clock).approve_join(&party, join, rng)?)
                })
                .await?;
            net.push([
                pingers_parties::Outbound::Commit(admission.commit),
                admission.close_invite,
            ]);
            net.flush().await
        }
        JoinDecision::Refuse => {
            net.backend
                .store(move |session, clock, _rng| {
                    Ok(PartyLifecycle::new(session, clock).decline_join(&party, join)?)
                })
                .await
        }
    }
}

/// An open invite and where to resume reading its mailbox.
struct OpenInvite {
    party: PartyId,
    invite: InviteId,
    after: Option<SequenceNumber>,
}

/// Reads what joins wait for: posts to this device's open invites, and the Welcome for the
/// join it asked for. Keeps an invite mailbox pooled if this device is in a Party. Returns
/// the events to raise.
///
/// # Errors
///
/// A storage failure; network failures leave the reads for the next wake.
pub(crate) async fn poll(net: &Net) -> Result<Vec<ClientEvent>, ClientError> {
    let mut events = Vec::new();
    let (invites, in_a_party) = net
        .backend
        .store(|session, clock, _rng| {
            let lifecycle = PartyLifecycle::new(session, clock);
            let parties = session.parties().list()?;
            let mut open = Vec::new();
            for party in &parties {
                for summary in lifecycle.invites(&party.id)? {
                    if summary.status == InviteStatus::Open {
                        open.push(OpenInvite {
                            party: party.id,
                            invite: summary.invite,
                            after: lifecycle.invite_read_position(summary.invite)?,
                        });
                    }
                }
            }
            Ok((open, !parties.is_empty()))
        })
        .await?;
    for open in invites {
        let read = transient(read(net, &open.invite.mailbox(), open.after).await)?;
        for message in read.unwrap_or_default() {
            events.extend(receive(net, &open, message).await?);
        }
    }
    if let Some(event) = transient(complete(net).await)?.flatten() {
        events.push(ClientEvent::PartiesChanged);
        events.push(event);
    }
    if in_a_party {
        transient(refill_invite_pool(net).await)?;
    }
    Ok(events)
}

/// A network failure is `None` (try on the next wake); a storage failure stays an error.
fn transient<T>(result: Result<T, ClientError>) -> Result<Option<T>, ClientError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error @ (ClientError::Store(_) | ClientError::Records(_))) => Err(error),
        Err(_) => Ok(None),
    }
}

async fn read(
    net: &Net,
    mailbox: &MailboxId,
    after: Option<SequenceNumber>,
) -> Result<Vec<MailboxMessage>, ClientError> {
    let read = net
        .engine()?
        .read_mailbox(mailbox, after, WakeBudget::UNLIMITED)
        .await;
    net.observe(&read);
    Ok(read?.messages)
}

/// Hands one invite post to pingers-parties.
async fn receive(
    net: &Net,
    open: &OpenInvite,
    message: MailboxMessage,
) -> Result<Option<ClientEvent>, ClientError> {
    let invite = open.invite;
    let outcome = net
        .backend
        .store(move |session, clock, _rng| {
            Ok(PartyLifecycle::new(session, clock).receive_join_request(
                invite,
                message.sequence,
                &message.body,
            )?)
        })
        .await?;
    Ok(match outcome {
        JoinRequestOutcome::AwaitingApproval(_) => Some(ClientEvent::JoinRequested(open.party)),
        JoinRequestOutcome::Withdrawn(_) => Some(ClientEvent::PartiesChanged),
        JoinRequestOutcome::Refused(_) | JoinRequestOutcome::Ignored(_) => None,
    })
}

/// Completes the recorded join if its Welcome has arrived; `None` while the inviter has not
/// let this device in.
async fn complete(net: &Net) -> Result<Option<ClientEvent>, ClientError> {
    let Some(Pending { join, subscription }) = pending(net).await? else {
        return Ok(None);
    };
    let engine = net.engine()?;
    let Some(welcome) = read(net, &join.reply_mailbox(), None)
        .await?
        .into_iter()
        .next()
        .map(|message| message.body)
    else {
        return Ok(None);
    };
    let looked = welcome.clone();
    let access = net
        .backend
        .store(move |session, clock, _rng| {
            Ok(PartyLifecycle::new(session, clock).welcome_access(join, &looked)?)
        })
        .await?;
    let call = GroupCall {
        group: &access.group,
        key: &access.auth,
    };
    let state = engine.group_state(call, WakeBudget::UNLIMITED).await;
    net.observe(&state);
    let arrived = ArrivedWelcome {
        join,
        welcome,
        snapshot: state?.state,
    };
    let joined = net
        .backend
        .store(move |session, clock, rng| {
            session.transaction(|session| {
                let joined = PartyLifecycle::new(session, clock).accept_welcome(&arrived, rng)?;
                records::delete(session, &Key::PendingJoin)?;
                records::write(session, &Key::LetIn, |writer| {
                    writer.fixed(joined.party.as_bytes());
                })?;
                Ok(joined)
            })
        })
        .await?;
    if let Some(subscription) = &subscription {
        engine
            .remember_subscription(&access.group, subscription)
            .await
            .map_err(|error: SyncError| ClientError::Sync(error))?;
    }
    net.push(joined.outbound);
    net.flush().await?;
    Ok(Some(ClientEvent::LetIn(joined.party)))
}

/// The Party this device was just let into, until the user opens it.
///
/// # Errors
///
/// A storage failure.
pub(crate) fn let_in(session: &pingers_store::Session) -> Result<Option<PartyId>, ClientError> {
    records::read(session, &Key::LetIn, |reader| {
        Ok(PartyId::from_bytes(reader.fixed()?))
    })
}

/// The join this device asked for, if any.
///
/// # Errors
///
/// A storage failure.
pub(crate) fn asked(session: &pingers_store::Session) -> Result<Option<JoinId>, ClientError> {
    records::read(session, &Key::PendingJoin, |reader| {
        let reply = MailboxId::from_bytes(reader.fixed()?);
        // The subscription is not needed here.
        drop(reader.rest());
        Ok(JoinId::for_reply_mailbox(reply))
    })
}
