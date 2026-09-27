-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — GROUPE QUI GARDE L'ANTENNE.
--
-- Une règle `every` / `at_clock` qui lance un groupe `sequence` / `shuffle`
-- lui laisse l'antenne jusqu'à la fin de son cycle (tous les membres, avec
-- leurs quotas `take` / `runtime`), même quand la règle n'est plus due au
-- tour suivant (son `every` est remis à zéro dès la première piste). Sans ça,
-- un groupe « jingle puis hit » ne jouait que le jingle.
--
-- Une seule ligne au plus : le groupe tenu. Survit à un redémarrage (le cycle
-- reprend où il en était, cf. `group_state`). Effacée à la fin du cycle, sur
-- `on_member_unavailable = "abort"` d'un membre vide, ou si la règle a disparu
-- de la grille / a changé de playlist. Seuls un override et un `at_clock hard`
-- passent devant.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS grid_hold (
    id           INTEGER PRIMARY KEY CHECK (id = 1),
    rule_id      TEXT    NOT NULL,   -- règle qui a lancé le groupe
    playlist_ref TEXT    NOT NULL,   -- le groupe (ref canonique de la règle)
    origin       TEXT    NOT NULL,   -- every | at_clock_hard | at_clock_soft
    started_at   INTEGER NOT NULL    -- epoch UTC du premier morceau du cycle
);
