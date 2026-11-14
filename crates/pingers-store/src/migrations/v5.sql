-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 5: what the main app needs to prove within a wake and to size token refills
-- (design B.4.8).
--
-- rln_tree_state: which hourly root the local leaf copy (rln_tree_leaf) reproduces, how many
-- leaves it holds, and this device's Merkle path in it (20 siblings of 32 bytes, leaf level
-- first; NULL while the device has no leaf in that root). A proof needs only the path and the
-- root, so a wake never rebuilds the tree. At most one row.
CREATE TABLE rln_tree_state (
    singleton     INTEGER PRIMARY KEY CHECK (singleton = 1),
    root_epoch    INTEGER NOT NULL CHECK (root_epoch BETWEEN 0 AND 4294967295),
    root          BLOB NOT NULL CHECK (length(root) = 32),
    leaf_count    INTEGER NOT NULL CHECK (leaf_count BETWEEN 0 AND 4294967295),
    path_position INTEGER CHECK (path_position BETWEEN 0 AND 4294967295),
    path          BLOB CHECK (length(path) = 640),
    CHECK ((path IS NULL) = (path_position IS NULL))
) STRICT;

-- pp_allowance: the server's advertised daily token limit, and how many tokens this account
-- has been issued on the server's current UTC day as far as this device knows. A refill asks for
-- at most what is left. At most one row.
CREATE TABLE pp_allowance (
    singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
    daily_limit INTEGER NOT NULL CHECK (daily_limit BETWEEN 0 AND 65535),
    day         INTEGER NOT NULL CHECK (day >= 0),
    issued      INTEGER NOT NULL CHECK (issued BETWEEN 0 AND 65535)
) STRICT;

-- prover_self_test: the last prover self-test (design B.4.8 fallback 2) and the parameters it
-- tested; a result for other parameters is void. Verdict 0 passed, 1 too slow, 2 wrong
-- parameters, 3 proof rejected. Times in milliseconds. At most one row.
CREATE TABLE prover_self_test (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    parameters TEXT NOT NULL CHECK (length(parameters) > 0),
    verdict    INTEGER NOT NULL CHECK (verdict IN (0, 1, 2, 3)),
    first_ms   INTEGER NOT NULL CHECK (first_ms >= 0),
    median_ms  INTEGER NOT NULL CHECK (median_ms >= 0),
    p95_ms     INTEGER NOT NULL CHECK (p95_ms >= 0),
    timed      INTEGER NOT NULL CHECK (timed BETWEEN 0 AND 255),
    tested_at  INTEGER NOT NULL CHECK (tested_at >= 0)
) STRICT;
