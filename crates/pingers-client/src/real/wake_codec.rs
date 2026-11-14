// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-wake's [`WakeSnapshot`] as a client record. The snapshot holds no coordinates.
//!
//! Version 2 starts with [`MARKER`] and the version byte, then keeps one board per server.
//! The first layout had no version: it started with the Spot count (never `u32::MAX`) and held
//! one board's leases, knocks and last upload, which [`load`] moves into a board for the
//! server the client was using.

use pingers_geo::{PresenceSnapshot, PresenceState};
use pingers_presence::SpotRef;
use pingers_proto::{Epoch, Lease, MailboxId, PartyId, RecordRandomness, ServerId, SpotId};
use pingers_store::Session;
use pingers_time::{Bucket, UnixSeconds};
use pingers_wake::{
    BoardSnapshot, DecidedKnock, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, PeerActivity,
    RealRecord, SentKnock, SpotSnapshot, WakeSnapshot,
};

use super::records::{self, Key, Reader, Writer, corrupt};
use crate::ClientError;

/// Where the first layout had its Spot count: a value no Spot count reaches.
const MARKER: u32 = u32::MAX;

/// The layout [`save`] writes.
const VERSION: u8 = 2;

/// The stored snapshot, or the empty one on a fresh install. A snapshot in the first layout
/// has its board data put under `legacy_server` (the server of the client's instance); with no
/// instance it is dropped, and the Spots, epochs and peer activity are kept.
pub(crate) fn load(
    session: &Session,
    legacy_server: Option<&ServerId>,
) -> Result<WakeSnapshot, ClientError> {
    Ok(records::read(session, &Key::WakeSnapshot, |reader| {
        decode(reader, legacy_server)
    })?
    .unwrap_or_default())
}

/// Stores `snapshot`, replacing the previous one.
pub(crate) fn save(session: &Session, snapshot: &WakeSnapshot) -> Result<(), ClientError> {
    records::write(session, &Key::WakeSnapshot, |writer| {
        encode(writer, snapshot);
    })
}

fn encode(writer: &mut Writer, snapshot: &WakeSnapshot) {
    writer.u32(MARKER);
    writer.byte(VERSION);
    writer.len(snapshot.spots.len());
    for spot in &snapshot.spots {
        spot_ref(writer, spot.spot);
        writer.byte(match spot.presence.state {
            PresenceState::Outside => 0,
            PresenceState::Candidate => 1,
            PresenceState::Inside => 2,
            PresenceState::Suppressed => 3,
        });
        writer.u64(spot.presence.since.get());
        writer.option(spot.presence.last_fix_at, |writer, at| {
            writer.u64(at.get());
        });
        writer.option(spot.presence.confirmed_at, |writer, at| {
            writer.u64(at.get());
        });
    }
    writer.option(snapshot.last_in_zone, |writer, at| {
        writer.u64(at.get());
    });
    writer.len(snapshot.boards.len());
    for board in &snapshot.boards {
        writer.text(board.server.as_str());
        encode_board_memory(writer, board);
    }
    writer.len(snapshot.epochs.len());
    for known in &snapshot.epochs {
        writer.fixed(known.party.as_bytes());
        writer.u64(known.epoch.get());
    }
    writer.len(snapshot.peer_activity.len());
    for activity in &snapshot.peer_activity {
        spot_ref(writer, activity.spot);
        writer.u64(activity.at.get());
    }
}

/// A board's leases, knocks, decisions and last upload, in the order the first layout kept
/// them.
fn encode_board_memory(writer: &mut Writer, board: &BoardSnapshot) {
    writer.len(board.leases.len());
    for lease in &board.leases {
        writer.u64(lease.start.index());
        for slot in &lease.slots {
            writer.fixed(slot.randomness.as_bytes());
            writer.option(slot.real, |writer, real| {
                spot_ref(writer, real.spot);
                writer.u64(real.epoch.get());
                writer.fixed(real.knock_mailbox.as_bytes());
            });
        }
    }
    writer.len(board.sent_knocks.len());
    for knock in &board.sent_knocks {
        spot_ref(writer, knock.spot);
        writer.u64(knock.epoch.get());
        writer.fixed(knock.knock_mailbox.as_bytes());
        writer.fixed(knock.matched.as_bytes());
        writer.u64(knock.matched_start.index());
        writer.fixed(knock.reply_mailbox.as_bytes());
        writer.u64(knock.sent_at.get());
        writer.byte(match knock.status {
            KnockStatus::Waiting => 0,
            KnockStatus::Answered => 1,
        });
    }
    writer.len(board.decided_knocks.len());
    for decided in &board.decided_knocks {
        writer.fixed(decided.reply_mailbox.as_bytes());
        writer.u64(decided.at.get());
    }
    writer.option(board.last_upload, |writer, at| {
        writer.u64(at.get());
    });
}

fn decode(
    reader: &mut Reader<'_>,
    legacy_server: Option<&ServerId>,
) -> Result<WakeSnapshot, ClientError> {
    let first = reader.u32()?;
    if first != MARKER {
        let spot_count = usize::try_from(first).map_err(|_too_long| corrupt())?;
        return decode_first_layout(reader, spot_count, legacy_server);
    }
    if reader.byte()? != VERSION {
        return Err(corrupt());
    }
    let spots = reader.list(decode_spot)?;
    let last_in_zone = reader.option(time)?;
    let boards = reader.list(|reader| {
        let server = ServerId::new(reader.text()?).map_err(|_bad| corrupt())?;
        Ok(decode_board_memory(reader)?.on(server))
    })?;
    let epochs = reader.list(decode_epoch)?;
    let peer_activity = reader.list(decode_activity)?;
    Ok(WakeSnapshot {
        spots,
        last_in_zone,
        boards,
        epochs,
        peer_activity,
    })
}

