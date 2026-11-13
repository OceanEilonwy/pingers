// SPDX-License-Identifier: AGPL-3.0-only
//! The Meetup's MLS group as this crate uses it: members, padded frames in and out, and the
//! Party roster and consent settings it is checked against.

use pingers_crypto::AccountSigningKey;
use pingers_meetup::{AlwaysAllow, LocationVisibility, Member, OutgoingFrame};
use pingers_mls::{Group, Meetup, MlsContext, Roster, RosterEntry};
use pingers_proto::{
    Delivery, Epoch, LiveFrame, MeetupMessage, MlsMessageBytes, Padded, PartyId, SizeClass,
    Urgency, WireDecode as _, WireEncode as _,
};
use pingers_store::Session;
use rand_core::CryptoRng;

use crate::SessionError;

/// A loaded Meetup group over the store's session.
pub(crate) type MeetupGroup<'session> = Group<'session, Session, Meetup>;

/// The store session of one step and the account key that signs in MLS groups.
#[derive(Clone, Copy)]
pub(crate) struct Local<'session> {
    /// The open session, inside the step's transaction.
    pub(crate) session: &'session Session,
    /// The account key (the leaf signature key in every group, design B.1).
    pub(crate) signer: &'session AccountSigningKey,
}

impl<'session> Local<'session> {
    /// An MLS context over the session.
    pub(crate) const fn mls(self) -> MlsContext<'session, Session> {
        MlsContext::new(self.session, self.signer)
    }
}

/// Every member of the group with its signature and grant key, as the session needs them.
///
/// # Errors
///
/// [`SessionError::Mls`] if a leaf lacks its grant key; [`SessionError::Internal`] if the
/// roster and the grant keys name different leaves.
pub(crate) fn members_of(group: &MeetupGroup<'_>) -> Result<Vec<Member>, SessionError> {
    let roster = group.roster();
    group
        .grant_keys()?
        .into_iter()
        .map(|grant| {
            roster
                .signature_key(grant.leaf)
                .map(|signature_key| Member {
                    leaf: grant.leaf,
                    signature_key: *signature_key,
                    grant_key: grant.grant_key,
                })
                .ok_or(SessionError::Internal)
        })
        .collect()
}

/// A frame ready for the network: the live-channel envelope, and whether to wake members
/// whose channel is down (only the group log can do that).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outgoing {
    pub(crate) frame: LiveFrame,
    pub(crate) wake: Option<Urgency>,
}

impl Outgoing {
    /// The bytes of one live-channel chunk.
    pub(crate) fn live_bytes(&self) -> Result<Vec<u8>, SessionError> {
        Ok(self.frame.encode()?)
    }

    /// Whether the server keeps it in the group log.
    pub(crate) fn is_stored(&self) -> bool {
        self.frame.delivery == Delivery::Stored
    }
}

/// Pads `frame` to the smallest Meetup size class and encrypts it into the group.
///
/// # Errors
///
/// [`SessionError::Proto`] if it does not encode or fits no class; [`SessionError::Mls`]
/// (`CommitPending` while a commit or a received proposal waits).
pub(crate) fn seal(
    group: &mut MeetupGroup<'_>,
    frame: &OutgoingFrame,
    rng: &mut impl CryptoRng,
) -> Result<Outgoing, SessionError> {
    let padded = Padded::pad_smallest(&frame.message.encode()?, &SizeClass::MEETUP_FRAME)?;
    let body = group.seal_application(&padded, rng)?;
    Ok(Outgoing {
        frame: LiveFrame {
            epoch: group.epoch(),
            body,
            delivery: frame.delivery,
        },
        wake: frame.wake,
    })
}

/// A standalone MLS message (a proposal) as a stored frame.
pub(crate) const fn stored(epoch: Epoch, body: MlsMessageBytes) -> Outgoing {
    Outgoing {
        frame: LiveFrame {
            epoch,
            body,
            delivery: Delivery::Stored,
        },
        wake: None,
    }
}

/// The Meetup frame inside a decrypted application message.
///
/// # Errors
///
/// [`SessionError::Proto`] if it is not a padded, canonical frame.
pub(crate) fn open(plaintext: &[u8]) -> Result<MeetupMessage, SessionError> {
    let padded = Padded::from_wire(plaintext.to_vec())?;
    Ok(MeetupMessage::decode(padded.payload()?)?)
}

/// What this device knows about the Party a Meetup formed in.
pub(crate) struct PartyContext {
    /// Every Party member, this device included: the Meetup roster check (design B.5).
    pub(crate) roster: Roster,
    /// The visibility in force: the Party's override, else the device setting.
    pub(crate) visibility: LocationVisibility,
    /// The Party's allow-list.
    pub(crate) always_allow: AlwaysAllow,
}

/// Reads the Party's roster and consent settings from the store.
///
/// # Errors
///
/// [`SessionError::NotFound`] if the Party or the identity is missing;
/// [`SessionError::Store`].
pub(crate) fn party_context(
    local: Local<'_>,
    party: &PartyId,
) -> Result<PartyContext, SessionError> {
    let Local { session, signer } = local;
    let record = session
        .parties()
        .get(party)?
        .ok_or(SessionError::NotFound)?;
    let own_key = signer.public_key();
    let members = session.members().list(party)?;
    let allow_list = session.always_allow();
    let mut allowed = Vec::new();
    for member in &members {
        if allow_list.contains(party, &member.signature_key)? {
            allowed.push(member.signature_key);
        }
    }
    let mut entries: Vec<RosterEntry> = members
        .iter()
        .filter(|member| member.signature_key != own_key)
        .map(|member| RosterEntry {
            leaf: member.leaf,
            signature_key: member.signature_key,
        })
        .collect();
    entries.push(RosterEntry {
        leaf: record.own_leaf,
        signature_key: own_key,
    });
    entries.sort_by_key(|entry| entry.leaf);
    let visibility = match record.location_visibility {
        Some(visibility) => visibility,
        None => session.settings().location_visibility()?,
    };
    Ok(PartyContext {
        roster: Roster::new(entries),
        visibility: match visibility {
            pingers_store::LocationVisibility::AskEachTime => LocationVisibility::AskEachTime,
            pingers_store::LocationVisibility::Everyone => LocationVisibility::Everyone,
            pingers_store::LocationVisibility::Nobody => LocationVisibility::Nobody,
        },
        always_allow: AlwaysAllow::new(allowed),
    })
}
