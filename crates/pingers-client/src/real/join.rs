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
    DisplayName, MailboxId, MailboxMessage, PartyId, SequenceNumber, ServerId, Subscription,
    TimeToLive, WireDecode as _, WireEncode,
};
use pingers_store::MailboxPurpose;
use pingers_sync::{CodeSource, GroupCall, PoolTarget, SyncError, WakeBudget};

use super::records::{self, Key, corrupt};
use super::servers::Link;
use super::sync::Net;
use crate::ports::{ClientEvent, JoinDecision};
use crate::snapshot::{InviteUrl, Name};
use crate::{ClientError, Unavailable};

/// A Party subscription lives as long as a sealed token: 30 days, renewed by the member
/// (design B.4.9).
const PARTY_SUBSCRIPTION: Duration = super::push::TOKEN_LIFETIME;

/// A reply mailbox lives a day: as long as the invite it answers.
const REPLY_LIFETIME: Duration = Duration::from_hours(24);

/// Posts a join request for `link` under `name` ("Ask to join"), on the server the link names.
/// With wake-up pushes off, or on but without the platform's push token yet, the request carries
/// no subscription: the join still works, and the device reads its reply mailbox itself
/// ([`poll`]).
///
/// # Errors
///
/// [`ClientError::Parties`] for a bad or expired link; [`ClientError::Server`] (`KeyMismatch`)
/// if the server's keys are not the ones the link pins; [`ClientError::Sync`].
pub(crate) async fn open_invite(net: &Net, link: InviteUrl, name: Name) -> Result<(), ClientError> {
    let link = InviteLink::from_text(link.as_str().to_owned());
    let payload = link.payload()?;
    // The link names the Party's server and the keys the inviter trusts for it: a server this
    // device has not used is recorded with those pins, and refused if its keys differ.
    let server = net.invite_link(&payload).await?;
    // A registration code in the link is for the first registration on the link's own server
    // (design B.1): offered before the reply mailbox needs a credential.
    match payload.registration_code {
        Some(code) => server.offer_code(code, CodeSource::Invite).await?,
        None => server.account().await?,
    }
    let subscription = match net.ensure_wake_push(&server.server).await {
        Ok(()) => server
            .engine
            .party_subscription(TimeToLive::new(PARTY_SUBSCRIPTION).map_err(|_never| corrupt())?)?,
        // No wake-ups (pushes off, or no token from the platform yet): the join still works;
        // the device then reads its reply mailbox on its own wakes and while it waits on screen.
        Err(ClientError::Unavailable(Unavailable::WakePushesOff | Unavailable::NoPushToken)) => {
            None
        }
        Err(error) => return Err(error),
    };
    let display_name = DisplayName::new(name.as_str().to_owned()).map_err(PartiesError::Proto)?;
    let asking = Asking {
        server: &server.server,
        link: &link,
        name: &display_name,
        subscription: subscription.as_ref(),
    };
    let prepared = match prepare(net, &asking).await {
        Err(ClientError::Parties(PartiesError::NoPooledMailbox)) => {
            refill_reply_pool(&server).await?;
            prepare(net, &asking).await?
        }
        other => other?,
    };
    net.push(&server.server, [prepared]);
    net.flush().await
}

/// Registers one reply mailbox on `link`'s server if its pool is empty.
async fn refill_reply_pool(link: &Link) -> Result<(), ClientError> {
    let target = PoolTarget {
        purpose: MailboxPurpose::Reply,
        size: 1,
        lifetime: TimeToLive::new(REPLY_LIFETIME).map_err(|_never| corrupt())?,
    };
    let refilled = link
        .engine
        .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
        .await;
    link.observe(&refilled);
    let _registered: pingers_sync::PoolRefill = refilled?;
    Ok(())
}

/// What a join request is made from.
struct Asking<'values> {
    server: &'values ServerId,
    link: &'values InviteLink,
    name: &'values DisplayName,
    subscription: Option<&'values Subscription>,
}

/// pingers-parties' join request, and the join recorded with its server and subscription.
async fn prepare(net: &Net, asking: &Asking<'_>) -> Result<pingers_parties::Outbound, ClientError> {
    let link = InviteLink::from_text(asking.link.as_str().to_owned());
    let name = asking.name.clone();
    let subscription = asking.subscription.cloned();
    let server = asking.server.clone();
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
                    writer.text(server.as_str());
                    writer.option(encoded.as_deref(), super::records::Writer::fixed);
                })?;
                Ok(prepared.outbound)
            })
        })
        .await
}

