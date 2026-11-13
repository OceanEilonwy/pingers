-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 1: every client table of design C.2, including the B.6.1 consent tables.
--
-- Conventions: identifiers and keys are BLOBs of their fixed length; times are whole Unix
-- seconds; enums are small INTEGER codes listed in the CHECK beside them (the Rust side maps
-- them in one place, src/codec.rs or the repository that owns the enum). Coordinates appear only
-- in Spot and zone geometry and in meetup_peer.location_* (the latest decrypted position of a
-- granted peer, one row per peer, deleted at Meetup end): no table is keyed by time and holds a
-- position (design C.2, "No location history table exists").

CREATE TABLE identity (
    singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
    account_id  BLOB NOT NULL CHECK (length(account_id) = 16),
    sig_sk      BLOB NOT NULL CHECK (length(sig_sk) = 32),
    server_url  TEXT NOT NULL,
    server_pin  BLOB NOT NULL CHECK (length(server_pin) = 32)
) STRICT;

CREATE TABLE party (
    party_id            BLOB PRIMARY KEY CHECK (length(party_id) = 16),
    mls_group_id        BLOB NOT NULL UNIQUE CHECK (length(mls_group_id) = 32),
    name                TEXT NOT NULL,
    my_leaf             INTEGER NOT NULL CHECK (my_leaf >= 0),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    -- Per-Party override of settings.location_visibility (design B.6.1); NULL follows the
    -- device setting.
    location_visibility INTEGER CHECK (location_visibility IN (0, 1, 2))
) STRICT;

CREATE TABLE member (
    party_id     BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    leaf_index   INTEGER NOT NULL CHECK (leaf_index >= 0),
    sig_pk       BLOB NOT NULL CHECK (length(sig_pk) = 32),
    display_name TEXT NOT NULL,
    avatar       BLOB,
    verified     INTEGER NOT NULL CHECK (verified IN (0, 1)),
    PRIMARY KEY (party_id, leaf_index)
) STRICT;

CREATE TABLE spot (
    spot_id    BLOB PRIMARY KEY CHECK (length(spot_id) = 16),
    party_id   BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    geometry   BLOB NOT NULL,
    created_by BLOB NOT NULL CHECK (length(created_by) = 32),
    lamport    INTEGER NOT NULL CHECK (lamport >= 0),
    deleted    INTEGER NOT NULL CHECK (deleted IN (0, 1))
) STRICT;
CREATE INDEX spot_by_party ON spot (party_id);

CREATE TABLE exclusion_zone (
    zone_id  BLOB PRIMARY KEY CHECK (length(zone_id) = 16),
    name     TEXT NOT NULL,
    geometry BLOB NOT NULL
) STRICT;

-- OpenMLS state for Party and Meetup groups alike, as opaque keys and values chosen by
-- pingers-mls (its MlsStorage trait).
CREATE TABLE mls_kv (
    key   BLOB PRIMARY KEY,
    value BLOB NOT NULL
) STRICT;

CREATE TABLE presence (
    spot_id     BLOB PRIMARY KEY REFERENCES spot (spot_id) ON DELETE CASCADE,
    state       INTEGER NOT NULL CHECK (state IN (0, 1, 2, 3)),
    since       INTEGER NOT NULL CHECK (since >= 0),
    last_fix_at INTEGER CHECK (last_fix_at >= 0)
) STRICT;

-- At most one lease: the current one (see the crate's Decisions).
CREATE TABLE lease (
    lease_id   BLOB PRIMARY KEY CHECK (length(lease_id) = 16),
    b0         INTEGER NOT NULL CHECK (b0 >= 0),
    revoke_key BLOB NOT NULL CHECK (length(revoke_key) = 32)
) STRICT;

CREATE TABLE lease_slot (
    lease_id      BLOB NOT NULL REFERENCES lease (lease_id) ON DELETE CASCADE,
    slot          INTEGER NOT NULL CHECK (slot BETWEEN 0 AND 3),
    r             BLOB NOT NULL CHECK (length(r) = 16),
    -- NULL for a dummy slot.
    spot_id       BLOB CHECK (spot_id IS NULL OR length(spot_id) = 16),
    knock_mailbox BLOB NOT NULL CHECK (length(knock_mailbox) = 16),
    PRIMARY KEY (lease_id, slot)
) STRICT;

CREATE TABLE mailbox (
    mailbox_id BLOB PRIMARY KEY CHECK (length(mailbox_id) = 16),
    read_key   BLOB NOT NULL CHECK (length(read_key) = 32),
    purpose    INTEGER NOT NULL CHECK (purpose IN (0, 1, 2)),
    state      INTEGER NOT NULL CHECK (state IN (0, 1)),
    has_push   INTEGER NOT NULL CHECK (has_push IN (0, 1)),
    handle     BLOB CHECK (handle IS NULL OR length(handle) = 8),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0),
    CHECK ((has_push = 1) = (handle IS NOT NULL))
) STRICT;
CREATE UNIQUE INDEX mailbox_by_handle ON mailbox (handle) WHERE handle IS NOT NULL;

