-- 0009_group_state.sql
-- ═══════════════════════════════════════════════════════════════════════════
-- FAMILLE (B) — ÉTAT DE PASSAGE d'un groupe `sequence`. Durable, préservé au
-- travers des `apply` (comme le curseur de parcours).
--
-- Un groupe `sequence` rend UNE piste par tour (frontière de piste). Il faut
-- donc mémoriser où on en est dans la séquence : quel membre est courant, et
-- combien de pistes ont déjà été rendues pour lui (quota `take`). Au tour
-- suivant : si `take_count` a atteint le `take` du membre, on passe au membre
-- suivant ; une fois le dernier membre dépassé, on reboucle au premier — une
-- NOUVELLE activation de l'émission (nouvelle intro tirée, dernier épisode,
-- nouvelle outro).
--
-- Clé = group_ref (le rel_path normalisé du groupe). Renommage = changement
-- d'identité, cohérent avec le reste.
-- ═══════════════════════════════════════════════════════════════════════════

CREATE TABLE IF NOT EXISTS group_state (
    group_ref  TEXT PRIMARY KEY,   -- rel_path normalisé du groupe
    member_idx INTEGER NOT NULL,   -- index du membre courant dans `members`
    take_count INTEGER NOT NULL,   -- pistes déjà rendues pour ce membre
    updated_at INTEGER NOT NULL    -- epoch (s) de la dernière avance
);
