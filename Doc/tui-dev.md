# TUI d'administration — premier jalon

Le TUI Ratatui est un client gRPC optionnel, au même rang que `stationctl`.
Il présente les données du daemon et propose un formulaire local de création
des TOML de playlists pour faciliter le développement. Le formulaire réutilise
le parseur et la validation de `stationd::playlist`, sans écrire dans SQLite.

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

Le polling ne déclenche que `Status`, `PlaylistList` et `ListRules`.
La création locale et `PlaylistSync` sont des actions utilisateur séparées ;
aucun accès SQLite dans le TUI.
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

## Création locale d'une playlist

Depuis l'onglet **Playlists**, `n` ouvre le formulaire, même si le daemon
est arrêté. Le répertoire cible est `[playlist].path` de `stationd.toml`
(ou du fichier passé par `--config`). Comme dans le daemon, les chemins
relatifs de configuration sont résolus depuis le répertoire de lancement.
`--playlist-root` permet de choisir explicitement un dossier local, sans
avoir besoin d'une configuration complète :

```sh
./target/debug/stationd-tui --playlist-root ./playlist
# Ou avec la configuration habituelle du daemon :
./target/debug/stationd-tui --config ./stationd.toml
```

Le formulaire comporte trois sections :

- **General** : chemin relatif du fichier `.toml`, nom, activation, mode.
- **Selection** : ordre et fichiers pour static ; filtres typés et tri pour
  dynamic ; URL pour remote ; ordre/taille pour queue ; stratégie et membres
  pour group. Les champs masqués lors d'un changement de mode restent dans
  le brouillon mais ne sont jamais émis pour un autre mode.
- **Broadcast** : paramètres de la grammaire effectivement implémentée dans
  `src/playlist.rs`, dont les fenêtres historiques facultatives. Le formulaire
  n'introduit pas les champs des propositions de grammaire non implémentées.

Touches du formulaire :

| Touche | Action |
|---|---|
| Tab / Shift-Tab, haut / bas | Champ suivant / précédent |
| Gauche / droite | Choix précédent / suivant, ou déplacement du curseur |
| F4 / Shift-F4 | Section suivante / précédente |
| F2 | Ajouter un fichier, filtre, membre ou fenêtre dans la section correspondante |
| F3 | Supprimer la ligne composée sélectionnée |
| Ctrl-U | Vider un champ texte |
| F5 ou Ctrl-S | Valider et afficher le TOML |
| Ctrl-S dans l'aperçu | Sauvegarder le fichier affiché |
| Esc dans l'aperçu | Revenir au formulaire |
| Esc / Ctrl-C dans le formulaire | Annuler ; un brouillon modifié demande confirmation |

Les lettres `q`, `r`, `a`, `n`, `s`, les accents et le collage restent de la
saisie dans le formulaire. Les valeurs scalaires et les chemins sont échappés
par le sérialiseur TOML. Les valeurs de filtre `texts` / `integers` utilisent
des éléments séparés par des virgules ; un élément texte contenant une virgule
doit être ajusté dans le TOML ensuite. Les champs/opérateurs de filtre restent
libres, conformément au parseur actuel : sa validation n'est pas un catalogue
complet des combinaisons métier.

La sauvegarde valide à nouveau le TOML, crée les sous-dossiers nécessaires
et publie un fichier complet via un temporaire sans suffixe `.toml`. Elle
refuse les chemins sortants, les sous-dossiers symboliques et les collisions
de référence, y compris de casse. Un fichier existant n'est jamais écrasé.
La création s'exécute en arrière-plan ; une erreur conserve le brouillon.
L'UUID est laissé absent jusqu'au passage habituel par `add` / `sync`.

Après sauvegarde, fermer le rapport avec Esc/Entrée puis presser `s` dans
Playlists pour synchroniser. Le rapport affiche les fichiers rejetés et leurs
erreurs ; il défile avec PgUp/PgDn. La validation des références et cycles de
groupes a lieu pendant cette synchronisation, pas pendant la création d'un
fichier isolé. Un échec de sync n'annule pas la sauvegarde locale.

Avec `--addr` distant, le fichier est toujours écrit sur la machine du TUI.
`s` scanne le dossier configuré **sur le daemon** : utiliser un dossier partagé
si l'on veut que le daemon distant voie ces fichiers. Aucun transfert implicite.

Tests : `cargo test --features tui --bin stationd-tui`. Ils couvrent notamment
les cinq modes, la saisie Unicode, les filtres typés, le changement de mode,
les membres de groupe, l'aperçu avant sauvegarde et le refus d'écrasement.

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

La modification des fichiers existants reste à ajouter ; le formulaire actuel
est réservé à la création locale. Le streaming attend un contrat
Watch/Subscribe et les événements de diffusion. Le TUI est de l'outillage,
pas un jalon du moteur de sélection.
