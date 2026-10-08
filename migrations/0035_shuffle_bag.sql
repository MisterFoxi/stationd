-- Family B: independent of scans and grid activation. Removed members remain
-- tombstones until the cycle ends, preventing remove/re-add from replaying them.
CREATE TABLE shuffle_cycle (
    playlist_ref TEXT PRIMARY KEY,
    cycle INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE shuffle_member (
    playlist_ref TEXT NOT NULL,
    media_uuid TEXT NOT NULL,
    position INTEGER NOT NULL,
    used INTEGER NOT NULL DEFAULT 0 CHECK (used IN (0, 1)),
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    PRIMARY KEY (playlist_ref, media_uuid)
);
-- Reservations are linked to the existing broadcast log when the engine logs
-- its selection. Cancellation returns a member, even after a cycle rollover.
CREATE TABLE shuffle_pick (
    id INTEGER PRIMARY KEY,
    playlist_ref TEXT NOT NULL,
    media_uuid TEXT NOT NULL,
    cycle INTEGER NOT NULL,
    log_id INTEGER UNIQUE,
    settled INTEGER NOT NULL DEFAULT 0 CHECK (settled IN (0, 1, 2))
);
CREATE INDEX shuffle_pick_media ON shuffle_pick(playlist_ref, media_uuid, id) WHERE settled != 2;
CREATE INDEX broadcast_log_aired_uuid ON broadcast_log(media_uuid) WHERE aired_at IS NOT NULL;
