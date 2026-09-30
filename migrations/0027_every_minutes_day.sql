-- 0027 — `every_minutes` compté depuis minuit (1..1439).
--
-- `every_minutes = N` pose ses repères N, 2N… minutes après minuit dans la
-- journée (45 → 00:45 01:30 … 23:15), et non plus :00 :N … recalés à chaque
-- heure : la borne passe de 60 à 1439. SQLite ne sait pas modifier un CHECK :
-- la table est reconstruite (même forme que 0026). Rien ne la référence.
-- Famille (A) : reconstruite à chaque `schedule apply` ; la copie garde la
-- grille en place d'ici là.

CREATE TABLE grid_at_clock_new (
    rule_id       TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref  TEXT NOT NULL,
    every_minutes INTEGER CHECK (every_minutes IS NULL OR every_minutes BETWEEN 1 AND 1439),
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
