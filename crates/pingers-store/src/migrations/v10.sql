-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 10: Parties on more than one server (decided 2026-11-14).
--
-- `server` lists the servers this device has Parties on, with what is needed to reach each
-- again: its OHTTP relays (`server_relay`), its push relay, the key pins its gateway and push
-- relay must match, how those pins were recorded (`trust`: 0 built into the app, 1 from an
-- invite link, 2 confirmed by the user), and the gateway key hash recorded with the account
-- (`server_pin`, formerly `identity.server_pin`). Pins are 32-byte hashes back to back.
--
-- Every row that belongs to one server gains `server_key`, and goes when its server goes:
-- account standing and bearer, Privacy Pass tokens and the daily allowance, RLN identity,
-- counters, tree and its state, mailboxes, subscriptions, push relays, fallbacks, leases,
-- outgoing knocks, seen board segments, the board upload, queued requests, and Parties. A
-- Party's server is then kept once, in `party`: `party_config` (schema version 6) loses its
-- `server` column. Rows that go with a Party (its config, rosters, invites, joined-via) go
-- with its server through it. A join still in progress (`party_join`) names its server in the
-- link it keeps. The account key in `identity` and the prover self-test (it tests this
-- device's prover, not a server) stay device-wide.
--
-- Existing rows belong to the one server the device used so far: the server named in
-- `identity`, which becomes server 1 (the app's built-in server, whose relays and pins the app
-- fills in from its build at the next start). A database with Parties but no identity cannot
-- be produced by the app; should one exist, its rows go to an unreachable placeholder server
-- rather than being lost. Per-server rows of a database with neither are dropped: they cannot
-- exist without an account either.
--
-- Tables are rebuilt (create, copy, drop, rename) with foreign keys off, which the migration
-- runner does for every migration and checks with `foreign_key_check` before committing.

CREATE TABLE server (
    server_key      INTEGER PRIMARY KEY,
    origin          TEXT NOT NULL UNIQUE CHECK (origin LIKE 'https://_%'),
    push_relay      TEXT CHECK (push_relay IS NULL OR length(push_relay) > 0),
    gateway_pins    BLOB NOT NULL CHECK (length(gateway_pins) % 32 = 0),
    push_relay_pins BLOB NOT NULL CHECK (length(push_relay_pins) % 32 = 0),
    trust           INTEGER NOT NULL CHECK (trust IN (0, 1, 2)),
    server_pin      BLOB CHECK (server_pin IS NULL OR length(server_pin) = 32)
) STRICT;

CREATE TABLE server_relay (
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    position   INTEGER NOT NULL CHECK (position >= 0),
    url        TEXT NOT NULL CHECK (length(url) > 0),
    PRIMARY KEY (server_key, position)
) STRICT;

INSERT INTO server (server_key, origin, push_relay, gateway_pins, push_relay_pins, trust,
                    server_pin)
SELECT 1, server_url, NULL, x'', x'', 0, server_pin FROM identity;

INSERT INTO server (server_key, origin, push_relay, gateway_pins, push_relay_pins, trust,
                    server_pin)
SELECT 1, 'https://unknown-server.invalid', NULL, x'', x'', 0, NULL
WHERE NOT EXISTS (SELECT 1 FROM server) AND EXISTS (SELECT 1 FROM party);

-- The triggers of versions 3 and 9 name tables rebuilt below; they are made again at the end. The
-- temporary tables keep the suffix `_v5` they were first written with.
DROP TRIGGER meetup_end_forgets_sync_state;
DROP TRIGGER party_delete_forgets_sync_state;
DROP TRIGGER meetup_end_forgets_subscription;

CREATE TABLE identity_v5 (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    account_id BLOB NOT NULL CHECK (length(account_id) = 16),
    sig_sk     BLOB NOT NULL CHECK (length(sig_sk) = 32)
) STRICT;
INSERT INTO identity_v5 SELECT singleton, account_id, sig_sk FROM identity;
DROP TABLE identity;
ALTER TABLE identity_v5 RENAME TO identity;

CREATE TABLE party_v5 (
    party_id            BLOB PRIMARY KEY CHECK (length(party_id) = 16),
    server_key          INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    mls_group_id        BLOB NOT NULL UNIQUE CHECK (length(mls_group_id) = 32),
    name                TEXT NOT NULL,
    my_leaf             INTEGER NOT NULL CHECK (my_leaf >= 0),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    location_visibility INTEGER CHECK (location_visibility IN (0, 1, 2))
) STRICT;
INSERT INTO party_v5
SELECT party_id, 1, mls_group_id, name, my_leaf, created_at, location_visibility FROM party;
DROP TABLE party;
ALTER TABLE party_v5 RENAME TO party;
CREATE INDEX party_by_server ON party (server_key);

CREATE TABLE account_registration_v5 (
    server_key INTEGER PRIMARY KEY REFERENCES server (server_key) ON DELETE CASCADE,
    state      INTEGER NOT NULL CHECK (state IN (0, 1, 2)),
    code       TEXT CHECK (code IS NULL OR length(code) > 0)
) STRICT;
INSERT INTO account_registration_v5
SELECT 1, state, code FROM account_registration WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE account_registration;
ALTER TABLE account_registration_v5 RENAME TO account_registration;

CREATE TABLE account_bearer_v5 (
    server_key INTEGER PRIMARY KEY REFERENCES server (server_key) ON DELETE CASCADE,
    bearer     TEXT NOT NULL CHECK (length(bearer) > 0),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0)
) STRICT;
INSERT INTO account_bearer_v5
SELECT 1, bearer, expires_at FROM account_bearer WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE account_bearer;
ALTER TABLE account_bearer_v5 RENAME TO account_bearer;

