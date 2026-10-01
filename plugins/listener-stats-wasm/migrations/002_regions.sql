-- Preserve V1 history: its region is unknown, never inferred from a city.
-- Rebuild the primary key so identical city names in different regions do
-- not collide. The host applies this migration in one transaction.
CREATE TABLE listener_geo_v2 (
    mount TEXT NOT NULL,
    at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('found', 'not_found', 'unavailable')),
    country TEXT NOT NULL,
    region TEXT NOT NULL,
    city TEXT NOT NULL,
    listeners INTEGER NOT NULL CHECK (listeners > 0),
    PRIMARY KEY (mount, at, status, country, region, city)
);
INSERT INTO listener_geo_v2 (mount, at, status, country, region, city, listeners)
    SELECT mount, at, status, country, '', city, listeners FROM listener_geo;
DROP TABLE listener_geo;
ALTER TABLE listener_geo_v2 RENAME TO listener_geo;
CREATE INDEX listener_geo_at ON listener_geo(at);
