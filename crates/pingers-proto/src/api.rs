// SPDX-License-Identifier: AGPL-3.0-only
//! Server API requests and responses not covered by a more specific module (design D).
//!
//! Besides the structs the contract fixes, this module carries the other request and response
//! bodies of design D: account registration and auth answers, the membership tree, mailbox
//! reads, group log reads and group state. Privacy Pass issuance is in `privacy_pass`.

use minicbor::Decoder;
use pingers_time::UnixSeconds;

use crate::cbor::{self, CborBody, Writer, versioned_cbor};
use crate::secret::option_ct_eq;
use crate::{
    AccountId, BoardPage, Epoch, GroupLogEntry, KeyConfigHash, Lease, LeaseId, Padded, ProtoError,
    Redacted, RelayId, RelayUrl, SealedSnapshot, SecretBytes, SecretText, SequenceNumber,
};
use subtle::ConstantTimeEq as _;

/// A push relay the server advertises (design D, `PushRelayInfo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRelayInfo {
    /// The id mailboxes refer to it by.
    pub relay_id: RelayId,
    /// Where it is.
    pub url: RelayUrl,
    /// Hash of its key config, to pin.
    pub key_config_hash: KeyConfigHash,
    /// Who runs it, as declared; shown in "What your server sees".
    pub operator: String,
}

/// An anonymous credential a server accepts (design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AnonymousCredential {
    /// RLN membership proofs.
    MembershipProof,
    /// Privacy Pass tokens.
    PrivacyPass,
}

/// Whether the server slashes a device that reuses a nullifier with a different body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slashing {
    /// Duplicates are rejected and the leaked share is used to ban the account.
    On,
    /// Duplicates are rejected; shares are ignored (the launch default).
    Off,
}

/// Whether devices may register their own push relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CustomPushRelays {
    /// `RelayRef::Custom` is accepted.
    Allowed,
    /// Only listed relays are accepted.
    NotAllowed,
}

/// Probability that a routine cover upload also reads the board (design B.4.3): 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct CoverReadRatio(f32);

impl CoverReadRatio {
    /// Every upload reads (the default).
    pub const ALWAYS: Self = Self(1.0);

    /// Checks the range.
    ///
    /// # Errors
    ///
    /// [`ProtoError::OutOfRange`] unless `ratio` is a number from 0 to 1.
    pub fn new(ratio: f32) -> Result<Self, ProtoError> {
        if (0.0..=1.0).contains(&ratio) {
            Ok(Self(ratio))
        } else {
            Err(ProtoError::OutOfRange {
                field: "cover read ratio",
            })
        }
    }

    /// The probability.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// `GET /v1/params` (design D). Server configuration every client needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// The credentials the server accepts for anonymous requests.
    pub anonymous_credentials: Vec<AnonymousCredential>,
    /// Whether the server slashes.
    pub rln_slashing: Slashing,
    /// How often cover uploads read the board.
    pub cover_read_ratio: CoverReadRatio,
    /// Records per board page (a count at the wire boundary).
    pub board_page_records: u32,
    /// The OHTTP relays to use; never empty for a usable server.
    pub ohttp_relays: Vec<RelayUrl>,
    /// The push relays the server can forward to.
    pub push_relays: Vec<PushRelayInfo>,
    /// Whether devices may register their own push relay.
    pub custom_push_relays: CustomPushRelays,
    /// Who runs the server, as declared.
    pub server_operator: String,
    /// The server's clock, for skew correction (design F.1).
    pub server_time: UnixSeconds,
}

/// `POST /v1/accounts` (design D). `==` compares the registration code in constant time.
#[derive(Clone, Debug)]
pub struct RegisterRequest {
    /// The account's Ed25519 public key.
    pub signature_key: [u8; 32],
    /// Signature proving possession of the key.
    pub proof: [u8; 64],
    /// The operator-issued registration code, if the server requires one.
    pub registration_code: Option<SecretText>,
}

