-- 0005_grid_playback_state.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — ÉTAT DURABLE du résolveur de grille.
-- Pendant SQL de resolver::PlaybackState. Jamais touché par un apply/reload.
--
-- Comme toute la famille (B) : AUCUNE FK vers les règles de grille (elles sont
-- de l'index reconstructible). Keyé par rule_id / token. Un rebuild de la
-- grille ne doit pas remettre à zéro un compteur Every — sinon la cadence
-- casse au premier reload, exactement le bug à ne pas réintroduire.
--
-- Temps : epoch UTC en INTEGER (time.md).
-- ═══════════════════════════════════════════════════════════════════════════

-- Cooldown glissant par règle Every. Sans persistance, un restart remet à zéro
-- « N pistes depuis le dernier passage » et casse le cadencement.
CREATE TABLE IF NOT EXISTS every_state (
    rule_id      TEXT PRIMARY KEY,                  -- pas de FK (survit au rebuild)
    last_played  INTEGER,                           -- epoch UTC ; NULL = jamais joué
    tracks_since INTEGER NOT NULL DEFAULT 0 CHECK (tracks_since >= 0)
);

-- Occurrences AtClock déjà consommées, par token (rule_id@YYYY-MM-DDTHH:MM).
-- Empêche un même rendez-vous de se déclencher deux fois (pendant sa propre
-- activation, ou après un restart dans la même minute). `taken_at` sert au
-- futur élagage des vieux tokens (tâche de maintenance, pas ici).
CREATE TABLE IF NOT EXISTS at_clock_taken (
    token    TEXT PRIMARY KEY,
    taken_at INTEGER NOT NULL                       -- epoch UTC
);
