// SPDX-License-Identifier: AGPL-3.0-only
//! Joining a Party from an invite link (design D invite flow), the joiner's half:
//!
//! 1. `open_invite`: a pooled reply mailbox (registered now if the pool is empty) and a Party
//!    subscription sealed to the push relay's pinned key go into pingers-parties' join request,
//!    which is posted to the invite mailbox. The join is recorded, so it survives a restart.
//! 2. `accept_invite`: once the inviter has admitted this device, the Welcome is in the reply
//!    mailbox. pingers-parties says which group and log-auth key it gives, pingers-sync fetches
//!    the group's state snapshot with them, and pingers-parties joins.
//!
//! The join request must carry a sealed subscription, so joining needs wake-up pushes on.

use core::time::Duration;

use pingers_parties::{
    ArrivedWelcome, InviteLink, JoinDetails, JoinId, PartiesError, PartyLifecycle,
};
use pingers_proto::{
    DisplayName, MailboxId, PartyId, Subscription, TimeToLive, WireDecode as _, WireEncode as _,
};
use pingers_store::MailboxPurpose;
use pingers_sync::{CodeSource, GroupCall, PoolTarget, WakeBudget};

use super::records::{self, Key, corrupt};
use super::sync::Net;
use crate::snapshot::{InviteUrl, Name};
use crate::{ClientError, Unavailable};

/// A Party subscription lives as long as a sealed token: 30 days, renewed by the member
/// (design B.4.9).
const PARTY_SUBSCRIPTION: Duration = super::push::TOKEN_LIFETIME;

/// A reply mailbox lives a day: long enough for the inviter to see the request.
const REPLY_LIFETIME: Duration = Duration::from_hours(24);

/// Posts a join request for `link` under `name`.
///
/// # Errors
///
/// [`Unavailable::WakePushesOff`] or [`Unavailable::NoPushToken`] if no subscription can be
/// sealed; [`ClientError::Parties`] for a bad or expired link; [`ClientError::Sync`].
pub(crate) async fn open_invite(net: &Net, link: InviteUrl, name: Name) -> Result<(), ClientError> {
    let payload = InviteLink::from_text(link.as_str().to_owned()).payload()?;
    let server = net.invite_link(&payload).await?;
    let engine = &server.engine;
    // A registration code in the link is for the first registration on the link's own server
    // (design B.1): offered before the reply mailbox needs a credential.
    match payload.registration_code {
        Some(code) => server.offer_code(code, CodeSource::Invite).await?,
        None => server.account().await?,
    }
    net.ensure_wake_push(&server.server).await?;
    let subscription = engine
        .party_subscription(TimeToLive::new(PARTY_SUBSCRIPTION).map_err(|_never| corrupt())?)?
        .ok_or(Unavailable::WakePushesOff)?;
    let link = InviteLink::from_text(link.as_str().to_owned());
    let display_name = DisplayName::new(name.as_str().to_owned()).map_err(PartiesError::Proto)?;
    let inputs = JoinInputs {
        server: server.server.clone(),
        link: link.as_str().to_owned(),
        name: display_name,
        subscription,
    };
    let prepared = match prepare(net, inputs.clone()).await {
        Err(ClientError::Parties(PartiesError::NoPooledMailbox)) => {
            let target = PoolTarget {
                purpose: MailboxPurpose::Reply,
                size: 1,
                lifetime: TimeToLive::new(REPLY_LIFETIME).map_err(|_never| corrupt())?,
            };
            let refilled = engine
                .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
                .await;
            server.observe(&refilled);
            let _registered: pingers_sync::PoolRefill = refilled?;
            prepare(net, inputs).await?
        }
        other => other?,
    };
    net.push(&server.server, [prepared]);
    net.flush().await
}

/// pingers-parties' join request, and the join recorded with its subscription.
/// What a join request is made from.
#[derive(Clone)]
struct JoinInputs {
    server: pingers_proto::ServerId,
    link: String,
    name: DisplayName,
    subscription: Subscription,
}

async fn prepare(net: &Net, inputs: JoinInputs) -> Result<pingers_parties::Outbound, ClientError> {
    let JoinInputs {
        server,
        link,
        name,
        subscription,
    } = inputs;
    let link = InviteLink::from_text(link);
    net.backend
        .store(move |session, clock, rng| {
            session.transaction(|session| {
                let encoded = subscription.encode().map_err(PartiesError::Proto)?;
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
                    writer.fixed(&encoded);
                })?;
                Ok(prepared.outbound)
            })
        })
        .await
}

/// Completes the recorded join once its Welcome has arrived.
///
/// # Errors
///
/// [`ClientError::Parties`] (`UnknownJoin`) with no join under way;
/// [`Unavailable::AwaitingWelcome`] while the inviter has not admitted this device;
/// [`ClientError::Sync`]; [`ClientError::Parties`].
pub(crate) async fn accept_invite(net: &Net) -> Result<PartyId, ClientError> {
    let (join, subscription, server) = net
        .backend
        .store(|session, _clock, _rng| {
            records::read(session, &Key::PendingJoin, |reader| {
                let reply = MailboxId::from_bytes(reader.fixed()?);
                let server =
                    pingers_proto::ServerId::new(reader.text()?).map_err(|_bad| corrupt())?;
                let subscription =
                    Subscription::decode(&reader.rest()).map_err(|_bad| corrupt())?;
                Ok((JoinId::for_reply_mailbox(reply), subscription, server))
            })
        })
        .await?
        .ok_or(PartiesError::UnknownJoin)?;
    let link = net.link(&server).await?;
    let engine = &link.engine;
    let read = engine
        .read_mailbox(&join.reply_mailbox(), None, WakeBudget::UNLIMITED)
        .await;
    link.observe(&read);
    let welcome = read?
        .messages
        .into_iter()
        .next()
        .ok_or(Unavailable::AwaitingWelcome)?
        .body;
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
                Ok(joined)
            })
        })
        .await?;
    engine
        .remember_subscription(&access.group, &subscription)
        .await?;
    net.push(&server, joined.outbound);
    net.flush().await?;
    Ok(joined.party)
}
