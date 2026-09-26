# Exemples

Point de départ pour une station : des playlists (un fichier par playlist,
un exemple par mode) et une grille qui les programme. Ces fichiers sont
vérifiés par `cargo test` (`tests/examples.rs`) : ils suivent la grammaire
réelle de stationd.

Pour s'en servir : copier ce qu'on veut garder dans `playlist/` (et
`grid.toml` à la racine), adapter les chemins à sa médiathèque, puis

```sh
stationctl playlist sync            # charge playlist/ (attribue les id)
stationctl schedule validate grid.toml
stationctl schedule apply grid.toml
stationctl schedule preview         # ce que la grille va diffuser
```

Chemins de médias (`files`, filtres `path`) : relatifs à `[media]
library_path`. Référence d'une playlist (`playlist_ref`, `ref` d'un membre) :
son chemin sous `playlist/`, sans `.toml`, en minuscules — `emission/intro`.
Un `ref` qui commence par `./` ou `../` est relatif au dossier du groupe.
