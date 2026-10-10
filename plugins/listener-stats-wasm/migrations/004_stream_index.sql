-- Cover the selected stream used by dashboards and geographic lookups.
CREATE INDEX IF NOT EXISTS listener_snapshot_mount_period ON listener_snapshot(mount, at, listeners);
