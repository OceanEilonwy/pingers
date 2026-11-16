-- SPDX-License-Identifier: AGPL-3.0-only
-- Schema version 8: what a knock leaves behind until its Welcome arrives (design B.5, B.6.1).
--
-- knock_key: the grant secret whose public half is in the Meetup KeyPackage a knock carried,
-- and the Meetup subscription the knock carried, keyed by the knock's reply mailbox (where the
-- Welcome arrives). Joining from the Welcome takes the row: the secret moves to `meetup.gk_sk`
-- and the subscription becomes the device's own in the Meetup. Rows of knocks that were never
-- answered are deleted once no Welcome can come (a lease after the knock), so no grant secret
-- outlives the knock that needed it (style guide section 8). The Party cascade covers a Party
-- this device leaves meanwhile.
CREATE TABLE knock_key (
    reply_mailbox BLOB PRIMARY KEY CHECK (length(reply_mailbox) = 16),
    party_id      BLOB NOT NULL REFERENCES party (party_id) ON DELETE CASCADE,
    gk_sk         BLOB NOT NULL CHECK (length(gk_sk) = 32),
    subscription  BLOB CHECK (subscription IS NULL OR length(subscription) > 0),
    created_at    INTEGER NOT NULL CHECK (created_at >= 0)
) STRICT;

-- A Meetup's subscription row (its push handle) goes when the Meetup ends, like everything
-- else that names the group (design B.5, G.7).
CREATE TRIGGER meetup_end_forgets_subscription AFTER DELETE ON meetup
BEGIN
    DELETE FROM subscription WHERE group_id = OLD.group_id;
END;
