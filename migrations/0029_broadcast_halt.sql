-- 0029 — Family (B): the station's halts (`paused` / `sleeping`), as
-- intervals. The anti-repetition windows (`no_same_*_within`) count AIR time,
-- not wall time: a halt freezes them (`air_time`). Without this, a station
-- that sleeps overnight wakes with its 24 h windows mostly spent and replays
-- yesterday's songs to the first listener.
--
-- One row per halt: `end_at` NULL = still halted (at most one such row).
-- Written by the broadcast-state writer (`station_control`), in order, in
-- the same transaction as the state itself. Append-only apart from closing
-- the open row.
CREATE TABLE broadcast_halt (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    start_at INTEGER NOT NULL,
    end_at   INTEGER CHECK (end_at IS NULL OR end_at >= start_at)
);
CREATE UNIQUE INDEX broadcast_halt_one_open ON broadcast_halt ((end_at IS NULL)) WHERE end_at IS NULL;
CREATE INDEX broadcast_halt_start ON broadcast_halt (start_at);

-- A station halted when this migration runs: its halt starts at its last
-- state change.
INSERT INTO broadcast_halt (start_at)
    SELECT updated_at FROM broadcast_state WHERE state IN ('paused', 'sleeping');
