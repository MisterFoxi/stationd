-- 0013_queue_entry.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — TAMPON RUNTIME d'une playlist `queue` (demandes auditeurs /
-- injection DJ). Rempli au runtime (push), consommé FIFO/LIFO : l'entrée jouée
-- est SUPPRIMÉE. Survit au restart (une demande en attente ne doit pas
-- disparaître). Aucune FK, keyé par playlist_ref. Temps : epoch UTC INTEGER.
--
-- FIFO = plus petit `id` d'abord ; LIFO = plus grand. `max_len` (config de la
-- playlist) borne l'accumulation : le push est REFUSÉ au-delà (pas de drop
-- silencieux) ; 0/absent = illimité.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS queue_entry (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    playlist_ref TEXT NOT NULL,
    rel_path     TEXT NOT NULL,
    enqueued_at  INTEGER NOT NULL       -- epoch UTC du push
);

-- Pop/len interrogent par playlist, ordonné par id.
CREATE INDEX IF NOT EXISTS queue_entry_ref ON queue_entry (playlist_ref, id);
