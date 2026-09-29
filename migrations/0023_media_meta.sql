-- Rebuildable plugin metadata, independent of genres and audio files.
-- One scalar value per key and media; plugin order resolves collisions.
CREATE TABLE media_meta (
    rel_path TEXT NOT NULL REFERENCES media(rel_path) ON DELETE CASCADE,
    key TEXT NOT NULL CHECK (length(key) > 0),
    value TEXT NOT NULL CHECK (length(trim(value)) > 0),
    PRIMARY KEY (rel_path, key)
);
CREATE INDEX media_meta_key_value ON media_meta(key, value, rel_path);
