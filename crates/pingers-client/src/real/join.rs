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
use pingers_sync::{GroupCall, PoolTarget, WakeBudget};

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
    let engine = net.engine()?;
    net.ensure_wake_push().await?;
    let subscription = engine
        .party_subscription(TimeToLive::new(PARTY_SUBSCRIPTION).map_err(|_never| corrupt())?)?
        .ok_or(Unavailable::WakePushesOff)?;
    let link = InviteLink::from_text(link.as_str().to_owned());
    let display_name = DisplayName::new(name.as_str().to_owned()).map_err(PartiesError::Proto)?;
    let prepared = match prepare(net, &link, &display_name, &subscription).await {
        Err(ClientError::Parties(PartiesError::NoPooledMailbox)) => {
            let target = PoolTarget {
                purpose: MailboxPurpose::Reply,
                size: 1,
                lifetime: TimeToLive::new(REPLY_LIFETIME).map_err(|_never| corrupt())?,
            };
            let refilled = engine
                .refill_mailbox_pool(&target, WakeBudget::UNLIMITED)
                .await;
            net.observe(&refilled);
            let _registered: pingers_sync::PoolRefill = refilled?;
            prepare(net, &link, &display_name, &subscription).await?
        }
        other => other?,
    };
    net.push([prepared]);
    net.flush().await.map(drop)
}

/// pingers-parties' join request, and the join recorded with its subscription.
async fn prepare(
    net: &Net,
    link: &InviteLink,
    name: &DisplayName,
    subscription: &Subscription,
) -> Result<pingers_parties::Outbound, ClientError> {
    let link = InviteLink::from_text(link.as_str().to_owned());
    let name = name.clone();
    let subscription = subscription.clone();
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
    let engine = net.engine()?;
    let (join, subscription) = net
        .backend
        .store(|session, _clock, _rng| {
            records::read(session, &Key::PendingJoin, |reader| {
                let reply = MailboxId::from_bytes(reader.fixed()?);
                let subscription =
                    Subscription::decode(&reader.rest()).map_err(|_bad| corrupt())?;
                Ok((JoinId::for_reply_mailbox(reply), subscription))
            })
        })
        .await?
        .ok_or(PartiesError::UnknownJoin)?;
    let read = engine
        .read_mailbox(&join.reply_mailbox(), None, WakeBudget::UNLIMITED)
        .await;
    net.observe(&read);
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
                Ok(joined)
            })
        })
        .await?;
    engine
        .remember_subscription(&access.group, &subscription)
        .await?;
    net.push(joined.outbound);
    net.flush().await?;
    Ok(joined.party)
}
