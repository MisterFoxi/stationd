-- Cover period scans without fetching every snapshot from the table.
CREATE INDEX listener_snapshot_period ON listener_snapshot(at, mount, listeners);