impl PartialEq for RegisterRequest {
    fn eq(&self, other: &Self) -> bool {
        self.signature_key == other.signature_key
            && self.proof == other.proof
            && option_ct_eq(
                self.registration_code.as_ref(),
                other.registration_code.as_ref(),
            )
    }
}

impl Eq for RegisterRequest {}

/// `POST /v1/auth` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRequest {
    /// Who is authenticating.
    pub account_id: AccountId,
    /// Signature over the server's nonce.
    pub nonce_signature: [u8; 64],
}

/// The lease a board sync replaces, proven by revealing its revoke key. `==` compares the key
/// in constant time; the server checks it with pingers-crypto's `revoke_key_matches`.
#[derive(Clone, Debug)]
pub struct LeaseReplacement {
    /// The lease being replaced.
    pub lease_id: LeaseId,
    /// The revoke key whose SHA-256 is that lease's commitment.
    pub revoke_key: SecretBytes<32>,
}

impl PartialEq for LeaseReplacement {
    fn eq(&self, other: &Self) -> bool {
        self.lease_id == other.lease_id && bool::from(self.revoke_key.ct_eq(&other.revoke_key))
    }
}

impl Eq for LeaseReplacement {}

/// Whether a board sync also returns the first page of the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadBoard {
    /// Return the first page (taken after the append).
    Read,
    /// Upload only (cover upload under `cover_read_ratio`, or "Low data").
    Skip,
}

/// `POST /v1/board/sync` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardSyncRequest {
    /// The new lease.
    pub lease: Lease,
    /// The lease it replaces, if any.
    pub replaces: Option<LeaseReplacement>,
    /// Whether to read the board.
    pub read: ReadBoard,
}

/// The answer to a board sync (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardSyncResponse {
    /// The id of the lease just stored.
    pub lease_id: LeaseId,
    /// The server's clock.
    pub server_time: UnixSeconds,
    /// The first page, if a read was asked for.
    pub page: Option<BoardPage>,
}

/// The answer to `POST /v1/accounts` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterResponse {
    /// The new account.
    pub account_id: AccountId,
}

/// The answer to `POST /v1/auth` (design D). `==` compares the bearer in constant time.
#[derive(Clone, Debug)]
pub struct AuthResponse {
    /// The bearer token for `Authorization: Bearer`, valid for one hour.
    pub bearer: SecretText,
    /// The server's clock.
    pub server_time: UnixSeconds,
}

impl PartialEq for AuthResponse {
    fn eq(&self, other: &Self) -> bool {
        self.server_time == other.server_time && bool::from(self.bearer.ct_eq(&other.bearer))
    }
}

impl Eq for AuthResponse {}

/// `POST /v1/rln/leaf` (design D): adds the account's identity commitment to the membership
/// tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipLeafRequest {
    /// `Poseidon(sk)`, a BN254 scalar field element.
    pub id_commitment: Redacted<[u8; 32]>,
}

/// The answer to `POST /v1/rln/leaf` (design D).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipLeafResponse {
    /// The leaf's position in the tree.
    pub index: MembershipIndex,
    /// The first hourly root that contains the leaf.
    pub live_from: RootEpoch,
}

/// One hourly root of the membership tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipRoot {
    /// Which hour.
    pub epoch: RootEpoch,
    /// The root.
    pub root: [u8; 32],
}

/// The answer to `GET /v1/rln/leaves?after=n` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipLeaves {
    /// Leaves after the requested index, in order.
    pub leaves: Vec<[u8; 32]>,
    /// Leaves that have been zeroed (banned or deleted accounts).
    pub zeroed: Vec<MembershipIndex>,
    /// The recent hourly roots.
    pub roots: Vec<MembershipRoot>,
    /// Where the next request starts.
    pub next: MembershipIndex,
}

/// One message of a mailbox read (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxMessage {
    /// Its position in the mailbox.
    pub sequence: SequenceNumber,
    /// The padded post, as the poster sent it (one of [`crate::SizeClass::MAILBOX_POST`]).
    pub body: Padded,
}