CREATE TABLE pp_token_v5 (
    token_id   INTEGER PRIMARY KEY,
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    token      BLOB NOT NULL,
    key_id     BLOB NOT NULL CHECK (length(key_id) = 32)
) STRICT;
INSERT INTO pp_token_v5
SELECT token_id, 1, token, key_id FROM pp_token WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE pp_token;
ALTER TABLE pp_token_v5 RENAME TO pp_token;
CREATE INDEX pp_token_by_server ON pp_token (server_key);

CREATE TABLE rln_identity_v5 (
    server_key           INTEGER PRIMARY KEY REFERENCES server (server_key) ON DELETE CASCADE,
    sk                   BLOB NOT NULL CHECK (length(sk) = 32),
    leaf_idx             INTEGER NOT NULL CHECK (leaf_idx BETWEEN 0 AND 4294967295),
    live_from_root_epoch INTEGER NOT NULL CHECK (live_from_root_epoch >= 0)
) STRICT;
INSERT INTO rln_identity_v5
SELECT 1, sk, leaf_idx, live_from_root_epoch FROM rln_identity
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE rln_identity;
ALTER TABLE rln_identity_v5 RENAME TO rln_identity;

CREATE TABLE rln_counter_v5 (
    server_key      INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    scope           INTEGER NOT NULL CHECK (scope IN (0, 1, 2)),
    bucket          INTEGER NOT NULL CHECK (bucket >= 0),
    next_message_id INTEGER NOT NULL CHECK (next_message_id BETWEEN 0 AND 8),
    PRIMARY KEY (server_key, scope, bucket)
) STRICT;
INSERT INTO rln_counter_v5
SELECT 1, scope, bucket, next_message_id FROM rln_counter WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE rln_counter;
ALTER TABLE rln_counter_v5 RENAME TO rln_counter;

CREATE TABLE rln_tree_leaf_v5 (
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    position   INTEGER NOT NULL CHECK (position BETWEEN 0 AND 4294967295),
    value      BLOB NOT NULL CHECK (length(value) = 32),
    PRIMARY KEY (server_key, position)
) STRICT;
INSERT INTO rln_tree_leaf_v5
SELECT 1, position, value FROM rln_tree_leaf WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE rln_tree_leaf;
ALTER TABLE rln_tree_leaf_v5 RENAME TO rln_tree_leaf;

CREATE TABLE rln_tree_node_v5 (
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    level      INTEGER NOT NULL CHECK (level BETWEEN 0 AND 255),
    position   INTEGER NOT NULL CHECK (position BETWEEN 0 AND 4294967295),
    value      BLOB NOT NULL CHECK (length(value) = 32),
    PRIMARY KEY (server_key, level, position)
) STRICT;
INSERT INTO rln_tree_node_v5
SELECT 1, level, position, value FROM rln_tree_node WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE rln_tree_node;
ALTER TABLE rln_tree_node_v5 RENAME TO rln_tree_node;