CREATE TABLE subscription (
    group_id   BLOB PRIMARY KEY CHECK (length(group_id) = 32),
    sub_id     BLOB NOT NULL UNIQUE CHECK (length(sub_id) = 16),
    handle     BLOB NOT NULL UNIQUE CHECK (length(handle) = 8),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0)
) STRICT;

CREATE TABLE push_relay (
    relay_id        INTEGER PRIMARY KEY CHECK (relay_id BETWEEN 0 AND 65535),
    url             TEXT NOT NULL,
    key_id          INTEGER NOT NULL CHECK (key_id BETWEEN 0 AND 255),
    public_key      BLOB NOT NULL CHECK (length(public_key) = 32),
    key_config_hash BLOB NOT NULL CHECK (length(key_config_hash) = 32),
    operator        TEXT NOT NULL,
    source          INTEGER NOT NULL CHECK (source IN (0, 1))
) STRICT;

CREATE TABLE knock_out (
    spot_id       BLOB NOT NULL CHECK (length(spot_id) = 16),
    matched_r     BLOB NOT NULL CHECK (length(matched_r) = 16),
    sent_at       INTEGER NOT NULL CHECK (sent_at >= 0),
    state         INTEGER NOT NULL CHECK (state IN (0, 1, 2)),
    reply_mailbox BLOB NOT NULL CHECK (length(reply_mailbox) = 16),
    PRIMARY KEY (spot_id, matched_r)
) STRICT;

CREATE TABLE board_seen (
    b0             INTEGER PRIMARY KEY CHECK (b0 >= 0),
    segment_sha256 BLOB NOT NULL CHECK (length(segment_sha256) = 32)
) STRICT;

CREATE TABLE meetup (
    group_id   BLOB PRIMARY KEY CHECK (length(group_id) = 32),
    party_id   BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    spot_id    BLOB NOT NULL CHECK (length(spot_id) = 16),
    started_at INTEGER NOT NULL CHECK (started_at >= 0),
    -- My per-Meetup grant secret key (design B.6.1). Kept here rather than repeated in every
    -- meetup_self_lk row; see the crate's Decisions.
    gk_sk      BLOB NOT NULL CHECK (length(gk_sk) = 32)
) STRICT;

CREATE TABLE meetup_msg (
    group_id    BLOB NOT NULL REFERENCES meetup (group_id) ON DELETE CASCADE,
    msg_id      BLOB NOT NULL CHECK (length(msg_id) = 16),
    parent      BLOB CHECK (parent IS NULL OR length(parent) = 16),
    sender_leaf INTEGER NOT NULL CHECK (sender_leaf >= 0),
    body        TEXT NOT NULL,
    ts          INTEGER NOT NULL CHECK (ts >= 0),
    PRIMARY KEY (group_id, msg_id)
) STRICT;

CREATE TABLE meetup_peer (
    group_id             BLOB NOT NULL REFERENCES meetup (group_id) ON DELETE CASCADE,
    leaf                 INTEGER NOT NULL CHECK (leaf >= 0),
    last_seen            INTEGER NOT NULL CHECK (last_seen >= 0),
    peer_lk              BLOB CHECK (peer_lk IS NULL OR length(peer_lk) = 32),
    peer_lk_gen          INTEGER CHECK (peer_lk_gen BETWEEN 0 AND 4294967295),
    peer_lk_expires_at   INTEGER CHECK (peer_lk_expires_at >= 0),
    location_lat_e7      INTEGER,
    location_lon_e7      INTEGER,
    location_accuracy_m  INTEGER CHECK (location_accuracy_m BETWEEN 0 AND 65535),
    location_heading_deg INTEGER CHECK (location_heading_deg BETWEEN 0 AND 359),
    location_speed_cms   INTEGER CHECK (location_speed_cms BETWEEN 0 AND 65535),
    location_taken_at    INTEGER CHECK (location_taken_at >= 0),
    PRIMARY KEY (group_id, leaf),
    CHECK ((peer_lk IS NULL) = (peer_lk_gen IS NULL)),
    CHECK ((peer_lk IS NULL) = (peer_lk_expires_at IS NULL)),
    -- The position is all there or all absent, and only while the peer's key is held.
    CHECK ((location_lat_e7 IS NULL) = (location_lon_e7 IS NULL)
       AND (location_lat_e7 IS NULL) = (location_accuracy_m IS NULL)
       AND (location_lat_e7 IS NULL) = (location_taken_at IS NULL)),
    CHECK (location_lat_e7 IS NULL OR peer_lk IS NOT NULL),
    CHECK (location_lat_e7 IS NOT NULL OR (location_heading_deg IS NULL AND location_speed_cms IS NULL))
) STRICT;

