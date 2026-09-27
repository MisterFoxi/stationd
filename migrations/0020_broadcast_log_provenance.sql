-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — PROVENANCE ET DIFFUSION RÉELLE dans l'historique de diffusion.
--
-- Chaque ligne de `broadcast_log` (écrite quand stationd CHOISIT une piste)
-- dit maintenant d'où elle vient, pour les statistiques par playlist / règle
-- (`stationctl stats`) :
--   rule_id      règle de grille gagnante (NULL : override, repli)
--   origin       AtClockHard | AtClockSoft | Every | DayPart | BaseRotation | Override
--   playlist_ref playlist de la règle / de l'override (un groupe reste le groupe)
--   leaf_ref     playlist feuille qui a produit le fichier (le membre d'un groupe)
--   aired_at     epoch UTC où Liquidsoap l'a réellement démarrée ; NULL = choisie
--                mais jamais partie à l'antenne (préparée puis vidée par une
--                coupure, arrêt…) ou pas de Liquidsoap.
-- Lignes antérieures : colonnes NULL (provenance inconnue).
-- ═══════════════════════════════════════════════════════════════════════════

ALTER TABLE broadcast_log ADD COLUMN rule_id      TEXT;
ALTER TABLE broadcast_log ADD COLUMN origin       TEXT;
ALTER TABLE broadcast_log ADD COLUMN playlist_ref TEXT;
ALTER TABLE broadcast_log ADD COLUMN leaf_ref     TEXT;
ALTER TABLE broadcast_log ADD COLUMN aired_at     INTEGER;
