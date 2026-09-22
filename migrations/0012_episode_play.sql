-- 0012_episode_play.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — HISTORIQUE PLAY-ONCE pour `unplayed_only`. Cooldown à
-- expiration INFINIE (distinct de broadcast_log, qui expire par fenêtre) :
-- « cet épisode a déjà été diffusé en entier pour CETTE playlist ».
--
-- Keyé par (playlist_ref canonique, identité d'épisode). Identité = chemin +
-- garde-fou (size_bytes, mtime_ns) : si le fichier change (taille/mtime
-- divergent), le mark devient périmé et l'épisode redevient éligible — le
-- garde-fou est comparé à la ligne `media` courante au moment du filtrage.
--
-- Comme toute la famille (B) : aucune FK, jamais remis à zéro par un scan/apply.
-- Écrit à la FIN d'une diffusion (grid_engine::on_episode_finished), pas au
-- démarrage : un épisode interrompu reste éligible. Temps : epoch UTC INTEGER.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS episode_play (
    playlist_ref TEXT NOT NULL,       -- ref canonique de la playlist unplayed_only
    rel_path     TEXT NOT NULL,       -- identité d'épisode : chemin
    size_bytes   INTEGER NOT NULL,    -- garde-fou : taille au moment du mark
    mtime_ns     INTEGER NOT NULL,    -- garde-fou : mtime au moment du mark
    played_at    INTEGER NOT NULL,    -- epoch UTC (fin de diffusion)
    PRIMARY KEY (playlist_ref, rel_path)
);