CREATE TABLE mailbox_v5 (
    mailbox_id BLOB PRIMARY KEY CHECK (length(mailbox_id) = 16),
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    read_key   BLOB NOT NULL CHECK (length(read_key) = 32),
    purpose    INTEGER NOT NULL CHECK (purpose IN (0, 1, 2)),
    state      INTEGER NOT NULL CHECK (state IN (0, 1)),
    has_push   INTEGER NOT NULL CHECK (has_push IN (0, 1)),
    handle     BLOB CHECK (handle IS NULL OR length(handle) = 8),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0),
    seal         BLOB CHECK (seal IS NULL OR length(seal) = 16),
    sealed_until INTEGER CHECK (sealed_until IS NULL OR sealed_until >= 0),
    CHECK ((has_push = 1) = (handle IS NOT NULL))
) STRICT;
INSERT INTO mailbox_v5
SELECT mailbox_id, 1, read_key, purpose, state, has_push, handle, expires_at, seal, sealed_until
FROM mailbox
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE mailbox;
ALTER TABLE mailbox_v5 RENAME TO mailbox;
CREATE UNIQUE INDEX mailbox_by_handle ON mailbox (handle) WHERE handle IS NOT NULL;
CREATE INDEX mailbox_by_server ON mailbox (server_key, purpose, state);

CREATE TABLE subscription_v5 (
    group_id   BLOB PRIMARY KEY CHECK (length(group_id) = 32),
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    sub_id     BLOB NOT NULL UNIQUE CHECK (length(sub_id) = 16),
    handle     BLOB NOT NULL UNIQUE CHECK (length(handle) = 8),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0),
    seal         BLOB CHECK (seal IS NULL OR length(seal) = 16),
    sealed_until INTEGER CHECK (sealed_until IS NULL OR sealed_until >= 0)
) STRICT;
INSERT INTO subscription_v5
SELECT group_id, 1, sub_id, handle, expires_at, seal, sealed_until FROM subscription
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE subscription;
ALTER TABLE subscription_v5 RENAME TO subscription;

CREATE TABLE push_relay_v5 (
    server_key      INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    relay_id        INTEGER NOT NULL CHECK (relay_id BETWEEN 0 AND 65535),
    url             TEXT NOT NULL,
    key_id          INTEGER NOT NULL CHECK (key_id BETWEEN 0 AND 255),
    public_key      BLOB NOT NULL CHECK (length(public_key) = 32),
    key_config_hash BLOB NOT NULL CHECK (length(key_config_hash) = 32),
    operator        TEXT NOT NULL,
    source          INTEGER NOT NULL CHECK (source IN (0, 1)),
    PRIMARY KEY (server_key, relay_id)
) STRICT;
INSERT INTO push_relay_v5
SELECT 1, relay_id, url, key_id, public_key, key_config_hash, operator, source FROM push_relay
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE push_relay;
ALTER TABLE push_relay_v5 RENAME TO push_relay;

CREATE TABLE fallback_v5 (
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    kind       INTEGER NOT NULL CHECK (kind IN (0, 1, 2, 3)),
    reason     TEXT NOT NULL,
    since      INTEGER NOT NULL CHECK (since >= 0),
    PRIMARY KEY (server_key, kind)
) STRICT;
INSERT INTO fallback_v5
SELECT 1, kind, reason, since FROM fallback WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE fallback;
ALTER TABLE fallback_v5 RENAME TO fallback;

-- At most one lease per server: each server's board holds this device's current lease.
-- lease_slot keeps referring to `lease` by name, so it needs no rebuild.
CREATE TABLE lease_v5 (
    lease_id   BLOB PRIMARY KEY CHECK (length(lease_id) = 16),
    server_key INTEGER NOT NULL UNIQUE REFERENCES server (server_key) ON DELETE CASCADE,
    b0         INTEGER NOT NULL CHECK (b0 >= 0),
    revoke_key BLOB NOT NULL CHECK (length(revoke_key) = 32)
) STRICT;
INSERT INTO lease_v5 SELECT lease_id, 1, b0, revoke_key FROM lease
WHERE EXISTS (SELECT 1 FROM server);
DELETE FROM lease_slot WHERE lease_id NOT IN (SELECT lease_id FROM lease_v5);
DROP TABLE lease;
ALTER TABLE lease_v5 RENAME TO lease;

CREATE TABLE knock_out_v5 (
    server_key    INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    spot_id       BLOB NOT NULL CHECK (length(spot_id) = 16),
    matched_r     BLOB NOT NULL CHECK (length(matched_r) = 16),
    sent_at       INTEGER NOT NULL CHECK (sent_at >= 0),
    state         INTEGER NOT NULL CHECK (state IN (0, 1, 2)),
    reply_mailbox BLOB NOT NULL CHECK (length(reply_mailbox) = 16),
    PRIMARY KEY (spot_id, matched_r)
) STRICT;
INSERT INTO knock_out_v5
SELECT 1, spot_id, matched_r, sent_at, state, reply_mailbox FROM knock_out
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE knock_out;
ALTER TABLE knock_out_v5 RENAME TO knock_out;

