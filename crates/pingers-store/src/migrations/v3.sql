-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 3: what pingers-sync held in memory and lost at every restart.
--
-- request_queue: anonymous requests an earlier wake could not send. `request` is the request
-- as pingers-sync encodes it, opaque here and without its anonymous credential (attached at
-- send time). `group_id` names the group a group request belongs to, only so ending a Meetup
-- or deleting a Party deletes its queued requests too (style guide section 8). Rows go when
-- sent, when `keep_until` passes, or when their group goes. AUTOINCREMENT: a request put back
-- after a failed send keeps its old position, which a newer request must never be given.
CREATE TABLE request_queue (
    position   INTEGER PRIMARY KEY AUTOINCREMENT,
    group_id   BLOB CHECK (group_id IS NULL OR length(group_id) = 32),
    keep_until INTEGER NOT NULL CHECK (keep_until >= 0),
    request    BLOB NOT NULL CHECK (length(request) > 0)
) STRICT;

CREATE INDEX request_queue_by_group ON request_queue (group_id) WHERE group_id IS NOT NULL;

-- The last board upload: the lease id the server assigned and the revoke key whose hash the
-- lease commits to, so the next upload replaces it. At most one row.
CREATE TABLE board_upload (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    lease_id   BLOB NOT NULL CHECK (length(lease_id) = 16),
    revoke_key BLOB NOT NULL CHECK (length(revoke_key) = 32)
) STRICT;

-- Log positions of group entries this device stored and has not pulled yet.
CREATE TABLE own_group_entry (
    group_id BLOB NOT NULL CHECK (length(group_id) = 32),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    PRIMARY KEY (group_id, sequence)
) STRICT, WITHOUT ROWID;

-- Triggers rather than calls in the repositories, so a Meetup deleted by its Party's cascade
-- is covered too.
CREATE TRIGGER meetup_end_forgets_sync_state AFTER DELETE ON meetup
BEGIN
    DELETE FROM request_queue WHERE group_id = OLD.group_id;
    DELETE FROM own_group_entry WHERE group_id = OLD.group_id;
END;

CREATE TRIGGER party_delete_forgets_sync_state AFTER DELETE ON party
BEGIN
    DELETE FROM request_queue WHERE group_id = OLD.mls_group_id;
    DELETE FROM own_group_entry WHERE group_id = OLD.mls_group_id;
END;
