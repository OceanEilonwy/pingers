// SPDX-License-Identifier: AGPL-3.0-only
//! pingers-wake's [`WakeSnapshot`] as a client record. The snapshot holds no coordinates.

use pingers_geo::{PresenceSnapshot, PresenceState};
use pingers_presence::SpotRef;
use pingers_proto::{Epoch, Lease, MailboxId, PartyId, RecordRandomness, SpotId};
use pingers_store::Session;
use pingers_time::{Bucket, UnixSeconds};
use pingers_wake::{
    DecidedKnock, KnockStatus, KnownEpoch, LeaseSlot, LiveLease, PeerActivity, RealRecord,
    SentKnock, SpotSnapshot, WakeSnapshot,
};

use super::records::{self, Key, Reader, Writer, corrupt};
use crate::ClientError;

/// The stored snapshot, or the empty one on a fresh install.
pub(crate) fn load(session: &Session) -> Result<WakeSnapshot, ClientError> {
    Ok(records::read(session, &Key::WakeSnapshot, decode)?.unwrap_or_default())
}

/// Stores `snapshot`, replacing the previous one.
pub(crate) fn save(session: &Session, snapshot: &WakeSnapshot) -> Result<(), ClientError> {
    records::write(session, &Key::WakeSnapshot, |writer| {
        encode(writer, snapshot);
    })
}

fn encode(writer: &mut Writer, snapshot: &WakeSnapshot) {
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
    writer.len(snapshot.leases.len());
    for lease in &snapshot.leases {
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
    writer.len(snapshot.sent_knocks.len());
    for knock in &snapshot.sent_knocks {
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
    writer.len(snapshot.decided_knocks.len());
    for decided in &snapshot.decided_knocks {
        writer.fixed(decided.reply_mailbox.as_bytes());
        writer.u64(decided.at.get());
    }
    writer.option(snapshot.last_upload, |writer, at| {
        writer.u64(at.get());
    });
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

fn decode(reader: &mut Reader<'_>) -> Result<WakeSnapshot, ClientError> {
    let spots = reader.list(|reader| {
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
    })?;
    let last_in_zone = reader.option(time)?;
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
    let epochs = reader.list(|reader| {
        Ok(KnownEpoch {
            party: PartyId::from_bytes(reader.fixed()?),
            epoch: Epoch::new(reader.u64()?),
        })
    })?;
    let peer_activity = reader.list(|reader| {
        Ok(PeerActivity {
            spot: read_spot_ref(reader)?,
            at: time(reader)?,
        })
    })?;
    Ok(WakeSnapshot {
        spots,
        last_in_zone,
        leases,
        sent_knocks,
        decided_knocks,
        last_upload,
        epochs,
        peer_activity,
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