CREATE TABLE meetup_self_lk (
    group_id BLOB NOT NULL REFERENCES meetup (group_id) ON DELETE CASCADE,
    lk_gen   INTEGER NOT NULL CHECK (lk_gen BETWEEN 0 AND 4294967295),
    lk       BLOB NOT NULL CHECK (length(lk) = 32),
    PRIMARY KEY (group_id, lk_gen)
) STRICT;

CREATE TABLE location_grant (
    group_id     BLOB NOT NULL REFERENCES meetup (group_id) ON DELETE CASCADE,
    grantee_leaf INTEGER NOT NULL CHECK (grantee_leaf >= 0),
    lk_gen       INTEGER NOT NULL CHECK (lk_gen BETWEEN 0 AND 4294967295),
    scope        INTEGER NOT NULL CHECK (scope IN (0, 1)),
    expires_at   INTEGER NOT NULL CHECK (expires_at >= 0),
    PRIMARY KEY (group_id, grantee_leaf)
) STRICT;

CREATE TABLE location_ask (
    group_id   BLOB NOT NULL REFERENCES meetup (group_id) ON DELETE CASCADE,
    req_id     BLOB NOT NULL CHECK (length(req_id) = 16),
    direction  INTEGER NOT NULL CHECK (direction IN (0, 1)),
    peer_leaf  INTEGER NOT NULL CHECK (peer_leaf >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    state      INTEGER NOT NULL CHECK (state IN (0, 1, 2)),
    PRIMARY KEY (group_id, req_id)
) STRICT;

CREATE TABLE always_allow (
    party_id      BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    member_sig_pk BLOB NOT NULL CHECK (length(member_sig_pk) = 32),
    PRIMARY KEY (party_id, member_sig_pk)
) STRICT;

CREATE TABLE pp_token (
    token_id INTEGER PRIMARY KEY,
    token    BLOB NOT NULL,
    key_id   BLOB NOT NULL CHECK (length(key_id) = 32)
) STRICT;

CREATE TABLE rln_identity (
    singleton            INTEGER PRIMARY KEY CHECK (singleton = 1),
    sk                   BLOB NOT NULL CHECK (length(sk) = 32),
    leaf_idx             INTEGER NOT NULL CHECK (leaf_idx BETWEEN 0 AND 4294967295),
    live_from_root_epoch INTEGER NOT NULL CHECK (live_from_root_epoch >= 0)
) STRICT;

CREATE TABLE rln_counter (
    scope           INTEGER NOT NULL CHECK (scope IN (0, 1, 2)),
    bucket          INTEGER NOT NULL CHECK (bucket >= 0),
    next_message_id INTEGER NOT NULL CHECK (next_message_id BETWEEN 0 AND 8),
    PRIMARY KEY (scope, bucket)
) STRICT;

-- The membership tree copy (design C.2 rln_tree): the leaf log and cached interior nodes.
CREATE TABLE rln_tree_leaf (
    position INTEGER PRIMARY KEY CHECK (position BETWEEN 0 AND 4294967295),
    value    BLOB NOT NULL CHECK (length(value) = 32)
) STRICT;

CREATE TABLE rln_tree_node (
    level    INTEGER NOT NULL CHECK (level BETWEEN 0 AND 255),
    position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 4294967295),
    value    BLOB NOT NULL CHECK (length(value) = 32),
    PRIMARY KEY (level, position)
) STRICT;

CREATE TABLE fallback (
    kind   INTEGER PRIMARY KEY CHECK (kind IN (0, 1, 2, 3)),
    reason TEXT NOT NULL,
    since  INTEGER NOT NULL CHECK (since >= 0)
) STRICT;

CREATE TABLE settings (
    k TEXT PRIMARY KEY,
    v BLOB NOT NULL
) STRICT;
