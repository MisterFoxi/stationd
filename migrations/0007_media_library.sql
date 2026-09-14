-- 0007_media_library.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (A) — INDEX RECONSTRUCTIBLE de la bibliothèque média.
--
-- Dérivé du filesystem par scan (walkdir + lofty). Reconstructible : un
-- re-scan recalcule tout. L'historique de lecture (famille B) référencera un
-- média par son IDENTITÉ (rel_path + garde-fou taille/mtime), PAS par FK —
-- donc cet index peut être reconstruit sans casser l'historique durable.
--
-- Identité / garde-fou :
--   rel_path  = clé. Chemin relatif à `media/`, séparateur '/', CASSE
--               CONSERVÉE (grammaire §3.3 ; distinct des refs playlists qui,
--               elles, sont normalisées en minuscules).
--   size_bytes + mtime_ns = garde-fou d'invalidation : même chemin, contenu
--               changé → l'identité d'épisode est invalidée côté famille (B).
--
-- Réconciliation (doc archi « épisode disparu → marqué indisponible ») : le
-- scan ne SUPPRIME pas les disparus, il les passe available = 0 (on distingue
-- « jamais vu » de « vu puis disparu »). Fait en UNE transaction : tout
-- repassé à 0 en tête de scan, puis chaque fichier vu ré-affirme available = 1.
--
-- No-silent-failure : la durée est le seul champ dont le scheduler dépend
-- vraiment (« ce titre finit-il avant la borne »). Un fichier illisible ou de
-- durée nulle N'ENTRE PAS dans cette table — il est remonté dans le rapport de
-- scan (jamais avalé en ligne muette). Les champs texte (title/artist/album/
-- year/genre) peuvent être NULL/vides : source dégradée, diagnostiquée, mais
-- présente comme colonne.
--
-- duration_ms : durée en MILLISECONDES (le sting < 1 s ne s'écrase pas à 0).
-- Le filtre grammaire `duration` (en secondes) se calcule à la requête
-- (duration_ms / 1000).
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS media (
    rel_path      TEXT PRIMARY KEY,          -- relatif à media/, séparateur '/'
    title         TEXT,
    artist        TEXT,
    album         TEXT,
    year          INTEGER CHECK (year IS NULL OR year BETWEEN 1 AND 9999),
    duration_ms   INTEGER NOT NULL CHECK (duration_ms > 0),
    size_bytes    INTEGER NOT NULL CHECK (size_bytes >= 0),
    mtime_ns      INTEGER NOT NULL,          -- ns depuis l'epoch UNIX (garde-fou)
    available     INTEGER NOT NULL DEFAULT 1 CHECK (available IN (0, 1)),
    scanned_at    INTEGER NOT NULL           -- epoch (s) du scan qui a affirmé la ligne
);

-- genre = ENSEMBLE de chaînes (grammaire : has / has_any / has_all). Table de
-- détail dédiée, cohérent avec « chaque variante a sa table ». Pas de cascade
-- FK activée dans ce projet (cf. grid_index) : le remplacement du set d'un
-- média est fait explicitement par le scanner (DELETE puis INSERT).
CREATE TABLE IF NOT EXISTS media_genre (
    rel_path TEXT NOT NULL REFERENCES media(rel_path) ON DELETE CASCADE,
    genre    TEXT NOT NULL,
    PRIMARY KEY (rel_path, genre)
);

-- Le filtrage courant portera sur la disponibilité et l'artiste (anti-répétition).
CREATE INDEX IF NOT EXISTS idx_media_available ON media (available);
CREATE INDEX IF NOT EXISTS idx_media_artist    ON media (artist);
