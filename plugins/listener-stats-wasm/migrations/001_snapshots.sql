-- Immutable migration: add another file for future schema changes.
-- NULL listeners means failed collection, not zero listeners.
CREATE TABLE listener_snapshot (
    mount TEXT NOT NULL,
    at INTEGER NOT NULL,
    listeners INTEGER CHECK (listeners >= 0),
    PRIMARY KEY (mount, at)
);
CREATE INDEX listener_snapshot_at ON listener_snapshot(at);

-- Only aggregates are stored. No IP, client ID or user agent survives.
-- Empty country/city means unknown. Status explains why.
CREATE TABLE listener_geo (
    mount TEXT NOT NULL,
    at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('found', 'not_found', 'unavailable')),
    country TEXT NOT NULL,
    city TEXT NOT NULL,
    listeners INTEGER NOT NULL CHECK (listeners > 0),
    PRIMARY KEY (mount, at, status, country, city)
);
CREATE INDEX listener_geo_at ON listener_geo(at);
