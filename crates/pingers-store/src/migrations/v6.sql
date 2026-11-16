-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 6: pingers-parties' per-Party state, which it kept as key-value rows under
-- `pingers-parties/v1/` until now (pingers-parties request 2). Every row goes with its Party.

-- The Party's server, suite (0 classical, 1 X-Wing) and whether it uses its own relays
-- (custom_relays = 1: the ordered list in party_relay) or the server's.
CREATE TABLE party_config (
    party_id      BLOB PRIMARY KEY REFERENCES party (party_id) ON DELETE CASCADE,
    server        TEXT NOT NULL CHECK (length(server) > 0),
    suite         INTEGER NOT NULL CHECK (suite IN (0, 1)),
    custom_relays INTEGER NOT NULL CHECK (custom_relays IN (0, 1))
) STRICT;

CREATE TABLE party_relay (
    party_id BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    url      TEXT NOT NULL CHECK (length(url) > 0),
    PRIMARY KEY (party_id, position)
) STRICT;

-- Rosters of the epochs knock verification may still ask about (pingers-parties keeps the
-- current and the previous one, design B.4.7).
CREATE TABLE roster_epoch (
    party_id BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    epoch    INTEGER NOT NULL CHECK (epoch >= 0),
    PRIMARY KEY (party_id, epoch)
) STRICT;

CREATE TABLE roster_entry (
    party_id   BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    epoch      INTEGER NOT NULL,
    leaf_index INTEGER NOT NULL CHECK (leaf_index >= 0),
    sig_pk     BLOB NOT NULL CHECK (length(sig_pk) = 32),
    PRIMARY KEY (party_id, epoch, leaf_index),
    FOREIGN KEY (party_id, epoch) REFERENCES roster_epoch (party_id, epoch) ON DELETE CASCADE
) STRICT;

-- Members' sealed-push subscription ids in the Party group, so a remove commit can list the
-- removed member's in subscriptions_removed. Ids only: no push token, nothing tied to an account
-- beyond the roster key this device already holds.
CREATE TABLE member_subscription (
    party_id BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    sig_pk   BLOB NOT NULL CHECK (length(sig_pk) = 32),
    sub_id   BLOB NOT NULL CHECK (length(sub_id) = 16),
    PRIMARY KEY (party_id, sig_pk)
) STRICT;

-- This device's commit waiting for the server's answer; `body` is pingers-parties' encoding of
-- what the commit does.
CREATE TABLE pending_commit (
    party_id BLOB PRIMARY KEY REFERENCES party (party_id) ON DELETE CASCADE,
    body     BLOB NOT NULL
) STRICT;

-- A row while this device's changed profile waits for a commit to settle.
CREATE TABLE profile_waiting (
    party_id BLOB PRIMARY KEY REFERENCES party (party_id) ON DELETE CASCADE
) STRICT;
