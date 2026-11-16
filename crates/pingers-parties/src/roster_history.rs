// SPDX-License-Identifier: AGPL-3.0-only
//! The roster history over pingers-store's `roster_epoch` and `roster_entry` tables: the rosters
//! knock verification may still ask about.

use pingers_mls::Roster;
use pingers_proto::{Epoch, PartyId};
use pingers_store::Session;

use crate::PartiesError;

/// How many epochs' rosters are kept: the current one and the one before it, which is all
/// knock verification can ask for (pingers-presence matches the previous epoch for one lease
/// after a Commit that was not a removal, design B.4.7).
const EPOCHS_KEPT: usize = 2;

/// The epochs whose rosters are kept, newest first.
pub(crate) fn epochs(session: &Session, party: &PartyId) -> Result<Vec<Epoch>, PartiesError> {
    Ok(session.rosters().epochs(party)?)
}

/// Records the roster at a new epoch and forgets rosters older than the previous epoch.
pub(crate) fn record_roster(
    session: &Session,
    party: &PartyId,
    epoch: Epoch,
    roster: &Roster,
) -> Result<(), PartiesError> {
    let rosters = session.rosters();
    rosters.record(party, epoch, roster)?;
    Ok(rosters.retain_newest(party, EPOCHS_KEPT)?)
}

pub(crate) fn roster_at(
    session: &Session,
    party: &PartyId,
    epoch: Epoch,
) -> Result<Option<Roster>, PartiesError> {
    Ok(session.rosters().at(party, epoch)?)
}