/// The first layout, its Spot count already read.
fn decode_first_layout(
    reader: &mut Reader<'_>,
    spot_count: usize,
    legacy_server: Option<&ServerId>,
) -> Result<WakeSnapshot, ClientError> {
    let mut spots = Vec::new();
    for _ in 0..spot_count {
        spots.push(decode_spot(reader)?);
    }
    let last_in_zone = reader.option(time)?;
    // The board data is read in full either way, so the epochs after it decode.
    let board = decode_board_memory(reader)?;
    let epochs = reader.list(decode_epoch)?;
    let peer_activity = reader.list(decode_activity)?;
    let boards = legacy_server
        .map(|server| board.on(server.clone()))
        .into_iter()
        .collect();
    Ok(WakeSnapshot {
        spots,
        last_in_zone,
        boards,
        epochs,
        peer_activity,
    })
}

fn decode_spot(reader: &mut Reader<'_>) -> Result<SpotSnapshot, ClientError> {
    let spot = read_spot_ref(reader)?;
    let state = match reader.byte()? {
        0 => PresenceState::Outside,
        1 => PresenceState::Candidate,
        2 => PresenceState::Inside,
        3 => PresenceState::Suppressed,
        _ => return Err(corrupt()),
    };
    Ok(SpotSnapshot {
        spot,
        presence: PresenceSnapshot {
            state,
            since: time(reader)?,
            last_fix_at: reader.option(time)?,
            confirmed_at: reader.option(time)?,
        },
    })
}

/// A board's stored memory before the server it belongs to is known.
struct BoardMemory {
    leases: Vec<LiveLease>,
    sent_knocks: Vec<SentKnock>,
    decided_knocks: Vec<DecidedKnock>,
    last_upload: Option<UnixSeconds>,
}

impl BoardMemory {
    fn on(self, server: ServerId) -> BoardSnapshot {
        BoardSnapshot {
            server,
            leases: self.leases,
            sent_knocks: self.sent_knocks,
            decided_knocks: self.decided_knocks,
            last_upload: self.last_upload,
        }
    }
}

fn decode_board_memory(reader: &mut Reader<'_>) -> Result<BoardMemory, ClientError> {
    let leases = reader.list(|reader| {
        let start = Bucket::from_index(reader.u64()?).map_err(|_bad| corrupt())?;
        let mut lease_slots = [LeaseSlot {
            randomness: RecordRandomness::from_bytes([0; RecordRandomness::LENGTH]),
            real: None,
        }; Lease::SLOTS];
        for slot in &mut lease_slots {
            *slot = LeaseSlot {
                randomness: RecordRandomness::from_bytes(reader.fixed()?),
                real: reader.option(|reader| {
                    Ok(RealRecord {
                        spot: read_spot_ref(reader)?,
                        epoch: Epoch::new(reader.u64()?),
                        knock_mailbox: MailboxId::from_bytes(reader.fixed()?),
                    })
                })?,
            };
        }
        Ok(LiveLease {
            start,
            slots: lease_slots,
        })
    })?;
    let sent_knocks = reader.list(|reader| {
        Ok(SentKnock {
            spot: read_spot_ref(reader)?,
            epoch: Epoch::new(reader.u64()?),
            knock_mailbox: MailboxId::from_bytes(reader.fixed()?),
            matched: RecordRandomness::from_bytes(reader.fixed()?),
            matched_start: Bucket::from_index(reader.u64()?).map_err(|_bad| corrupt())?,
            reply_mailbox: MailboxId::from_bytes(reader.fixed()?),
            sent_at: time(reader)?,
            status: match reader.byte()? {
                0 => KnockStatus::Waiting,
                1 => KnockStatus::Answered,
                _ => return Err(corrupt()),
            },
        })
    })?;
    let decided_knocks = reader.list(|reader| {
        Ok(DecidedKnock {
            reply_mailbox: MailboxId::from_bytes(reader.fixed()?),
            at: time(reader)?,
        })
    })?;
    let last_upload = reader.option(time)?;
    Ok(BoardMemory {
        leases,
        sent_knocks,
        decided_knocks,
        last_upload,
    })
}

fn decode_epoch(reader: &mut Reader<'_>) -> Result<KnownEpoch, ClientError> {
    Ok(KnownEpoch {
        party: PartyId::from_bytes(reader.fixed()?),
        epoch: Epoch::new(reader.u64()?),
    })
}

fn decode_activity(reader: &mut Reader<'_>) -> Result<PeerActivity, ClientError> {
    Ok(PeerActivity {
        spot: read_spot_ref(reader)?,
        at: time(reader)?,
    })
}

fn spot_ref(writer: &mut Writer, spot: SpotRef) {
    writer.fixed(spot.party.as_bytes());
    writer.fixed(spot.spot.as_bytes());
}

fn read_spot_ref(reader: &mut Reader<'_>) -> Result<SpotRef, ClientError> {
    Ok(SpotRef {
        party: PartyId::from_bytes(reader.fixed()?),
        spot: SpotId::from_bytes(reader.fixed()?),
    })
}

fn time(reader: &mut Reader<'_>) -> Result<UnixSeconds, ClientError> {
    Ok(UnixSeconds::new(reader.u64()?))
}
