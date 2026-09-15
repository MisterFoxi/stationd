-- 0008_playlist_cursor.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — ÉTAT DE LECTURE DURABLE. Préservé au travers des `apply`
-- (contrairement aux vues reconstructibles de famille A) : un re-sync des
-- playlists ne doit pas remettre un parcours séquentiel à zéro.
--
-- Curseur de parcours : pour une playlist en ordre `sequential`/`newest`/
-- `oldest`, on mémorise le DERNIER fichier rendu. Au tour suivant, le pool
-- ordonné est recalculé et on prend l'élément juste après ce dernier (en
-- bouclant). Mémoriser le rel_path plutôt qu'un index rend le curseur robuste
-- aux changements de pool (fichier ajouté/retiré entre deux tours) : si le
-- dernier rendu a disparu, on repart du début.
--
-- Clé = playlist_ref (le rel_path normalisé de la playlist, tel que la grille
-- le référence et que la vue le stocke). Un renommage est un changement
-- d'identité, pas un simple relabel — cohérent avec le reste du projet.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS playlist_cursor (
    playlist_ref  TEXT PRIMARY KEY,    -- rel_path normalisé de la playlist
    last_rel_path TEXT NOT NULL,       -- dernier média rendu (identité, pas index)
    updated_at    INTEGER NOT NULL     -- epoch (s) de la dernière avance
);
