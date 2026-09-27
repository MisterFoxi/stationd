-- 0018_live_opening.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — OUVERTURES PONCTUELLES DU LIVE (`stationctl live open`).
--
-- Une fenêtre de connexion accordée à un DJ hors de la grille, de `opened_at`
-- à `until` (epoch UTC). Active tant que `until > now`. `live close` la
-- referme en ramenant `until` à l'instant de fermeture ; une nouvelle
-- ouverture pour le même DJ referme de même l'ouverture active.
--
-- `cut` = le DJ a été coupé (silence ou kick) pendant cette ouverture : il
-- est refusé jusqu'à sa fin (même règle que pour un créneau de grille).
--
-- Comme toute la famille (B) : aucune FK, jamais remise à zéro par un apply
-- ou un scan ; survit au redémarrage de stationd.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS live_opening (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    dj        TEXT    NOT NULL,                  -- id du fichier des DJ
    opened_at INTEGER NOT NULL,                  -- epoch UTC
    until     INTEGER NOT NULL,                  -- epoch UTC, fin (exclue)
    cut       INTEGER NOT NULL DEFAULT 0 CHECK (cut IN (0, 1))
);

-- « l'ouverture active de ce DJ » : dj + fin.
CREATE INDEX IF NOT EXISTS live_opening_dj_until ON live_opening (dj, until);
