// SPDX-License-Identifier: AGPL-3.0-only
//! The closed board segments this device read, with the hashes the server gave them, kept so
//! a Meetup's first message can compare them with the other members' (design B.4.3: a server
//! that shows different boards to different devices is caught when they meet).

use pingers_proto::{BoardPage, SeenSegment};

use super::sync::Net;

impl Net {
    /// Keeps the hash of every closed segment on `page`. Best effort: a segment not kept only
    /// weakens the next comparison, and must not fail the board read.
    pub(super) async fn saw_board(&self, page: &BoardPage) {
        let seen: Vec<SeenSegment> = page
            .segments
            .iter()
            .filter_map(|segment| {
                segment.closed_hash.map(|hash| SeenSegment {
                    start: segment.start,
                    hash,
                })
            })
            .collect();
        if seen.is_empty() {
            return;
        }
        let _kept = self
            .backend
            .store(move |session, _clock, _rng| {
                for segment in &seen {
                    session.board_seen().record(segment)?;
                }
                Ok(())
            })
            .await;
    }
}
