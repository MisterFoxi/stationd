-- Initial migration: only validates the migration mechanism itself (that
-- sqlx::migrate! finds this directory, applies it, and tracks its state in
-- its own internal _sqlx_migrations table). The real schema (tracks,
-- playlists, scheduling, roles...) will come in later migrations, driven by
-- actual needs — not anticipated here.
CREATE TABLE IF NOT EXISTS schema_check (
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