/// The answer to `GET /v1/mailboxes/{id}/msgs?after=seq` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxMessages {
    /// The messages, oldest first.
    pub messages: Vec<MailboxMessage>,
}

/// The answer to `POST /v1/groups/{id}/msgs` (design D).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageAccepted {
    /// The stored message's log position.
    pub sequence: SequenceNumber,
}

/// The answer to `GET /v1/groups/{id}/msgs?after=seq` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupMessages {
    /// The log entries, oldest first.
    pub messages: Vec<GroupLogEntry>,
    /// The log's current epoch.
    pub head_epoch: Epoch,
}

/// The answer to `GET /v1/groups/{id}/state` (design D).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupState {
    /// The epoch the snapshot belongs to.
    pub epoch: Epoch,
    /// The sealed snapshot.
    pub state: SealedSnapshot,
}

/// A position in the RLN membership tree (design B.4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MembershipIndex(u32);

impl MembershipIndex {
    /// Wraps an index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// An hour of membership tree roots (design B.4.8, `root_epoch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootEpoch(u32);

impl RootEpoch {
    /// Wraps an epoch.
    #[must_use]
    pub const fn new(epoch: u32) -> Self {
        Self(epoch)
    }

    /// The epoch.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Reads a server timestamp.
fn read_time(decoder: &mut Decoder<'_>) -> Result<UnixSeconds, ProtoError> {
    Ok(UnixSeconds::new(decoder.u64()?))
}

impl AnonymousCredential {
    const fn to_wire(self) -> u8 {
        match self {
            Self::MembershipProof => 0,
            Self::PrivacyPass => 1,
        }
    }
}

impl CborBody for AnonymousCredential {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.unsigned(self.to_wire());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        match decoder.u8()? {
            0 => Ok(Self::MembershipProof),
            1 => Ok(Self::PrivacyPass),
            _ => Err(ProtoError::OutOfRange {
                field: "anonymous credential",
            }),
        }
    }
}

impl CborBody for RelayUrl {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        self.encode_cbor(writer)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        Self::decode_cbor(decoder)
    }
}

/// Layout: `[relay id, url, key config hash, operator]`.
impl CborBody for PushRelayInfo {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.unsigned(self.relay_id.get());
        self.url.encode_cbor(writer)?;
        writer.bytes(self.key_config_hash.as_bytes())?;
        writer.text(&self.operator)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            relay_id: RelayId::new(decoder.u16()?),
            url: RelayUrl::decode_cbor(decoder)?,
            key_config_hash: KeyConfigHash::from_bytes(cbor::fixed_bytes(decoder)?),
            operator: decoder.str()?.to_owned(),
        })
    }
}

/// Layout: `[credentials, slashing (0 off, 1 on), cover read ratio (f32), board page records,
/// OHTTP relays, push relays, custom push relays (0 not allowed, 1 allowed), operator,
/// server time]`.
impl CborBody for Params {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(9)?;
        writer.list(&self.anonymous_credentials)?;
        writer.unsigned(match self.rln_slashing {
            Slashing::Off => 0_u8,
            Slashing::On => 1,
        });
        // The constructor admits -0.0; writing its magnitude keeps one encoding per value.
        writer.float(self.cover_read_ratio.get().abs());
        writer.unsigned(self.board_page_records);
        writer.list(&self.ohttp_relays)?;
        writer.list(&self.push_relays)?;
        writer.unsigned(match self.custom_push_relays {
            CustomPushRelays::NotAllowed => 0_u8,
            CustomPushRelays::Allowed => 1,
        });
        writer.text(&self.server_operator)?;
        writer.unsigned(self.server_time.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 9)?;
        Ok(Self {
            anonymous_credentials: cbor::list(decoder)?,
            rln_slashing: match decoder.u8()? {
                0 => Slashing::Off,
                1 => Slashing::On,
                _ => return Err(ProtoError::OutOfRange { field: "slashing" }),
            },
            cover_read_ratio: CoverReadRatio::new(decoder.f32()?)?,
            board_page_records: decoder.u32()?,
            ohttp_relays: cbor::list(decoder)?,
            push_relays: cbor::list(decoder)?,
            custom_push_relays: match decoder.u8()? {
                0 => CustomPushRelays::NotAllowed,
                1 => CustomPushRelays::Allowed,
                _ => {
                    return Err(ProtoError::OutOfRange {
                        field: "custom push relays",
                    });
                }
            },
            server_operator: decoder.str()?.to_owned(),
            server_time: read_time(decoder)?,
        })
    }
}

