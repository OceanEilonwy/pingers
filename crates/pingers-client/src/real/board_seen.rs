// SPDX-License-Identifier: AGPL-3.0-only
//! The closed board segments this device read on each server's board, with the hashes that
//! server gave them, kept so a Meetup's first message can compare them with the other members'
//! (design B.4.3: a server that shows different boards to different devices is caught when they
//! meet). A Meetup compares the segments of its Party's server only.

use pingers_proto::{BoardPage, SeenSegment, ServerId};

use super::sync::Net;

impl Net {
    /// Keeps the hash of every closed segment on `page`, read from `server`'s board. Best
    /// effort: a segment not kept only weakens the next comparison, and must not fail the board
    /// read.
    pub(super) async fn saw_board(&self, server: &ServerId, page: &BoardPage) {
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
        let server = server.clone();
        let _kept = self
            .backend
            .store(move |session, _clock, _rng| {
                let board = session.server(&server)?;
                for segment in &seen {
                    board.board_seen().record(segment)?;
                }
                Ok(())
            })
            .await;
    }
}
