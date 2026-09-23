-- Family (B): the station's broadcast control state (A2 host surface, and the
-- first-class `stationctl station stop|pause|resume|stop-when-idle`).
-- Single row. Survives a restart: an operator's stop stays a stop, an armed
-- stop-when-idle stays armed. Absent row = `running` (fresh install).
CREATE TABLE broadcast_state (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    state      TEXT    NOT NULL CHECK (state IN ('running', 'paused', 'stopped', 'draining')),
    updated_at INTEGER NOT NULL
);
