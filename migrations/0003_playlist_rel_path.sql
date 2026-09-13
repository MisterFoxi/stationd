-- Store each playlist's canonical relative path (normalized: `/`-separated,
-- no extension, lowercased) alongside its UUID. This is the *human handle*
-- used by group `ref`s, while the UUID stays the internal key. The sync pass
-- fills it in from each file's location under the playlist root.
--
-- UNIQUE: two files normalizing to the same key (e.g. differing only by case
-- in the same folder) is a conflict — surfaced loudly rather than silently
-- overwriting one another.
ALTER TABLE playlists ADD COLUMN rel_path TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_playlists_rel_path ON playlists (rel_path);
