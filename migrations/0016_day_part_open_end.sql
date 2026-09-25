-- 0016 — `day_part` à fin ouverte (sans `end`).
--
-- Une tranche sans `end` court jusqu'au prochain début d'une autre tranche
-- (grille de programmes : chaque émission dure jusqu'à la suivante ; cf.
-- resolver::open_part_covers). La fin devient donc facultative : les deux
-- colonnes sont NULL ensemble, ou renseignées ensemble.
--
-- SQLite ne sait pas relâcher un NOT NULL : la table est reconstruite.
-- Famille (A) : reconstruite à chaque `schedule apply` de toute façon ; la
-- copie ne sert qu'à garder la grille en place d'ici là.

CREATE TABLE grid_day_part_new (
    rule_id      TEXT PRIMARY KEY REFERENCES grid_rule(id) ON DELETE CASCADE,
    playlist_ref TEXT NOT NULL,
    start_hour   INTEGER NOT NULL CHECK (start_hour   BETWEEN 0 AND 23),
    start_minute INTEGER NOT NULL CHECK (start_minute BETWEEN 0 AND 59),
    end_hour     INTEGER          CHECK (end_hour     BETWEEN 0 AND 23),
    end_minute   INTEGER          CHECK (end_minute   BETWEEN 0 AND 59),
    CHECK ((end_hour IS NULL) = (end_minute IS NULL))
);
INSERT INTO grid_day_part_new
    SELECT rule_id, playlist_ref, start_hour, start_minute, end_hour, end_minute
    FROM grid_day_part;
DROP TABLE grid_day_part;
ALTER TABLE grid_day_part_new RENAME TO grid_day_part;
