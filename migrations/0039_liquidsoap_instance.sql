-- Reservation wall time, independent from the controllable station clock.
-- Legacy reservations predate the first startup handshake (reserved_at = 0).
ALTER TABLE shuffle_pick ADD COLUMN reserved_at INTEGER NOT NULL DEFAULT 0;
CREATE TABLE liquidsoap_instance (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    instance TEXT NOT NULL
);
