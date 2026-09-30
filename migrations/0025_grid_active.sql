-- Grille active (lot 5) : les grilles sont des fichiers de `[grid] path`
-- (`grid/` par défaut), une seule est appliquée. Son nom est gardé ici —
-- `stationctl schedule activate <nom>` le change, il survit au redémarrage.
-- Aucune ligne = `grid.toml`. État durable (famille B) : un `schedule apply`
-- ne le touche pas.
CREATE TABLE grid_active (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    name TEXT NOT NULL
);