CREATE TABLE board_seen_v5 (
    server_key     INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    b0             INTEGER NOT NULL CHECK (b0 >= 0),
    segment_sha256 BLOB NOT NULL CHECK (length(segment_sha256) = 32),
    PRIMARY KEY (server_key, b0)
) STRICT;
INSERT INTO board_seen_v5
SELECT 1, b0, segment_sha256 FROM board_seen WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE board_seen;
ALTER TABLE board_seen_v5 RENAME TO board_seen;

CREATE TABLE board_upload_v5 (
    server_key INTEGER PRIMARY KEY REFERENCES server (server_key) ON DELETE CASCADE,
    lease_id   BLOB NOT NULL CHECK (length(lease_id) = 16),
    revoke_key BLOB NOT NULL CHECK (length(revoke_key) = 32)
) STRICT;
INSERT INTO board_upload_v5
SELECT 1, lease_id, revoke_key FROM board_upload WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE board_upload;
ALTER TABLE board_upload_v5 RENAME TO board_upload;

CREATE TABLE request_queue_v5 (
    position   INTEGER PRIMARY KEY AUTOINCREMENT,
    server_key INTEGER NOT NULL REFERENCES server (server_key) ON DELETE CASCADE,
    group_id   BLOB CHECK (group_id IS NULL OR length(group_id) = 32),
    keep_until INTEGER NOT NULL CHECK (keep_until >= 0),
    request    BLOB NOT NULL CHECK (length(request) > 0)
) STRICT;
INSERT INTO request_queue_v5
SELECT position, 1, group_id, keep_until, request FROM request_queue
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE request_queue;
ALTER TABLE request_queue_v5 RENAME TO request_queue;
CREATE INDEX request_queue_by_group ON request_queue (group_id) WHERE group_id IS NOT NULL;
CREATE INDEX request_queue_by_server ON request_queue (server_key, position);

CREATE TABLE rln_tree_state_v5 (
    server_key    INTEGER PRIMARY KEY REFERENCES server (server_key) ON DELETE CASCADE,
    root_epoch    INTEGER NOT NULL CHECK (root_epoch BETWEEN 0 AND 4294967295),
    root          BLOB NOT NULL CHECK (length(root) = 32),
    leaf_count    INTEGER NOT NULL CHECK (leaf_count BETWEEN 0 AND 4294967295),
    path_position INTEGER CHECK (path_position BETWEEN 0 AND 4294967295),
    path          BLOB CHECK (length(path) = 640),
    CHECK ((path IS NULL) = (path_position IS NULL))
) STRICT;
INSERT INTO rln_tree_state_v5
SELECT 1, root_epoch, root, leaf_count, path_position, path FROM rln_tree_state
WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE rln_tree_state;
ALTER TABLE rln_tree_state_v5 RENAME TO rln_tree_state;

CREATE TABLE pp_allowance_v5 (
    server_key  INTEGER PRIMARY KEY REFERENCES server (server_key) ON DELETE CASCADE,
    daily_limit INTEGER NOT NULL CHECK (daily_limit BETWEEN 0 AND 65535),
    day         INTEGER NOT NULL CHECK (day >= 0),
    issued      INTEGER NOT NULL CHECK (issued BETWEEN 0 AND 65535)
) STRICT;
INSERT INTO pp_allowance_v5
SELECT 1, daily_limit, day, issued FROM pp_allowance WHERE EXISTS (SELECT 1 FROM server);
DROP TABLE pp_allowance;
ALTER TABLE pp_allowance_v5 RENAME TO pp_allowance;

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

CREATE TRIGGER meetup_end_forgets_subscription AFTER DELETE ON meetup
BEGIN
    DELETE FROM subscription WHERE group_id = OLD.group_id;
END;

-- The Party's server is its `party` row's.
ALTER TABLE party_config DROP COLUMN server;

-- A join in progress keeps the link it came from, in the layout from before links carried the
-- server's key pins (decided 2026-11-14). Its server cannot be pinned from it, and pinning on
-- first use is what pins exist to avoid: the joiner opens a new link.
DELETE FROM party_join;
