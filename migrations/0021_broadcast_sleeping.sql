-- Family (B): broadcast state `stopped` → `sleeping`.
-- The idle stop (stop-when-idle) is now `sleeping`: left by `wake` when the
-- audience comes back. The operator's stop is no longer a broadcast state: it
-- is stationd itself stopped (`stationctl station stop`, marker
-- data/stationd.stopped). A station stopped before this migration therefore
-- sleeps: it wakes at its first listener (release notes).
-- SQLite cannot alter a CHECK: rebuild the single-row table.
CREATE TABLE broadcast_state_new (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    state      TEXT    NOT NULL CHECK (state IN ('running', 'paused', 'draining', 'sleeping')),
    updated_at INTEGER NOT NULL
);
INSERT INTO broadcast_state_new (id, state, updated_at)
    SELECT id, CASE state WHEN 'stopped' THEN 'sleeping' ELSE state END, updated_at
    FROM broadcast_state;
DROP TABLE broadcast_state;
ALTER TABLE broadcast_state_new RENAME TO broadcast_state;
