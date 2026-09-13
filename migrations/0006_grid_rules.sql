-- 0006_grid_rules.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (A) — INDEX RECONSTRUCTIBLE des règles de grille.
-- Projette resolver::Rule. Une table de base + une table de détail par branche
-- du oneof `RuleKind` (même parti pris que la sélection playlist : chaque
-- variante a SA table, donc un champ d'une autre variante n'a pas de colonne
-- où exister). Droppable/reconstruisible par un futur `apply` de grille.
--
-- `playlist_ref` = ref_effective humain d'une playlist. PAS de FK vers
-- playlists : la résolution ref → média est un problème runtime aval (comme la
-- sortie du résolveur), et l'unicité de ref_effective n'est pas encore imposée
-- en base (cf. 0004). Une ref cassée est une erreur de résolution, pas une
-- contrainte de table.
--
-- L'état durable du résolveur (compteurs Every, occurrences AtClock prises)
-- vit en famille (B), migration 0005 — jamais ici.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS grid_rule (
    id         TEXT PRIMARY KEY,
    enabled    INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    kind       TEXT NOT NULL CHECK (kind IN ('base_rotation','day_part','at_clock','every')),
    -- Portée de validité bornée (override daté). Date civile station, inclusive.
    date_start TEXT,                                 -- "YYYY-MM-DD" ou NULL
    date_end   TEXT
);

-- Jours de validité. AUCUNE ligne = tous les jours (récurrence par défaut).
CREATE TABLE IF NOT EXISTS grid_rule_weekday (
    rule_id TEXT NOT NULL REFERENCES grid_rule(id) ON DELETE CASCADE,
    weekday TEXT NOT NULL CHECK (weekday IN ('mon','tue','wed','thu','fri','sat','sun')),
    PRIMARY KEY (rule_id, weekday)
);

-- ── Détail par variante ─────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS grid_base_rotation (
    rule_id      TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS grid_day_part (
    rule_id      TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref TEXT NOT NULL,
    start_hour   INTEGER NOT NULL CHECK (start_hour   BETWEEN 0 AND 23),
    start_minute INTEGER NOT NULL CHECK (start_minute BETWEEN 0 AND 59),
    end_hour     INTEGER NOT NULL CHECK (end_hour     BETWEEN 0 AND 23),
    end_minute   INTEGER NOT NULL CHECK (end_minute   BETWEEN 0 AND 59)
);

CREATE TABLE IF NOT EXISTS grid_at_clock (
    rule_id       TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref  TEXT NOT NULL,
    -- Ancrage : EXACTEMENT une forme, soit every_minutes, soit (at_hour,at_minute).
    every_minutes INTEGER CHECK (every_minutes IS NULL OR every_minutes BETWEEN 1 AND 60),
    at_hour       INTEGER CHECK (at_hour   IS NULL OR at_hour   BETWEEN 0 AND 23),
    at_minute     INTEGER CHECK (at_minute IS NULL OR at_minute BETWEEN 0 AND 59),
    mode          TEXT NOT NULL DEFAULT 'soft' CHECK (mode IN ('soft','hard')),
    expiry_secs   INTEGER CHECK (expiry_secs IS NULL OR expiry_secs > 0),
    CHECK (
        (every_minutes IS NOT NULL AND at_hour IS NULL AND at_minute IS NULL)
        OR
        (every_minutes IS NULL AND at_hour IS NOT NULL AND at_minute IS NOT NULL)
    )
);

CREATE TABLE IF NOT EXISTS grid_every (
    rule_id      TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref TEXT NOT NULL,
    -- Cadence : EXACTEMENT un ancrage (temps écoulé XOR compteur de pistes).
    elapsed_secs INTEGER CHECK (elapsed_secs IS NULL OR elapsed_secs > 0),
    tracks       INTEGER CHECK (tracks IS NULL OR tracks >= 1),
    CHECK ((elapsed_secs IS NOT NULL) <> (tracks IS NOT NULL))
);
