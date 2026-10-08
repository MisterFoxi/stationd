-- Durable media identity. URI is a locator, UUID is playback identity.
CREATE TABLE media_identity (
    uuid TEXT PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-8' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)))),
    uri TEXT NOT NULL UNIQUE
);

INSERT INTO media_identity (uri)
SELECT rel_path FROM media
UNION SELECT rel_path FROM broadcast_log
UNION SELECT rel_path FROM episode_play
UNION SELECT rel_path FROM queue_entry
UNION SELECT last_rel_path FROM playlist_cursor;

ALTER TABLE media ADD COLUMN media_uuid TEXT;
UPDATE media SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = media.rel_path);
ALTER TABLE media_genre ADD COLUMN media_uuid TEXT;
UPDATE media_genre SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = media_genre.rel_path);
ALTER TABLE media_meta ADD COLUMN media_uuid TEXT;
UPDATE media_meta SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = media_meta.rel_path);
ALTER TABLE media_tag ADD COLUMN media_uuid TEXT;
UPDATE media_tag SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = media_tag.rel_path);
ALTER TABLE media_analysis ADD COLUMN media_uuid TEXT;
UPDATE media_analysis SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = media_analysis.rel_path);
ALTER TABLE broadcast_log ADD COLUMN media_uuid TEXT;
UPDATE broadcast_log SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = broadcast_log.rel_path);
ALTER TABLE episode_play ADD COLUMN media_uuid TEXT;
UPDATE episode_play SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = episode_play.rel_path);
ALTER TABLE queue_entry ADD COLUMN media_uuid TEXT;
UPDATE queue_entry SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = queue_entry.rel_path);
ALTER TABLE playlist_cursor ADD COLUMN media_uuid TEXT;
UPDATE playlist_cursor SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = playlist_cursor.last_rel_path);

CREATE UNIQUE INDEX media_uuid_unique ON media (media_uuid);
CREATE UNIQUE INDEX episode_play_uuid_unique ON episode_play (playlist_ref, media_uuid);
CREATE INDEX broadcast_log_uuid_played ON broadcast_log (media_uuid, played_at);
CREATE INDEX broadcast_log_uuid_aired ON broadcast_log (media_uuid, aired_at) WHERE aired_at IS NOT NULL;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER media_assign_uuid AFTER INSERT ON media
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE media SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER media_genre_assign_uuid AFTER INSERT ON media_genre
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE media_genre SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER media_meta_assign_uuid AFTER INSERT ON media_meta
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE media_meta SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER media_tag_assign_uuid AFTER INSERT ON media_tag
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE media_tag SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER media_analysis_assign_uuid AFTER INSERT ON media_analysis
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE media_analysis SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER broadcast_log_assign_uuid AFTER INSERT ON broadcast_log
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE broadcast_log SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER episode_play_assign_uuid AFTER INSERT ON episode_play
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE episode_play SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER queue_entry_assign_uuid AFTER INSERT ON queue_entry
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.rel_path);
    UPDATE queue_entry SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.rel_path)
    WHERE rowid = NEW.rowid;
END;

-- Compatibility for path-based imports and older callers. Normal writers bind UUIDs.
CREATE TRIGGER playlist_cursor_assign_uuid AFTER INSERT ON playlist_cursor
WHEN NEW.media_uuid IS NULL
BEGIN
    INSERT OR IGNORE INTO media_identity (uri) VALUES (NEW.last_rel_path);
    UPDATE playlist_cursor SET media_uuid = (SELECT uuid FROM media_identity WHERE uri = NEW.last_rel_path)
    WHERE rowid = NEW.rowid;
END;

CREATE TRIGGER media_move_identity AFTER UPDATE OF rel_path ON media
WHEN NEW.rel_path <> OLD.rel_path
BEGIN
    UPDATE media_identity SET uri = NEW.rel_path WHERE uuid = NEW.media_uuid;
END;

CREATE UNIQUE INDEX media_genre_uuid_unique ON media_genre (media_uuid, genre);
CREATE UNIQUE INDEX media_meta_uuid_unique ON media_meta (media_uuid, key);
CREATE UNIQUE INDEX media_tag_uuid_unique ON media_tag (media_uuid, origin, value);
CREATE UNIQUE INDEX media_analysis_uuid_unique ON media_analysis (media_uuid);

CREATE TRIGGER media_identity_uuid_immutable BEFORE UPDATE OF uuid ON media_identity
WHEN NEW.uuid <> OLD.uuid
BEGIN
    SELECT RAISE(ABORT, 'media UUID is immutable');
END;

CREATE TRIGGER media_uuid_immutable BEFORE UPDATE OF media_uuid ON media
WHEN OLD.media_uuid IS NOT NULL AND NEW.media_uuid IS NOT OLD.media_uuid
BEGIN
    SELECT RAISE(ABORT, 'media UUID is immutable');
END;
