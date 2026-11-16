// SPDX-License-Identifier: AGPL-3.0-only
//! [`PartiesPort`] over pingers-parties' `PartyLifecycle`, one store session per call.
//!
//! Every `Outbound` value goes to pingers-sync through the [`Net`] outbox, flushed at once.
//! Joining, both the joiner's half (`open_invite`, `withdraw_join`, `dismiss_join`) and the
//! inviter's (`decide_join`; the invite mailboxes are read on wakes), is in `join.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pingers_geo::Fence;
use pingers_parties::{
    InviteId, InviteStatus as PartiesInviteStatus, JoinCode, Myself, NewInvite, NewParty, NewSpot,
    Outbound, PartiesError, PartyLifecycle, PartySettings, PostQuantum as PartiesPostQuantum,
    RelayChoice, SpotEdit, SpotShape,
};
use pingers_proto::{LeafIndex, MailboxId, PartyId, SecretText, ServerId, SpotId};
use pingers_store::{MemberName, Party as StoredParty, PartyName, Session, SpotName};
use pingers_time::Clock;

use super::Backend;
use super::records::{self, Key, corrupt};
use super::sync::Net;
use crate::ClientError;
use crate::ports::{
    AllowListChange, ClientEvent, JoinDecision, PartiesPort, PartiesSection, PartyDraft, SpotDraft,
};
use crate::snapshot::{
    Host, Hosting, Invite, InviteLink, InviteStatus, InviteUrl, JoinRequest, Joining, Me, Member,
    Name, Operator, Party, PartyColour, PartyConfig, PostQuantum, PushRelay, PushRelayKind,
    SafetyCode, Service, Spot,
};

/// The client's [`PartiesPort`].
pub(crate) struct Parties {
    backend: Arc<Backend>,
    net: Arc<Net>,
    /// The last invite link made per Party, for the snapshot's share sheet. In memory only:
    /// the link holds the invite secret, and pingers-parties does not hand it out again.
    links: Mutex<HashMap<PartyId, SharedLink>>,
}

/// An invite link and the invite it opens, so cancelling the invite withdraws the link.
#[derive(Clone)]
struct SharedLink {
    invite: MailboxId,
    link: InviteLink,
}

impl Parties {
    pub(crate) fn new(backend: Arc<Backend>, net: Arc<Net>) -> Self {
        Self {
            backend,
            net,
            links: Mutex::new(HashMap::new()),
        }
    }

    /// Hands `outbound` to pingers-sync and sends what it can now.
    async fn send(&self, outbound: impl IntoIterator<Item = Outbound>) -> Result<(), ClientError> {
        self.net.push(outbound);
        self.net.flush().await.map(drop)
    }

