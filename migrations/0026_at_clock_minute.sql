-- 0026 — `at_clock` : ancrage `minute` (un repère par heure, à :MM).
--
-- Trois formes d'ancrage, exactement une :
--   every_minutes seul          → repères :00, :N, :2N… (N divise l'heure)
--   at_minute seul              → un repère par heure, à :MM (top décalé)
--   at_hour + at_minute         → une heure fixe par jour
-- Le CHECK de 0006 n'admet pas la deuxième forme et SQLite ne sait pas le
-- modifier : la table est reconstruite. Rien ne la référence. Famille (A) :
-- reconstruite à chaque `schedule apply` ; la copie garde la grille en place
-- d'ici là. `every_minutes` reste 1..60 ici : les diviseurs de l'heure sont
-- imposés par la grammaire (grid_toml), l'index ne fait que refléter une
-- grille déjà acceptée.

CREATE TABLE grid_at_clock_new (
    rule_id       TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref  TEXT NOT NULL,
    every_minutes INTEGER CHECK (every_minutes IS NULL OR every_minutes BETWEEN 1 AND 60),
    at_hour       INTEGER CHECK (at_hour   IS NULL OR at_hour   BETWEEN 0 AND 23),
    at_minute     INTEGER CHECK (at_minute IS NULL OR at_minute BETWEEN 0 AND 59),
    mode          TEXT NOT NULL DEFAULT 'soft' CHECK (mode IN ('soft','hard')),
    expiry_secs   INTEGER CHECK (expiry_secs IS NULL OR expiry_secs > 0),
    CHECK (
        (every_minutes IS NOT NULL AND at_hour IS NULL AND at_minute IS NULL)
        OR
        (every_minutes IS NULL AND at_hour IS NULL AND at_minute IS NOT NULL)
        OR
        (every_minutes IS NULL AND at_hour IS NOT NULL AND at_minute IS NOT NULL)
    )
);
INSERT INTO grid_at_clock_new
    SELECT rule_id, playlist_ref, every_minutes, at_hour, at_minute, mode, expiry_secs
    FROM grid_at_clock;
DROP TABLE grid_at_clock;
ALTER TABLE grid_at_clock_new RENAME TO grid_at_clock;
