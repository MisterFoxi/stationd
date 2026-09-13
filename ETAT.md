# État d'avancement — passation entre sessions

Journal de ce qui est **fait** et ce qui **reste**, pour reprendre le travail
sans reconstruire le contexte. À distinguer des docs de `Doc/` (décisions
d'architecture durables) : ce fichier-ci est volatil, à mettre à jour à
chaque session.

Dernière mise à jour : 2026-09-13.

---

## Où on en est en une phrase

Le circuit playlists est fonctionnel de bout en bout : config → parser TOML →
`add` / `sync` / `list` via gRPC, avec validation métier (par fichier + sur
l'ensemble) et une vue SQLite. Le crate est passé en **lib + bin** (option A),
la logique de `sync` est extraite dans un module `sync` testable hors tonic, et
un test d'intégration `tests/sync.rs` la couvre de bout en bout. 40 tests
unitaires verts (migration lib+bin validée) et 4 tests d'intégration `sync`
verts. Le câblage Liquidsoap/Icecast et le résolveur de
programmation ne sont pas commencés.

---

## Fait

### Config
- Section `[playlist] path` dans `stationd.toml` : racine des playlists,
  scannée récursivement. Chemin relatif au CWD (piège systemd noté plus bas).
- `config.rs` : `PlaylistConfig { path }`, champ requis (absence = erreur au
  démarrage, no-silent-failure).

### Modèle & parser (`src/playlist.rs`)
- Un fichier = une playlist. Table racine (`id`/`name`/`enabled` +
  sous-tables `[selection]`/`[broadcast]`). Désérialisation serde stricte
  (`deny_unknown_fields`).
- 5 modes : `static`, `dynamic`, `remote`, `queue`, `group`.
  - `remote` = un flux distant relayé (`url` + `name`). Distinct du podcast
    (podcast = fichier partagé entre applis, hors scope ici).
- `id` = UUID v4, **généré par le système**, absent d'un fichier écrit à la
  main (`id: Option<Uuid>`). `assign_id` l'injecte via `toml_edit` (lossless,
  commentaires/format préservés), idempotent.
- Validation par fichier : `order` selon `mode`, `take`/`weight` selon la
  strategy de groupe, cohérence des champs par mode, `url` réservé à `remote`.
- Validation d'ensemble (`validate_set`, pure, sans DB) : résolution des refs
  de groupe + détection de cycles.
  - `ref` d'un membre = **chemin relatif du fichier** (pas l'UUID, qui est
    cuisine interne). Ex. `ref = "nuit/goodnight"`.
  - `normalize_ref` : `\`→`/`, sans `.toml`, minuscules, anti-`..`, anti-`/`
    initial. Appliqué symétriquement au ref et au chemin scanné.

### gRPC (`proto/station.proto`, `src/grpc.rs`)
- RPC : `Status`, `Quit`, `PlaylistAdd`, `PlaylistSync`, `PlaylistList`.
- `add` : le CLI lit le fichier + pré-check syntaxique, envoie le contenu ;
  stationd parse/valide/génère l'UUID/écrit la vue, renvoie le TOML réécrit ;
  le CLI réécrit le fichier. (I/O délégué au CLI, métier dans stationd.)
- `sync` : stationd scanne **sa propre** racine (récursif), en 3 passes :
  1. chargement + validation par fichier (best-effort, collision de clés
     détectée) ; 2. `validate_set` (refs + cycles) ; 3. persistance des seuls
     survivants + réécriture ciblée (fichiers sans id uniquement, pas de churn
     git). Rapport d'erreurs nommées, exit ≠ 0 côté CLI si erreurs.
- `list` : lecture de la vue, tri par `rel_path` (NULL en dernier).

### Persistance (`src/store.rs`, `migrations/`)
- `store.rs` : `upsert` / `list` (fonctions pures sur `&SqlitePool`), type
  `PlaylistRow`. Les handlers gRPC délèguent ici (transport mince).
- Migrations : `0001` (mécanisme), `0002` (table `playlists` : id, name,
  enabled, toml brut), `0003` (colonne `rel_path` + index UNIQUE).
- **La vue stocke le TOML brut**, pas encore une vue « riche » requêtable.

### CLI (`src/bin/stationctl.rs`)
- `status`, `quit`, `playlist add <path>`, `playlist sync`, `playlist list`.

### Structure : lib + bin (option A, fait)
- `src/lib.rs` expose `pub mod config; db; grpc; playlist; store; sync;`.
  `main.rs` est devenu un binaire mince (`use stationd::…`). `stationctl.rs`
  reste autonome (proto généré chez lui, ne tire rien de la lib).
- Le Linux-only (`signal::unix`) reste dans `main.rs`, hors de la lib, pour ne
  pas contaminer sa portabilité. `Cargo.toml` inchangé : l'auto-détection
  lib+bin+bin suffit. **Migration validée : 40 tests verts, comportement
  constant.**

### Sync : logique extraite du handler (`src/sync.rs`)
- Le corps de `playlist_sync` vivait *dans* le handler gRPC (métier dans le
  transport). Extrait dans `sync::sync_root(db, root) -> SyncOutcome`, fonction
  sans dépendance tonic, appelable depuis `tests/`. Renvoie des types métier
  (`SyncOutcome`/`SyncError`), pas du proto — même discipline que `store` avec
  `PlaylistRow`. Le handler mappe `SyncError` → `PlaylistSyncError`.

### Robustesse
- `build.rs` : `rerun-if-changed=migrations` (+ proto). Corrige le bug
  rencontré : un binaire qui n'embarquait pas une migration fraîchement
  ajoutée et loggait quand même « migrations applied ».
- **40 tests unitaires** : 35 purs (`playlist.rs`) + 5 base (`store.rs`, vraie
  base migrée). **+ `tests/sync.rs`** : 4 tests d'intégration de bout en bout
  (best-effort, non-`.toml` ignoré, exclusion de cycle, collision de clés,
  réécriture ciblée de l'id) — tous verts.

---

## Reste à faire

### ⭐ PROCHAINE SESSION : premier proto du résolveur (scheduler)

Le refactor lib+bin (A) et le test d'intégration de `sync` sont faits et verts.
Le prochain cœur, d'après
`Doc/modele-programmation.md`, c'est le **résolveur de programmation** — et son
point d'entrée obligé est le **schéma proto d'une règle** (« C'est le premier
proto à écrire ; le reste du scheduling en découle »).

Cible : le message d'une règle avec `kind` (`AtClock`/`Every`/`DayPart`/
`BaseRotation`), portée de validité (toujours / plage récurrente / bornée à des
dates), mode `soft|hard`, flag de péremption. Puis `resolve_next(now, état) ->
Décision` et `stationctl schedule preview --at … / --next 24h` (simulable sans
attendre le wall-clock). À trancher au passage : la crate temps (`jiff` penché
dans le doc), premier type temps du projet.

### Autres tâches dans le périmètre playlists
- **Rapport de cycle partiel** : `validate_set` détecte toujours un cycle
  (sûreté OK, pas de boucle infinie possible) mais ne nomme pas forcément
  *tous* les fichiers du cycle. Rendre le rapport exact = Tarjan / composantes
  fortement connexes. Confort de diagnostic, non urgent.
- **Vraie vue matérialisée SQLite** : colonnes/tables pour `selection` /
  `broadcast`, pour requêter vite (drag & drop, prog). Aujourd'hui = TOML brut.
- **CRUD restant** : `remove`, `export` (redéverser la vue en TOML), évoqués
  dans les docs.
- **Reload / watch** : stationd surveille la racine et recharge sur édition
  externe (`stationctl reload`). Le `sync` en est la version manuelle.
- Points ouverts listés dans `Doc/playlists.md` (partage vs exclusivité d'un
  membre de groupe, précédence `limit` vs `constraints`, défaut de
  `on_exhausted`, sémantique du `schedule`, source feed podcast, mode
  `episodic` dédié).

### Hors périmètre playlists (chantiers suivants)
- **Résolveur / scheduler** (`Doc/modele-programmation.md`) : `resolve_next`,
  les 4 familles de règles, `soft`/`hard`, péremption des `AtClock`, état de
  lecture persisté. C'est le cœur suivant. Premier proto à écrire d'après le
  doc : le schéma d'une règle.
- **Câblage Liquidsoap / Icecast** : rien de branché (stationd ne pilote
  encore rien). `request.dynamic` + fallback côté Liquidsoap.
- **Scan de la bibliothèque média** (lecture des tags).
- **Gestion du temps** : crate à trancher (`jiff` vs `chrono`+`chrono-tz` ;
  `modele-programmation.md` penche `jiff`). Pas encore de type temps dans le
  code.
- **Rôles / permissions** : différés (enum fixe prévu, liste à définir).
- **Plugins WASM**, **mTLS gRPC** : reportés.

---

## Pièges & points de vigilance (vécus ou anticipés)

- **`signal::unix` dans `main.rs`** ne compile que sur Linux. Dev sous
  Windows (lecteur `X:`) mais build/run sur Linux (`/data/dev/stationd`).
- **Chemins relatifs de config** (`./playlist`, `./media`) résolus depuis le
  CWD, pas depuis l'emplacement du binaire ni du `stationd.toml`. À surveiller
  le jour du `WorkingDirectory` systemd.
- **`data/stationd.db` est jetable** (file-first) : en cas de souci de
  migration/schéma, la supprimer + relancer + `playlist sync` reconstruit tout
  depuis les TOML.
- **Migration pas ré-embarquée** : réglé par `build.rs`. Si un doute subsiste,
  recompilation forcée (`cargo clean -p stationd`) puis run.

---

## Fichiers clés

| Fichier | Rôle |
|---|---|
| `src/lib.rs` | Racine de la lib : `pub mod` des modules partagés |
| `src/config.rs` | Chargement config TOML |
| `src/playlist.rs` | Modèle, parser, validation (fichier + ensemble), assign_id |
| `src/store.rs` | Persistance vue (upsert/list), tests base |
| `src/sync.rs` | Réconciliation `sync_root` (walk + validate + persist), hors tonic |
| `src/grpc.rs` | Service gRPC (handlers minces, délèguent à store/sync) |
| `src/db.rs` | Init pool SQLite + migrations |
| `src/main.rs` | Binaire mince : démarrage daemon, shutdown (Linux `signal::unix`) |
| `src/bin/stationctl.rs` | CLI client gRPC (autonome) |
| `tests/sync.rs` | Test d'intégration de `sync` (base temp + arbre tempdir) |
| `proto/station.proto` | Contrat gRPC |
| `migrations/*.sql` | Schéma (0001→0003) |
| `Doc/*.md` | Décisions d'architecture (référence durable) |
