-- 0015 — clé de genre repliée (comparaison insensible à la casse).
--
-- Les filtres `genre` des playlists dynamiques (has/has_any/has_all/has_none)
-- comparent désormais `genre_key` et non plus `genre` : la valeur de filtre est
-- repliée côté Rust (`media_index::genre_key` : trim + minuscules Unicode) et
-- comparée à cette colonne, remplie par le scan avec la même fonction.
-- `genre` garde la graphie d'origine (affichage).
--
-- Famille (A) : le backfill ci-dessous utilise lower(trim()) SQLite, qui ne
-- replie que l'ASCII (« Électro » reste « Électro »). Le prochain
-- `library scan` réécrit toutes les clés avec le repli Unicode exact.

ALTER TABLE media_genre ADD COLUMN genre_key TEXT NOT NULL DEFAULT '';
UPDATE media_genre SET genre_key = lower(trim(genre));
CREATE INDEX IF NOT EXISTS idx_media_genre_key ON media_genre (genre_key);
