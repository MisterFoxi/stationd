-- 0010_group_runtime_shuffle.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — extension de l'ÉTAT DE PASSAGE d'un groupe (`group_state`),
-- pour deux nouveautés de rotation de groupe :
--
--   1. Quota par membre exprimé en TEMPS (`runtime = "20m"`), à côté du quota
--      en NOMBRE de pistes (`take`). Le budget est du temps mural écoulé depuis
--      le début du membre courant : il faut donc mémoriser cet instant de
--      départ pour, au bord de piste suivant, décider si le budget est épuisé
--      (soft : la piste en cours déborde, on bascule au bord d'après). Après un
--      arrêt plus long que le budget, le membre est périmé au redémarrage → on
--      avance (catch-up), cohérent avec le résolveur.
--
--   2. Stratégie `shuffle` : mêmes membres qu'une `sequence`, mais parcourus
--      dans un ordre TIRÉ AU HASARD, re-tiré à chaque cycle complet. La
--      permutation courante est persistée pour qu'un redémarrage en milieu de
--      cycle NE rebatte PAS les cartes (sinon un membre pourrait repasser deux
--      fois). `member_idx` indexe alors la permutation, pas `members`.
--
-- Colonnes nullables : les lignes existantes (groupes `sequence` à quota
-- `take`) restent valides — `member_started_at` NULL = pas de budget temps en
-- cours, `permutation` NULL = ordre déclaré (sequence). Famille (B), préservée
-- au travers des `apply` comme le reste de `group_state`.
-- ═══════════════════════════════════════════════════════════════════════════

ALTER TABLE group_state ADD COLUMN member_started_at INTEGER;  -- epoch (s) de début du membre courant (budget `runtime`)
ALTER TABLE group_state ADD COLUMN permutation       TEXT;     -- ordre du cycle courant (indices CSV), pour `shuffle`
