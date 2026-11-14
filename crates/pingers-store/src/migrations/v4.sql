-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 4: the account's standing with its server (design B.1, D).
--
-- account_registration: whether `POST /v1/accounts` has been done, and the registration code
-- that goes with it: the one to present (pending), the one the server took (registered; shared
-- in this device's invite links), or the one it refused (refused; never presented again, so a
-- 403 cannot turn into a retry loop). No row means pending without a code.
CREATE TABLE account_registration (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    state     INTEGER NOT NULL CHECK (state IN (0, 1, 2)),
    code      TEXT CHECK (code IS NULL OR length(code) > 0)
) STRICT;

-- The account's bearer from `POST /v1/auth` (an hour), so a restart within the hour does not
-- authenticate again. At most one row.
CREATE TABLE account_bearer (
    singleton  INTEGER PRIMARY KEY CHECK (singleton = 1),
    bearer     TEXT NOT NULL CHECK (length(bearer) > 0),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0)
) STRICT;
