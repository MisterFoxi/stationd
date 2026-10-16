-- 0037_media_breath.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (A) — CACHE RECONSTRUCTIBLE de la détection de respirations Suno.
--
-- La VÉRITÉ est dans le tag du fichier : une frame unique TXXX:STATIOND_BREATH
-- portant {version de schéma, version d'analyseur, régions}. Cette table n'est
-- qu'un miroir indexé, rempli par le scan en LISANT le tag (lofty) — jamais en
-- relançant la détection. Crash / VM neuve = un scan de tags suffit à la
-- reconstituer. La détection lourde ne tourne QUE sur les fichiers dont le
-- marqueur (av) est absent ou périmé.
--
-- Identité : media_uuid (0033), PAS rel_path. Une respiration est une propriété
-- du CONTENU audio, pas du chemin — elle doit survivre à un renommage, ce que
-- l'uuid garantit (trigger media_move_identity) et le rel_path non. FK sur
-- media(media_uuid) : lie le cache à l'index (famille A), comme media_analysis
-- l'est à media(rel_path) — un média hors de l'index sort de sa détection.
--
-- Propre vs jamais-vu : un fichier analysé sans défaut écrit QUAND MÊME le tag
-- (regions vide) et donc une ligne ici avec breath_count = 0. On distingue
-- « vérifié, RAS » de « jamais analysé » (ligne absente) — pas de ré-analyse
-- des fichiers sains à chaque scan.
--
-- regions_json : tableau des régions détectées, chacune
--   {s,e}   début / fin en SECONDES (résolution ms) — converties en samples au
--           moment d'écrire les cue markers du WAV, avec le sample rate réel.
--   {lo,hi} bande de fréquence du blop (Hz) : borne la region côté RX et cible
--           l'atténuation spectrale si la correction auto est un jour activée.
--   {c}     confiance 0→1 : sous le plancher, la région n'est pas retenue.
--
-- No-silent-failure : un regions_json illisible N'ENTRE PAS en ligne muette —
-- il est remonté dans le rapport de scan.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS media_breath (
    media_uuid       TEXT PRIMARY KEY REFERENCES media(media_uuid) ON DELETE CASCADE,
    breath_count     INTEGER NOT NULL CHECK (breath_count >= 0),
    max_confidence   REAL    CHECK (max_confidence IS NULL OR (max_confidence BETWEEN 0 AND 1)),
    regions_json     TEXT    NOT NULL,       -- tableau JSON [{s,e,lo,hi,c}, …]
    schema_version   INTEGER NOT NULL CHECK (schema_version >= 1),  -- "v" du tag
    analyzer_version TEXT    NOT NULL,        -- "av" du tag (marqueur run-once)
    analyzed_at      INTEGER NOT NULL         -- epoch (s) du scan qui a reflété la ligne
);

-- Filtre principal : lister les médias à travailler (breath_count > 0),
-- trier par gravité (max_confidence).
CREATE INDEX IF NOT EXISTS idx_media_breath_flag ON media_breath (breath_count);
CREATE INDEX IF NOT EXISTS idx_media_breath_conf ON media_breath (max_confidence);
