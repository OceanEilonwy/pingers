// SPDX-License-Identifier: AGPL-3.0-only
//! [`PartiesPort`] over pingers-parties' `PartyLifecycle`, one store session per call.
//!
//! Every `Outbound` value goes to pingers-sync through the [`Net`] outbox, flushed at once. Joining (`open_invite`,
//! `accept_invite`) and admitting (`decide_join`) need a subscription, a reply mailbox or a
//! join request that only pingers-sync can provide, so they answer [`Unavailable::Sync`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use pingers_geo::Fence;
use pingers_parties::{
    INVITE_LIFETIME, InviteStatus as PartiesInviteStatus, Myself, NewParty, NewSpot, Outbound,
    PartiesError, PartyLifecycle, PartySettings, PostQuantum as PartiesPostQuantum, RelayChoice,
    SpotEdit, SpotShape,
};
use pingers_proto::{InviteUses, LeafIndex, MailboxId, PartyId, ServerId, SpotId};
use pingers_store::{MemberName, Party as StoredParty, PartyName, Session, SpotName};
use pingers_time::{Clock, UnixSeconds};

use super::Backend;
use super::records::{self, Key, corrupt};
use super::sync::Net;
use crate::ports::{
    AllowListChange, JoinDecision, PartiesPort, PartiesSection, PartyDraft, SpotDraft,
};
use crate::snapshot::{
    Host, Hosting, Invite, InviteLink, InviteStatus, InviteUrl, Me, Member, Name, Operator, Party,
    PartyColour, PartyConfig, PostQuantum, PushRelay, PushRelayKind, Service, Spot,
};
use crate::{ClientError, Unavailable};

/// The client's [`PartiesPort`].
pub(crate) struct Parties {
    backend: Arc<Backend>,
    net: Arc<Net>,
    /// The last invite link made per Party, for the snapshot's share sheet. In memory only:
    /// the link holds the invite secret, and pingers-parties does not hand it out again.
    links: Mutex<HashMap<PartyId, InviteLink>>,
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

    fn links(&self) -> std::sync::MutexGuard<'_, HashMap<PartyId, InviteLink>> {
        self.links.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl PartiesPort for Parties {
    async fn create_party(&self, draft: PartyDraft) -> Result<PartyId, ClientError> {
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
        let created = self
            .backend
            .store(move |session, clock, rng| {
                Ok(PartyLifecycle::new(session, clock).create_invite(
                    &party,
                    InviteUses::Multiple,
                    rng,
                )?)
            })
            .await?;
        let link = InviteLink {
            url: InviteUrl::new(created.link.as_str()).map_err(|_invalid| corrupt())?,
            expires_at: created.expires,
        };
        self.links().insert(party, link.clone());
        Ok(link)
    }

    async fn open_invite(&self, _link: InviteUrl, _my_name: Name) -> Result<(), ClientError> {
        Err(Unavailable::Sync.into())
    }

    async fn accept_invite(&self) -> Result<PartyId, ClientError> {
        Err(Unavailable::Sync.into())
    }

    async fn decide_join(
        &self,
        _party: PartyId,
        _invite: MailboxId,
        _decision: JoinDecision,
    ) -> Result<(), ClientError> {
        Err(Unavailable::Sync.into())
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
        let links = self.links().clone();
        let parties = self
            .backend
            .store(move |session, clock, _rng| {
                session
                    .parties()
                    .list()?
                    .into_iter()
                    .map(|row| party(session, clock, &row, &links))
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        Ok(PartiesSection {
            parties,
            join_preview: None,
        })
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
    let invites = lifecycle
        .invites(&id)?
        .into_iter()
        .rev()
        .filter(|invite| invite.status == PartiesInviteStatus::Open)
        .map(|invite| Invite {
            id: invite.invite.mailbox(),
            status: InviteStatus::Pending {
                // pingers-parties keeps the expiry, not the creation time; the creation time is
                // the expiry less the 24 h lifetime unless the mailbox expired sooner.
                shared_at: UnixSeconds::new(
                    invite
                        .expires
                        .get()
                        .saturating_sub(INVITE_LIFETIME.as_secs()),
                ),
                expires_at: invite.expires,
            },
        })
        .collect();
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
    })
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