/// The join this device started, the server it asked on, and its subscription.
struct Pending {
    join: JoinId,
    server: ServerId,
    subscription: Option<Subscription>,
}

async fn pending(net: &Net) -> Result<Option<Pending>, ClientError> {
    net.backend
        .store(|session, _clock, _rng| {
            records::read(session, &Key::PendingJoin, |reader| {
                let reply = MailboxId::from_bytes(reader.fixed()?);
                let server = ServerId::new(reader.text()?).map_err(|_bad| corrupt())?;
                let subscription = reader.option(|reader| {
                    Subscription::decode(&reader.rest()).map_err(|_bad| corrupt())
                })?;
                Ok(Pending {
                    join: JoinId::for_reply_mailbox(reply),
                    server,
                    subscription,
                })
            })
        })
        .await
}

/// Whether this device waits on a join no push will announce (asked without a subscription):
/// then every wake reads its reply mailbox.
///
/// # Errors
///
/// A storage failure.
pub(crate) async fn waits_unannounced(net: &Net) -> Result<bool, ClientError> {
    Ok(pending(net)
        .await?
        .is_some_and(|pending| pending.subscription.is_none()))
}

/// Withdraws the join this device asked for ("Withdraw request").
///
/// # Errors
///
/// [`ClientError::Parties`] (`UnknownJoin`) with no join under way; a storage failure.
pub(crate) async fn withdraw(net: &Net) -> Result<(), ClientError> {
    let asked = pending(net).await?.ok_or(PartiesError::UnknownJoin)?;
    let join = asked.join;
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
    net.push(&asked.server, outbound);
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
            let server = net.party_server(party).await?;
            let admission = net
                .backend
                .store(move |session, clock, rng| {
                    Ok(PartyLifecycle::new(session, clock).approve_join(&party, join, rng)?)
                })
                .await?;
            net.push(
                &server,
                [
                    pingers_parties::Outbound::Commit(admission.commit),
                    admission.close_invite,
                ],
            );
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
    server: ServerId,
    invite: InviteId,
    after: Option<SequenceNumber>,
}

/// Reads what joins wait for: posts to this device's open invites, and the Welcome for the
/// join it asked for. Keeps an invite mailbox pooled on each server this device has a Party
/// on. Returns the events to raise.
///
/// # Errors
///
/// A storage failure; network failures leave the reads for the next wake.
pub(crate) async fn poll(net: &Net) -> Result<Vec<ClientEvent>, ClientError> {
    let mut events = Vec::new();
    let (invites, servers) = net
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
                            server: party.server.clone(),
                            invite: summary.invite,
                            after: lifecycle.invite_read_position(summary.invite)?,
                        });
                    }
                }
            }
            let mut servers: Vec<ServerId> = Vec::new();
            for party in parties {
                if !servers.contains(&party.server) {
                    servers.push(party.server);
                }
            }
            Ok((open, servers))
        })
        .await?;
    for open in invites {
        let read = transient(read(net, &open.server, &open.invite.mailbox(), open.after).await)?;
        for message in read.unwrap_or_default() {
            events.extend(receive(net, &open, message).await?);
        }
    }
    if let Some(event) = transient(complete(net).await)?.flatten() {
        events.push(ClientEvent::PartiesChanged);
        events.push(event);
    }
    // One invite mailbox pooled on each server with a Party, for the next link there.
    for server in &servers {
        transient(net.refill_invite_pool(server).await)?;
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
    server: &ServerId,
    mailbox: &MailboxId,
    after: Option<SequenceNumber>,
) -> Result<Vec<MailboxMessage>, ClientError> {
    let link = net.link(server).await?;
    let read = link
        .engine
        .read_mailbox(mailbox, after, WakeBudget::UNLIMITED)
        .await;
    link.observe(&read);
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
    let Some(Pending {
        join,
        server,
        subscription,
    }) = pending(net).await?
    else {
        return Ok(None);
    };
    let link = net.link(&server).await?;
    let engine = &link.engine;
    let Some(welcome) = read(net, &server, &join.reply_mailbox(), None)
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
    link.observe(&state);
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
    net.push(&server, joined.outbound);
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
