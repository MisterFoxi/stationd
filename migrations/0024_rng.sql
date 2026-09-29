-- Reproducible random draws of the selection (`src/draw.rs`).
-- One station seed, created once by `db::init` (never by this migration: the
-- on-air simulation's in-memory copy runs the migrations, then copies the
-- live rows — the live seed must be the one it gets).
CREATE TABLE rng_seed (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    seed BLOB NOT NULL CHECK (length(seed) = 32)
);
-- How many draws each scope (a playlist's pick, a group's permutation or
-- weighted choice) has consumed: draw n uses stream n of the scope.
CREATE TABLE rng_draws (
    scope TEXT PRIMARY KEY,
    draws INTEGER NOT NULL CHECK (draws >= 0)
);
