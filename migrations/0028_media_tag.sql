-- 0028 — d'où viennent les genres : `media_tag`.
--
-- `media_genre` est l'ensemble des genres tel que les playlists le voient :
-- les valeurs du genre du fichier (TCON) et celles que le plugin custom-tags
-- tire de ses TXXX sources (ex. `Type`), confondues. L'écran Tags a besoin de
-- les distinguer (répartition par Type, médias sans Type, renommer une valeur
-- là où elle est écrite) : cette table garde chaque valeur avec son origine.
--
--   origin = ''      → genre du fichier (TCON)
--   origin = 'Type'  → le TXXX source de ce nom (tel que déclaré dans la
--                      config custom-tags)
--
-- Famille (A) : reconstruite à chaque scan (et pour un fichier après une
-- écriture de tags). Vide jusqu'au premier scan après cette migration.
CREATE TABLE IF NOT EXISTS media_tag (
    rel_path  TEXT NOT NULL REFERENCES media(rel_path) ON DELETE CASCADE,
    origin    TEXT NOT NULL,
    value     TEXT NOT NULL CHECK (length(trim(value)) > 0),
    value_key TEXT NOT NULL,
    PRIMARY KEY (rel_path, origin, value)
);
CREATE INDEX IF NOT EXISTS media_tag_origin_key ON media_tag (origin, value_key);
