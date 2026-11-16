-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 12: join codes have 16 digits (decided 2026-11-17, from the pre-launch review;
-- they had 12). `joined_via.join_code` is rebuilt with the wider bound, the rows kept.
CREATE TABLE joined_via_v12 (
    party_id     BLOB PRIMARY KEY REFERENCES party (party_id) ON DELETE CASCADE,
    inviter_leaf INTEGER NOT NULL CHECK (inviter_leaf >= 0),
    join_code    INTEGER NOT NULL CHECK (join_code >= 0 AND join_code < 10000000000000000)
) STRICT;
INSERT INTO joined_via_v12 (party_id, inviter_leaf, join_code)
    SELECT party_id, inviter_leaf, join_code FROM joined_via;
DROP TABLE joined_via;
ALTER TABLE joined_via_v12 RENAME TO joined_via;
