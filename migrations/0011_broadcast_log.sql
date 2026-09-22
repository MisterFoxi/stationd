-- 0011_broadcast_log.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — HISTORIQUE DE DIFFUSION station-wide (« titre X démarré à T »).
-- Alimente l'anti-répétition : `no_same_track_within` / `no_same_artist_within`.
--
-- Comme toute la famille (B) : AUCUNE FK (l'index média/playlists est
-- reconstructible ; l'historique, lui, ne l'est pas — il ne doit pas être
-- remis à zéro par un scan ou un apply). Adressage par identité (rel_path /
-- artist), jamais par FK. Temps : epoch UTC en INTEGER (time.md).
--
-- Écrit au DÉMARRAGE d'une piste (grid_engine::next_media), lu à la sélection
-- (selection::apply_constraints) pour écarter les candidats récemment joués.
-- Journal en append-only : une ligne par piste démarrée.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS broadcast_log (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    rel_path  TEXT NOT NULL,                     -- média démarré (identité)
    artist    TEXT,                              -- NULL = non taggé ; ne matche jamais une fenêtre artiste
    played_at INTEGER NOT NULL                   -- epoch UTC du démarrage
);

-- Les fenêtres anti-répétition interrogent « joué depuis T » → index temporel.
CREATE INDEX IF NOT EXISTS broadcast_log_played_at ON broadcast_log (played_at);
