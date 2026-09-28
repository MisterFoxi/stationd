-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — FIN DE DIFFUSION dans l'historique de diffusion.
--
-- Écrit quand une de nos pistes quitte l'antenne (GridEngine::on_track_left,
-- rapporté par le pont Liquidsoap) : l'historique de l'antenne (OnAirService)
-- distingue « diffusé jusqu'au bout » de « coupé ».
--   left_at        epoch UTC où la piste a quitté l'antenne ; NULL = pas (encore)
--                  rapporté (à l'antenne, lignes antérieures, pas de Liquidsoap)
--   played_to_end  1 = jusqu'au bout (tolérance du fondu), 0 = coupée,
--                  NULL = inconnu (durée non indexée, lignes antérieures)
-- ═══════════════════════════════════════════════════════════════════════════

ALTER TABLE broadcast_log ADD COLUMN left_at       INTEGER;
ALTER TABLE broadcast_log ADD COLUMN played_to_end INTEGER;

-- L'historique de l'antenne lit « diffusé avant T », du plus récent au plus ancien.
CREATE INDEX IF NOT EXISTS broadcast_log_aired_at ON broadcast_log (aired_at);
