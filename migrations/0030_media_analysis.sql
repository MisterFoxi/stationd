-- 0030_media_analysis.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (A) — CACHE RECONSTRUCTIBLE de l'analyse audio offline (Essentia).
--
-- La VÉRITÉ est dans les tags du fichier (TBPM, TKEY, ReplayGain, TXXX:*).
-- Cette table n'est qu'un miroir indexé, rempli par le scan en LISANT les tags
-- (lofty) — jamais en relançant Essentia. Crash / VM neuve = un scan de tags
-- suffit à la reconstituer. L'analyse lourde ne tourne QUE sur les fichiers
-- sans marqueur TXXX:STATIOND_ANALYSIS à jour (cf. analyzer_version).
--
-- Identité : rel_path, cohérent avec `media` (0007). FK CASCADE : un média qui
-- sort de l'index sort de son analyse. Pas d'historique durable ici (famille A).
--
-- Contrat figé (changer la liste = ré-analyse de toute la biblio) :
--   signal (fiable, pas de ML) : bpm, key, scale, loudness_lufs, replaygain_db
--   TensorFlow (labels)        : danceability, genre_top/prob, mood/prob
--   provenance                 : analyzer_version, analyzed_at
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS media_analysis (
    rel_path         TEXT PRIMARY KEY REFERENCES media(rel_path) ON DELETE CASCADE,
    -- signal (fiable, pas de ML)
    bpm              REAL    CHECK (bpm IS NULL OR bpm > 0),
    key              TEXT,                 -- "A", "C#", …
    scale            TEXT    CHECK (scale IS NULL OR scale IN ('major', 'minor')),
    loudness_lufs    REAL,                 -- EBU R128 intégré
    replaygain_db    REAL,
    -- TensorFlow (labels sémantiques)
    danceability     REAL    CHECK (danceability IS NULL OR (danceability BETWEEN 0 AND 1)),
    genre_top        TEXT,
    genre_prob       REAL    CHECK (genre_prob IS NULL OR (genre_prob BETWEEN 0 AND 1)),
    mood             TEXT,
    mood_prob        REAL    CHECK (mood_prob IS NULL OR (mood_prob BETWEEN 0 AND 1)),
    -- provenance
    analyzer_version TEXT    NOT NULL,
    analyzed_at      INTEGER NOT NULL      -- epoch (s) du scan qui a reflété la ligne
);

-- Filtres playlists probables : tempo, tonalité, loudness, genre, mood.
CREATE INDEX IF NOT EXISTS idx_media_analysis_bpm   ON media_analysis (bpm);
CREATE INDEX IF NOT EXISTS idx_media_analysis_key   ON media_analysis (key, scale);
CREATE INDEX IF NOT EXISTS idx_media_analysis_genre ON media_analysis (genre_top);
CREATE INDEX IF NOT EXISTS idx_media_analysis_mood  ON media_analysis (mood);