/// Layout: `[signature key, proof, registration code or null]`.
impl CborBody for RegisterRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(3)?;
        writer.bytes(&self.signature_key)?;
        writer.bytes(&self.proof)?;
        writer.option(self.registration_code.as_ref(), |writer, code| {
            writer.text(code.expose())
        })
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 3)?;
        Ok(Self {
            signature_key: cbor::fixed_bytes(decoder)?,
            proof: cbor::fixed_bytes(decoder)?,
            registration_code: cbor::option(decoder, |decoder| {
                Ok(SecretText::new(decoder.str()?.to_owned()))
            })?,
        })
    }
}

/// Layout: `[account id, nonce signature]`.
impl CborBody for AuthRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.bytes(self.account_id.as_bytes())?;
        writer.bytes(&self.nonce_signature)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            account_id: AccountId::from_bytes(cbor::fixed_bytes(decoder)?),
            nonce_signature: cbor::fixed_bytes(decoder)?,
        })
    }
}

/// Layout: `[lease id, revoke key]`.
impl CborBody for LeaseReplacement {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.bytes(self.lease_id.as_bytes())?;
        writer.bytes(self.revoke_key.expose())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            lease_id: LeaseId::from_bytes(cbor::fixed_bytes(decoder)?),
            revoke_key: SecretBytes::new(cbor::fixed_bytes(decoder)?),
        })
    }
}

/// Layout: `[lease (424 bytes, fixed layout), replacement or null, read (0 skip, 1 read)]`.
impl CborBody for BoardSyncRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(3)?;
        writer.bytes(&self.lease.to_bytes())?;
        writer.option(self.replaces.as_ref(), |writer, replaces| {
            replaces.encode_body(writer)
        })?;
        writer.unsigned(match self.read {
            ReadBoard::Skip => 0_u8,
            ReadBoard::Read => 1,
        });
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 3)?;
        Ok(Self {
            lease: Lease::from_bytes(decoder.bytes()?)?,
            replaces: cbor::option(decoder, LeaseReplacement::decode_body)?,
            read: match decoder.u8()? {
                0 => ReadBoard::Skip,
                1 => ReadBoard::Read,
                _ => {
                    return Err(ProtoError::OutOfRange {
                        field: "read board",
                    });
                }
            },
        })
    }
}

/// Layout: `[lease id, server time, page or null]`.
impl CborBody for BoardSyncResponse {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(3)?;
        writer.bytes(self.lease_id.as_bytes())?;
        writer.unsigned(self.server_time.get());
        writer.option(self.page.as_ref(), |writer, page| page.encode_body(writer))
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 3)?;
        Ok(Self {
            lease_id: LeaseId::from_bytes(cbor::fixed_bytes(decoder)?),
            server_time: read_time(decoder)?,
            page: cbor::option(decoder, BoardPage::decode_body)?,
        })
    }
}

/// Layout: `[account id]`.
impl CborBody for RegisterResponse {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.bytes(self.account_id.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 1)?;
        Ok(Self {
            account_id: AccountId::from_bytes(cbor::fixed_bytes(decoder)?),
        })
    }
}

