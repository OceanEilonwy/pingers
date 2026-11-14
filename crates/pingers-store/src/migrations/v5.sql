-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 5: what each registered sealed push was sealed with (design B.4.9).
--
-- seal: pingers-crypto's 16-byte fingerprint of the relay key and push token the mailbox's or
-- subscription's sealed push was made from; sealed_until: the not_after sealed into it. When
-- the token changes, the relay key rotates or not_after comes near, pingers-sync seals the push
-- again and replaces it on the server. Both NULL for a row with no sealed push, and for rows
-- written before this version (their seal is unknown, so they are sealed again).
ALTER TABLE mailbox ADD COLUMN seal BLOB CHECK (seal IS NULL OR length(seal) = 16);
ALTER TABLE mailbox ADD COLUMN sealed_until INTEGER CHECK (sealed_until IS NULL OR sealed_until >= 0);
ALTER TABLE subscription ADD COLUMN seal BLOB CHECK (seal IS NULL OR length(seal) = 16);
ALTER TABLE subscription ADD COLUMN sealed_until INTEGER CHECK (sealed_until IS NULL OR sealed_until >= 0);
