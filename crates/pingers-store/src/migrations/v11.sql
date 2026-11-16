-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 11: the map host's key pins (decided 2026-11-17, from the pre-launch review).
--
-- A server's map host gateway key was taken on first use from what the server forwarded.
-- It is now pinned like the gateway's and the push relay's: from the build, a setup link or an
-- invite. `map_host_pins` holds those 32-byte hashes back to back; empty means maps are not
-- fetched through that server's relay.
ALTER TABLE server ADD COLUMN map_host_pins BLOB NOT NULL DEFAULT x''
    CHECK (length(map_host_pins) % 32 = 0);
