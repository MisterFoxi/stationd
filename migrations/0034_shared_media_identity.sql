-- IDs synthesized from paths are provisional until the file tag is verified.
ALTER TABLE media_identity ADD COLUMN confirmed INTEGER NOT NULL DEFAULT 0 CHECK (confirmed IN (0, 1));
DROP TRIGGER media_uuid_immutable;
CREATE TRIGGER media_uuid_immutable BEFORE UPDATE OF media_uuid ON media
WHEN OLD.media_uuid IS NOT NULL AND NEW.media_uuid IS NOT OLD.media_uuid
 AND EXISTS (SELECT 1 FROM media_identity WHERE uuid = OLD.media_uuid AND confirmed = 1)
BEGIN
    SELECT RAISE(ABORT, 'confirmed media UUID is immutable');
END;