/// Layout: `[bearer, server time]`.
impl CborBody for AuthResponse {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.text(self.bearer.expose())?;
        writer.unsigned(self.server_time.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            bearer: SecretText::new(decoder.str()?.to_owned()),
            server_time: read_time(decoder)?,
        })
    }
}

/// Layout: `[id commitment]`.
impl CborBody for MembershipLeafRequest {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.bytes(self.id_commitment.get())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 1)?;
        Ok(Self {
            id_commitment: Redacted::new(cbor::fixed_bytes(decoder)?),
        })
    }
}

/// Layout: `[index, live from root epoch]`.
impl CborBody for MembershipLeafResponse {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.unsigned(self.index.get());
        writer.unsigned(self.live_from.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            index: MembershipIndex::new(decoder.u32()?),
            live_from: RootEpoch::new(decoder.u32()?),
        })
    }
}

impl CborBody for [u8; 32] {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.bytes(self)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::fixed_bytes(decoder)
    }
}

impl CborBody for MembershipIndex {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.unsigned(self.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        Ok(Self::new(decoder.u32()?))
    }
}

/// Layout: `[root epoch, root]`.
impl CborBody for MembershipRoot {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.unsigned(self.epoch.get());
        writer.bytes(&self.root)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            epoch: RootEpoch::new(decoder.u32()?),
            root: cbor::fixed_bytes(decoder)?,
        })
    }
}

/// Layout: `[leaves, zeroed indexes, roots, next]`.
impl CborBody for MembershipLeaves {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(4)?;
        writer.list(&self.leaves)?;
        writer.list(&self.zeroed)?;
        writer.list(&self.roots)?;
        writer.unsigned(self.next.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 4)?;
        Ok(Self {
            leaves: cbor::list(decoder)?,
            zeroed: cbor::list(decoder)?,
            roots: cbor::list(decoder)?,
            next: MembershipIndex::new(decoder.u32()?),
        })
    }
}

/// Layout: `[sequence, padded body]`; the body's length must be a size class.
impl CborBody for MailboxMessage {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.unsigned(self.sequence.get());
        writer.bytes(self.body.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            sequence: SequenceNumber::new(decoder.u64()?),
            body: Padded::from_wire(decoder.bytes()?.to_vec())?,
        })
    }
}

/// Layout: `[messages]`.
impl CborBody for MailboxMessages {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.list(&self.messages)
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 1)?;
        Ok(Self {
            messages: cbor::list(decoder)?,
        })
    }
}

/// Layout: `[sequence]`.
impl CborBody for MessageAccepted {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(1)?;
        writer.unsigned(self.sequence.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 1)?;
        Ok(Self {
            sequence: SequenceNumber::new(decoder.u64()?),
        })
    }
}

/// Layout: `[entries, head epoch]`.
impl CborBody for GroupMessages {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.list(&self.messages)?;
        writer.unsigned(self.head_epoch.get());
        Ok(())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            messages: cbor::list(decoder)?,
            head_epoch: Epoch::new(decoder.u64()?),
        })
    }
}

/// Layout: `[epoch, snapshot]`.
impl CborBody for GroupState {
    fn encode_body(&self, writer: &mut Writer) -> Result<(), ProtoError> {
        writer.array(2)?;
        writer.unsigned(self.epoch.get());
        writer.bytes(self.state.as_bytes())
    }

    fn decode_body(decoder: &mut Decoder<'_>) -> Result<Self, ProtoError> {
        cbor::array_of(decoder, 2)?;
        Ok(Self {
            epoch: Epoch::new(decoder.u64()?),
            state: SealedSnapshot::from_bytes(decoder.bytes()?.to_vec()),
        })
    }
}

versioned_cbor!(
    Params,
    RegisterRequest,
    RegisterResponse,
    AuthRequest,
    AuthResponse,
    MembershipLeafRequest,
    MembershipLeafResponse,
    MembershipLeaves,
    BoardSyncRequest,
    BoardSyncResponse,
    MailboxMessages,
    MessageAccepted,
    GroupMessages,
    GroupState,
);
