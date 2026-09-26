-- 0017 — famille de règle `live` (créneau DJ).
--
-- Une règle `live` ouvre la fenêtre de connexion d'un DJ (harbor) à `start`,
-- jusqu'au prochain début d'une autre tranche (`day_part` ou `live`) ; elle
-- ne sélectionne aucune playlist (cf. resolver::live_window). Détail : le DJ
-- (id du fichier des DJ, `[live] djs_path`) et l'heure de début.
--
-- `grid_rule.kind` porte un CHECK sur les familles : SQLite ne sait pas le
-- modifier, la table est reconstruite. Les tables de détail la référencent
-- (ON DELETE CASCADE) : avec les clés étrangères actives, supprimer
-- `grid_rule` viderait leurs lignes — elles sont donc mises de côté puis
-- remises. Famille (A) : reconstruite à chaque `schedule apply` de toute
-- façon ; la copie ne sert qu'à garder la grille en place d'ici là.

CREATE TABLE grid_rule_new (
    id         TEXT PRIMARY KEY,
    enabled    INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0,1)),
    kind       TEXT NOT NULL CHECK (kind IN ('base_rotation','day_part','at_clock','every','live')),
    date_start TEXT,
    date_end   TEXT
);
INSERT INTO grid_rule_new SELECT id, enabled, kind, date_start, date_end FROM grid_rule;

CREATE TEMP TABLE keep_weekday       AS SELECT * FROM grid_rule_weekday;
CREATE TEMP TABLE keep_base_rotation AS SELECT * FROM grid_base_rotation;
CREATE TEMP TABLE keep_day_part      AS SELECT * FROM grid_day_part;
CREATE TEMP TABLE keep_at_clock      AS SELECT * FROM grid_at_clock;
CREATE TEMP TABLE keep_every         AS SELECT * FROM grid_every;

DROP TABLE grid_rule;
ALTER TABLE grid_rule_new RENAME TO grid_rule;

DELETE FROM grid_rule_weekday;
DELETE FROM grid_base_rotation;
DELETE FROM grid_day_part;
DELETE FROM grid_at_clock;
DELETE FROM grid_every;
INSERT INTO grid_rule_weekday  SELECT * FROM keep_weekday;
INSERT INTO grid_base_rotation SELECT * FROM keep_base_rotation;
INSERT INTO grid_day_part      SELECT * FROM keep_day_part;
INSERT INTO grid_at_clock      SELECT * FROM keep_at_clock;
INSERT INTO grid_every         SELECT * FROM keep_every;
DROP TABLE keep_weekday;
DROP TABLE keep_base_rotation;
DROP TABLE keep_day_part;
DROP TABLE keep_at_clock;
DROP TABLE keep_every;

CREATE TABLE IF NOT EXISTS grid_live (
    rule_id      TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    dj           TEXT NOT NULL,
    start_hour   INTEGER NOT NULL CHECK (start_hour   BETWEEN 0 AND 23),
    start_minute INTEGER NOT NULL CHECK (start_minute BETWEEN 0 AND 59)
);
