# TUI d'administration — premier jalon

Le TUI Ratatui est un client gRPC optionnel, au même rang que `stationctl`.
Il présente les données du daemon ; toute décision métier reste dans StationD.

## Périmètre implémenté

- Binaire `stationd-tui`, activé par la feature Cargo `tui`.
- Onglets Status / Playlists / Grid, sélection et panneau de détail.
- État du daemon : nom, fuseau, uptime et PID.
- Playlists : nom, UUID, chemin relatif, mode et activation (résumé du RPC existant).
- Grille : les quatre familles de règles, paramètres, activation et validité.
  Les heures affichées sont les heures civiles du fuseau de la station.
- Polling toutes les trois secondes après la réponse précédente ; `r` pour
  rafraîchir, `a` pour passer en manuel ou reprendre l'auto.
- Requêtes en arrière-plan, délai de cinq secondes par RPC, une actualisation
  à la fois. Le terminal reste utilisable pendant les appels.
- Une erreur est affichée pour chaque ressource concernée. Les dernières
  données reçues restent visibles et marquées `STALE`. Une erreur ou un RPC
  `UNIMPLEMENTED` n'est jamais présenté comme une liste vide réussie.
- La sélection suit l'identité de l'objet après un rafraîchissement.
- Affichage côte à côte à partir de 90 colonnes, vertical en dessous.
  Minimum 40 colonnes et 12 lignes ; les détails défilent avec PgUp/PgDn.
- `q` / Ctrl-C ferme le client. Le daemon poursuit son travail.

La v1 ne déclenche que `Status`, `PlaylistList` et `ListRules`. Aucune écriture
locale, aucun accès SQLite, aucun appel de mutation dans le TUI.
`ResolveNext` est une opération vivante qui persiste des effets : il ne doit
pas servir de sonde de rafraîchissement. `Preview` reste non implémenté.

## Préalables réalisés dans ce correctif

- `stationd::proto::{station, schedule}` partage les types générés et clients
  entre daemon, CLI et TUI. Les anciens chemins publics côté serveur sont
  conservés par réexport.
- `GridEngine::list_rules` lit l'index, sans toucher à l'état de lecture.
- Le handler `ListRules` convertit les quatre familles vers le contrat et
  trie les règles par ID pour une présentation stable.
- `stationctl schedule list` donne accès à la même lecture.

## Construire et lancer

Depuis la racine du dépôt, avec la chaîne Rust du projet et `protoc` :

```sh
cargo build --features tui --bin stationd --bin stationctl --bin stationd-tui
cargo test --features tui
```

Le premier build résout les dépendances Ratatui/Crossterm et met à jour
`Cargo.lock`. Conserver ce lockfile après validation. Ne pas utiliser
`--locked` avant cette première résolution.

Redémarrer le daemon avec son nouveau binaire pour disposer de `ListRules`,
puis, dans un terminal séparé :

```sh
./target/debug/stationctl schedule list
./target/debug/stationd-tui --addr http://127.0.0.1:50051
```

Autres options :

```sh
./target/debug/stationd-tui --manual
./target/debug/stationd-tui --refresh-seconds 5
```

Navigation : Tab / Shift-Tab ou 1–3 pour les vues ; flèches ou j/k pour la
sélection ; Home/End pour le premier/dernier élément ; PgUp/PgDn pour les
détails ; `?` pour l'aide.

## Validation et limites de livraison

Ce correctif a été préparé sur `dev` au commit
`c0cf92d7a3e30cfc4b985aa03c6eff44b24422c0`.
L'environnement de préparation ne possède pas Rust/Cargo/protoc ; leur
installation n'a pas abouti. La compilation, les tests Rust et l'essai en
terminal réel restent donc à exécuter. L'analyse syntaxique Rust, le manifeste
TOML et l'application du patch sont vérifiés séparément.

Les tests fournis couvrent le mapping des variantes, la conservation de
l'état de lecture pendant `ListRules`, les erreurs de lecture, la sélection
après réordonnancement/suppression et les tailles de terminal.

Les détails playlists sont limités aux champs de `PlaylistSummary` : aucune
lecture TOML parallèle n'est ajoutée pour contourner le contrat. Une grille
sans règles dans l'index apparaît vide ; le TUI n'en fabrique aucune.

## Suite

Les éditions s'ajouteront lorsque leurs RPC seront disponibles, avec la même
capacité accessible depuis `stationctl`. Le streaming attend un contrat
Watch/Subscribe et les événements de diffusion. Le TUI est de l'outillage,
pas un jalon du moteur de sélection.