    fn links(&self) -> std::sync::MutexGuard<'_, HashMap<PartyId, SharedLink>> {
        self.links.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl PartiesPort for Parties {
    async fn create_party(&self, draft: PartyDraft) -> Result<PartyId, ClientError> {
        // The first Party makes the account (design B.1).
        self.net.ready().await?;
        let created = self
            .backend
            .store(move |session, clock, rng| {
                let server = account_server(session)?;
                let new = NewParty {
                    name: PartyName::new(draft.name.as_str().to_owned())?,
                    own_name: MemberName::new(draft.my_name.as_str().to_owned())?,
                    settings: PartySettings {
                        server,
                        relays: RelayChoice::ServerDefault,
                        post_quantum: match draft.post_quantum {
                            PostQuantum::On => PartiesPostQuantum::QuantumResistant,
                            PostQuantum::Off => PartiesPostQuantum::Classical,
                        },
                        location_visibility: None,
                    },
                    subscription: None,
                };
                let created = PartyLifecycle::new(session, clock).create_party(new, rng)?;
                records::write(session, &Key::Colour(&created.party), |writer| {
                    writer.byte(colour_code(draft.colour));
                })?;
                Ok(created)
            })
            .await?;
        self.send([created.outbound]).await?;
        Ok(created.party)
    }

    async fn create_invite(&self, party: PartyId) -> Result<InviteLink, ClientError> {
        // The code this device registered with travels in the link (design B.1).
        let registration_code = self.net.shared_registration_code().await?;
        let make = |code: Option<SecretText>| {
            self.backend.store(move |session, clock, rng| {
                let invite = NewInvite {
                    registration_code: code,
                };
                Ok(PartyLifecycle::new(session, clock).create_invite(&party, invite, rng)?)
            })
        };
        let created = match make(registration_code.clone()).await {
            // A fresh device has no invite mailbox pooled yet: register one (with the account
            // and credentials it needs) and try again.
            Err(ClientError::Parties(PartiesError::NoPooledMailbox)) => {
                self.net.ready().await?;
                super::join::refill_invite_pool(&self.net).await?;
                make(registration_code).await?
            }
            other => other?,
        };
        let link = InviteLink {
            url: InviteUrl::new(created.link.as_str()).map_err(|_invalid| corrupt())?,
            expires_at: created.expires,
        };
        let shared = SharedLink {
            invite: created.invite.mailbox(),
            link: link.clone(),
        };
        self.links().insert(party, shared);
        Ok(link)
    }

    async fn cancel_invite(&self, party: PartyId, invite: MailboxId) -> Result<(), ClientError> {
        let outbound = self
            .backend
            .store(move |session, clock, _rng| {
                let lifecycle = PartyLifecycle::new(session, clock);
                let invite = InviteId::for_mailbox(invite);
                // pingers-parties finds an invite by its mailbox alone; check it is this Party's.
                if !lifecycle
                    .invites(&party)?
                    .iter()
                    .any(|summary| summary.invite == invite)
                {
                    return Err(PartiesError::UnknownInvite.into());
                }
                Ok(lifecycle.revoke_invite(invite)?)
            })
            .await?;
        {
            let mut links = self.links();
            if links
                .get(&party)
                .is_some_and(|shared| shared.invite == invite)
            {
                links.remove(&party);
            }
        }
        self.send([outbound]).await
    }

    async fn set_display_name(&self, name: Name) -> Result<(), ClientError> {
        self.backend
            .store(move |session, _clock, _rng| {
                // Checked as pingers-parties checks a member name, so a join cannot fail on it.
                let name = MemberName::new(name.as_str().to_owned())?;
                records::write(session, &Key::DisplayName, |writer| {
                    writer.text(name.as_str());
                })
            })
            .await
    }

    /// Posts the join request (`join.rs`). Nothing about the Party is known until the Welcome.
    async fn open_invite(&self, link: InviteUrl, my_name: Name) -> Result<(), ClientError> {
        super::join::open_invite(&self.net, link, my_name).await
    }

    async fn withdraw_join(&self) -> Result<(), ClientError> {
        super::join::withdraw(&self.net).await
    }

    async fn dismiss_join(&self) -> Result<(), ClientError> {
        super::join::dismiss(&self.net).await
    }

    async fn check_joins(&self) -> Result<Vec<ClientEvent>, ClientError> {
        super::join::poll(&self.net).await
    }

    async fn decide_join(
        &self,
        party: PartyId,
        request: MailboxId,
        decision: JoinDecision,
    ) -> Result<(), ClientError> {
        super::join::decide(&self.net, party, request, decision).await
    }

    async fn remove_member(&self, party: PartyId, member: LeafIndex) -> Result<(), ClientError> {
        let commit = self
            .backend
            .store(move |session, clock, rng| {
                Ok(PartyLifecycle::new(session, clock).remove_member(&party, member, rng)?)
            })
            .await?;
        self.send([Outbound::Commit(commit)]).await?;
        Ok(())
    }

    async fn leave_party(&self, party: PartyId) -> Result<(), ClientError> {
        let outbound = self
            .backend
            .store(move |session, clock, rng| {
                let outbound = PartyLifecycle::new(session, clock).leave_party(&party, rng)?;
                records::delete(session, &Key::Colour(&party))?;
                Ok(outbound)
            })
            .await?;
        self.links().remove(&party);
        self.send(outbound).await?;
        Ok(())
    }

    async fn save_spot(&self, party: PartyId, spot: SpotDraft) -> Result<SpotId, ClientError> {
        let change = self
            .backend
            .store(move |session, clock, rng| {
                let lifecycle = PartyLifecycle::new(session, clock);
                let name = SpotName::new(spot.name.as_str().to_owned())?;
                let shape = shape(spot.fence);
                Ok(match spot.id {
                    None => lifecycle.create_spot(&party, NewSpot { name, shape }, rng)?,
                    Some(id) => {
                        let edit = SpotEdit {
                            spot: id,
                            name: Some(name),
                            shape: Some(shape),
                        };
                        lifecycle.edit_spot(&party, edit, rng)?
                    }
                })
            })
            .await?;
        self.send([change.outbound]).await?;
        Ok(change.spot)
    }

    async fn delete_spot(&self, party: PartyId, spot: SpotId) -> Result<(), ClientError> {
        let change = self
            .backend
            .store(move |session, clock, rng| {
                Ok(PartyLifecycle::new(session, clock).delete_spot(&party, &spot, rng)?)
            })
            .await?;
        self.send([change.outbound]).await?;
        Ok(())
    }

    async fn set_always_allow(
        &self,
        party: PartyId,
        member: LeafIndex,
        choice: AllowListChange,
    ) -> Result<(), ClientError> {
        self.backend
            .store(move |session, clock, _rng| {
                let key = PartyLifecycle::new(session, clock)
                    .roster(&party)?
                    .into_iter()
                    .find(|entry| entry.leaf == member)
                    .ok_or(PartiesError::UnknownMember)?
                    .signature_key;
                let allow_list = session.always_allow();
                match choice {
                    AllowListChange::Add => allow_list.add(&party, &key)?,
                    AllowListChange::Remove => allow_list.remove(&party, &key)?,
                }
                Ok(())
            })
            .await
    }

    async fn section(&self) -> Result<PartiesSection, ClientError> {
        let links: HashMap<PartyId, InviteLink> = self
            .links()
            .iter()
            .map(|(party, shared)| (*party, shared.link.clone()))
            .collect();
        self.backend
            .store(move |session, clock, _rng| {
                let parties = session
                    .parties()
                    .list()?
                    .into_iter()
                    .map(|row| party(session, clock, &row, &links))
                    .collect::<Result<Vec<_>, _>>()?;
                let display_name = records::read(session, &Key::DisplayName, |reader| {
                    name_of(&reader.text()?)
                })?;
                let joining = joining(session, clock, &parties)?;
                Ok(PartiesSection {
                    parties,
                    joining,
                    display_name,
                })
            })
            .await
    }
}

/// The server this device's account is registered with.
fn account_server(session: &Session) -> Result<ServerId, ClientError> {
    Ok(session
        .identity()
        .load()?
        .ok_or(PartiesError::NoAccount)?
        .server)
}

/// One Party as the snapshot shows it.
fn party(
    session: &Session,
    clock: &dyn Clock,
    row: &StoredParty,
    links: &HashMap<PartyId, InviteLink>,
) -> Result<Party, ClientError> {
    let id = row.id;
    let lifecycle = PartyLifecycle::new(session, clock);
    let settings = lifecycle.settings(&id)?;
    let members = lifecycle
        .roster(&id)?
        .into_iter()
        .filter_map(|entry| {
            let name = entry.name?;
            Some(name_of(name.as_str()).map(|name| Member {
                leaf: entry.leaf,
                name,
                me: match entry.is_self {
                    Myself::Yes => Me::Me,
                    Myself::No => Me::Other,
                },
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let spots = lifecycle
        .spots(&id)?
        .into_iter()
        .map(|spot| {
            Ok(Spot {
                id: spot.id,
                name: name_of(spot.name.as_str())?,
                fence: spot.fence,
            })
        })
        .collect::<Result<Vec<_>, ClientError>>()?;
    let summaries = lifecycle.invites(&id)?;
    let invites = summaries
        .iter()
        .rev()
        .filter(|invite| invite.status == PartiesInviteStatus::Open)
        .map(|invite| Invite {
            id: invite.invite.mailbox(),
            status: InviteStatus::Pending {
                shared_at: invite.shared_at,
                expires_at: invite.expires,
            },
        })
        .collect();
    let join_requests = lifecycle
        .pending_joins(&id)?
        .into_iter()
        .map(|pending| {
            let shared_at = summaries
                .iter()
                .find(|summary| summary.invite == pending.invite)
                .map_or(pending.asked_at, |summary| summary.shared_at);
            Ok(JoinRequest {
                id: pending.join.reply_mailbox(),
                name: name_of(pending.name.as_str())?,
                code: code_of(pending.code)?,
                asked_at: pending.asked_at,
                shared_at,
            })
        })
        .collect::<Result<Vec<_>, ClientError>>()?;
    let colour = records::read(session, &Key::Colour(&id), |reader| {
        colour_from_code(reader.byte()?)
    })?
    .unwrap_or(PartyColour::Blue);
    Ok(Party {
        id,
        name: name_of(row.name.as_str())?,
        colour,
        members,
        spots,
        config: config(&settings)?,
        invites,
        invite_link: links.get(&id).cloned(),
        join_requests,
    })
}

/// The join this device started: just let in (until the user opens the Party), or asked.
fn joining(
    session: &Session,
    clock: &dyn Clock,
    parties: &[Party],
) -> Result<Option<Joining>, ClientError> {
    let lifecycle = PartyLifecycle::new(session, clock);
    // A Party left since, or an inviter removed since, leaves nothing to show.
    if let Some(party) = super::join::let_in(session)?
        && let Some(via) = lifecycle.joined_via(&party)?
        && let Some(inviter) = parties
            .iter()
            .find(|row| row.id == party)
            .and_then(|row| row.members.iter().find(|member| member.leaf == via.inviter))
    {
        return Ok(Some(Joining::LetIn {
            party,
            inviter: inviter.name.clone(),
            code: code_of(via.code)?,
        }));
    }
    let Some(join) = super::join::asked(session)? else {
        return Ok(None);
    };
    let asked = lifecycle.asked_join(join)?;
    Ok(Some(Joining::Asked {
        code: code_of(asked.code)?,
        asked_at: asked.asked_at,
    }))
}

/// A join code as both screens show it ("4821 0937 5512").
fn code_of(code: JoinCode) -> Result<SafetyCode, ClientError> {
    SafetyCode::new(&code.to_string()).map_err(|_never| corrupt())
}

/// Who runs what for a Party.
///
/// WAITING(pingers-sync): operator declarations, the hosting kind and the relays come from the
/// server's `/v1/config`, which pingers-sync reads. Until then the snapshot names the Party's
/// server for every service and "undeclared" for every operator.
fn config(settings: &PartySettings) -> Result<PartyConfig, ClientError> {
    let origin = settings.server.as_str();
    let host = Host::new(origin.strip_prefix("https://").unwrap_or(origin))
        .map_err(|_invalid| corrupt())?;
    let operator = Operator::new("undeclared").map_err(|_invalid| corrupt())?;
    Ok(PartyConfig {
        server: Service {
            host: host.clone(),
            operator: operator.clone(),
        },
        hosting: Hosting::Public,
        relay: Service {
            host,
            operator: operator.clone(),
        },
        push_relay: PushRelay {
            kind: PushRelayKind::Official,
            operator,
        },
        post_quantum: match settings.post_quantum {
            PartiesPostQuantum::QuantumResistant => PostQuantum::On,
            PartiesPostQuantum::Classical => PostQuantum::Off,
        },
    })
}

fn name_of(text: &str) -> Result<Name, ClientError> {
    Name::new(text).map_err(|_invalid| corrupt())
}

/// The editor's fence as pingers-parties takes it (it validates the shape again).
fn shape(fence: Fence) -> SpotShape {
    match fence {
        Fence::Circle(circle) => SpotShape::Circle {
            center: circle.center(),
            radius: circle.radius(),
        },
        Fence::Polygon(polygon) => SpotShape::Polygon(polygon.vertices().to_vec()),
    }
}

const fn colour_code(colour: PartyColour) -> u8 {
    match colour {
        PartyColour::Blue => 0,
        PartyColour::Orange => 1,
        PartyColour::Ink => 2,
        PartyColour::Teal => 3,
        PartyColour::Plum => 4,
    }
}

fn colour_from_code(code: u8) -> Result<PartyColour, ClientError> {
    match code {
        0 => Ok(PartyColour::Blue),
        1 => Ok(PartyColour::Orange),
        2 => Ok(PartyColour::Ink),
        3 => Ok(PartyColour::Teal),
        4 => Ok(PartyColour::Plum),
        _ => Err(corrupt()),
    }
}
