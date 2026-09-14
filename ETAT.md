# État d'avancement — passation entre sessions

Journal de ce qui est **fait** et ce qui **reste**, pour reprendre le travail
sans reconstruire le contexte. À distinguer des docs de `Doc/` (décisions
d'architecture durables) : ce fichier-ci est volatil, à mettre à jour à
chaque session.

Dernière mise à jour : 2026-09-14.


## Ajout : TUI d'administration (correctif préparé, compilation à confirmer)

Premier jalon Ratatui derrière la feature `tui` : `stationd-tui`, vues
Status/Playlists/Grid et polling gRPC. Modules générés partagés dans `proto`,
`ListRules` relié à la lecture de l'index, `stationctl schedule list` ajouté.
Aucun appel au résolveur vivant pour afficher les données.

**Validation restante :** `cargo build --features tui` puis
`cargo test --features tui`, et essai terminal avec le daemon reconstruit.
Rust/Cargo/protoc indisponibles dans l'environnement de préparation ; le
lockfile doit être actualisé par le premier build. Les indications plus bas
sur `ListRules` non implémenté décrivent l'état antérieur à ce correctif.
Voir `Doc/tui-dev.md` pour le périmètre, les commandes et les limites.

---

## Où on en est en une phrase

Le circuit playlists était déjà fonctionnel de bout en bout ; cette session a
bâti **toute la couche grille / résolveur** : du cœur pur `resolve_next`
jusqu'à un RPC `ScheduleService.ResolveNext` servi et joignable via
`stationctl schedule next`, avec temps correct DST (`jiff`), état de lecture
persisté et index des règles en SQLite. Tout ce qui va jusqu'à `grid_engine` +
`config` est **compilé et vert** ; le dernier lot (câblage gRPC du service
scheduling, option A) **compile** (`cargo build` OK). Reste à confirmer la
suite de tests complète et l'essai bout-en-bout — cf. tâche d'entrée.

---

## ⭐ TÂCHE D'ENTRÉE PROCHAINE SESSION

1. **Compiler + tester** (Rust indispo dans l'env de prépa) :
   `cargo build` puis `cargo test -p stationd`. Nouveau module `grid_toml` +
   `grid_index::replace_grid` + méthodes `GridEngine::{validate,apply,export}_grid`
   + 3 RPC `ApplyGrid`/`ValidateGrid`/`ExportGrid` branchés. Points de
   vigilance build ci-dessous.
2. **Glu CLI `stationctl schedule apply|validate|export`** (délibérément
   différée cette session) : lire `src/bin/stationctl.rs`, ajouter les
   sous-commandes (lecture fichier `grid.toml` côté client → `GridFile`, appel
   du même endpoint que `schedule next`). C'est ce qui rend la grille pilotable
   en CLI de bout en bout (invariant CLI-first).
3. **Test bout en bout** : `stationctl schedule apply grid.toml`, puis
   `stationctl schedule next --at 32400` (09:00 UTC) → `origin` + `playlist_ref`.
4. **Ensuite** : `Preview` (projection grille sur fenêtre), étage sélection
   `playlist_ref` → média, refacto acteur `GridEngine`, `DayPart` cross-minuit.

---

## Fait

### — Grammaire TOML de la grille + apply/validate/export (2026-09-14) —

**Point « 2b » terminé.** Contrat figé dans
`Doc/proposition-grammaire-grille-v1.md` : un `grid.toml` unique,
`schema_version = 1` obligatoire, conteneur `[[rule]]` (une règle n'est jamais
référencée → divergence assumée vs playlists), `kind` qui gate les champs
(comme `mode` playlist), fenêtre `day_part` `[start,end)` molle (cross-minuit
rejeté en v1), `at_clock` = `every_minutes` XOR `at` + `soft|hard` + `expiry`,
`every` = `min_tracks` XOR `min_elapsed`, durées `[1-9][0-9]*(s|m|h|d)`.

- `src/grid_toml.rs` (pur/std-only, 15 tests) : `parse_grid` (parse strict
  `deny_unknown_fields` → `Vec<Rule>`, fail-fast avec id de règle ; set-level :
  ids uniques, ≤ 1 `base_rotation`), `validate_refs` (best-effort, refs vs
  clés playlists connues, via `playlist::normalize_ref`), `to_toml` (export,
  sortie stable, non lossless). ⚠ doublon assumé de `parse_date`/`parse_weekday`
  avec `grid_index` (copies std-only, portées séparées).
- `src/grid_index.rs` : `replace_grid(pool, &[Rule])` — DROP+rebuild famille
  (A) transactionnel (les 6 tables vidées explicitement, cascade FK non
  activée), famille (B) intacte. Corps d'insertion partagé `insert_rule_in_tx`.
- `src/grid_engine.rs` : `GridOpError` (Invalid → `invalid_argument`, Infra →
  `internal`), `validate_grid`/`apply_grid`/`export_grid` + `known_playlist_keys`
  (refs résolues vs `rel_path` de la vue). `apply` rejette **sans rien écrire**
  si une ref est inconnue.
- `src/schedule_grpc.rs` : 3 RPC branchés sur le moteur (fin des `UNIMPLEMENTED`).

### — Couche grille / résolveur (session précédente) —

**Invariant structurant, gravé partout : deux familles de tables.**
(A) index reconstructible (dérivé des TOML, `apply` fait DROP+rebuild) ;
(B) état durable (jamais dérivable, `apply` n'y touche jamais). Visible jusque
dans le découpage des modules (`grid_index` = A, `grid_store` = B).

#### Résolveur pur (`src/resolver.rs`, 13 tests)
- `resolve_next(now, grid, state) -> GridDecision` : **pur, sans fuseau, sans
  I/O**. Reçoit un `LocalNow` déjà décomposé → testable sans wall-clock.
- 4 familles : `BaseRotation` (plancher), `DayPart` (fenêtre qui sélectionne la
  base), `AtClock` (rendez-vous horloge), `Every` (cadence glissante).
- Ordre de collision fixe : `AtClock hard > AtClock soft > Every > base
  (DayPart → BaseRotation) > fallback`.
- `DayPart.end` = borne de validité (`start ≤ now < end`), **jamais une coupe**
  (borne molle : la dernière piste déborde).
- `AtClock` : ancrage `EveryMinutes(N)` XOR `At(hh:mm)`, `soft|hard`, péremption
  par règle (`expiry_secs`), token d'occurrence anti-rejeu. `soft` = rattrapage
  au prochain bord de piste, repères dépassés **fusionnés** (pas de rafale).
- `Epoch(i64)` newtype (jamais `i64` nu, cf. `time.md`).

#### Frontière temps / DST (`src/clock.rs`, `jiff` 0.2, 4 tests)
- `to_local_now(epoch, tz) -> LocalNow` : **seul endroit** qui décompose epoch
  UTC → civil local avec DST. Fuseau inconnu → `ClockError` (pas d'avalage).
- Tests anti-DST Europe/Paris : nuit de printemps (02:30 inexistant, le mur
  saute 01:30→03:30), nuit d'automne (02:30 deux fois, epochs distincts).

#### État durable grille — famille (B) (`src/grid_store.rs`, migration `0005`, 5 tests)
- Tables `every_state` (compteurs `tracks_since` / `last_played` par règle) et
  `at_clock_taken` (tokens d'occurrences consommées). **Aucune FK vers (A)** →
  survit au rebuild de l'index.
- Fonctions : `load_playback_state`, `ensure_every_rows`, `bump_tracks_since`,
  `reset_every`, `record_at_clock_taken`. Ordre d'appel documenté en tête
  (c'est la boucle qui l'orchestre). Test-clé : `ensure` idempotent **ne remet
  pas** un compteur à zéro au reload.

#### Index des règles — famille (A) (`src/grid_index.rs`, migration `0006`, 3 tests)
- Table `grid_rule` + une table de détail par variante (`grid_base_rotation`,
  `grid_day_part`, `grid_at_clock`, `grid_every`) + `grid_rule_weekday` (aucune
  ligne = tous les jours). XOR ancrage/cadence en `CHECK` (état illégal non
  représentable).
- `load_grid(pool) -> Grid` (lignes plates → enum `RuleKind`) ; `insert_rule`
  transactionnel. Test bout en bout : DB → `load_grid` → `resolve_next` rend
  `jazz` à 09:00.

#### Boucle vivante (`src/grid_engine.rs`, 3 tests)
- `GridEngine { pool, tz }` : `next(now)` = `clock` → `load_grid` +
  `load_playback_state` → `resolve_next` → persistance des effets
  (`record_at_clock_taken`, `reset_every`). `on_track_completed` (bump),
  `sync_grid` (ensure au démarrage, catch-up).
- Choix assumés : recharge à chaque appel (correct/simple ; cible = tâche
  tokio possédante + mpsc, optimisation) ; `next` et `on_track_completed` sont
  deux événements distincts.

#### Config : fuseau station (`src/config.rs`, 3 tests)
- `StationConfig.timezone` (IANA, **obligatoire**), validé au load via `jiff`
  → fuseau bidon = pas de démarrage (no-silent-failure). `stationd.toml` +
  `.example` mis à jour.

#### Protos & câblage gRPC (option A — **compile ✓**)
- `proto/schedule_v1.proto` : `ScheduleService` (grille + `ResolveNext` +
  `Preview`), 4 familles, `soft|hard`, péremption, portée de validité.
  Compilé par `build.rs` (+ `prost-types`).
- `src/schedule_grpc.rs` : handler mince sur `GridEngine`. **`resolve_next`,
  `list_rules`, `apply_grid`, `validate_grid`, `export_grid` réels** ; seul
  `preview` reste `UNIMPLEMENTED` (projection à concevoir).
- `main.rs` : construit `GridEngine`, `sync_grid` au démarrage, second
  `add_service(ScheduleServiceServer)` sur le même serveur/port.
- `stationctl schedule next [--at <epoch>]` : client du même endpoint.

#### Migration `0004` (vue matérialisée riche playlists)
- `ALTER` de `playlists` : `handle`, `ref_effective = handle ?? name`
  (VIRTUAL — SQLite interdit d'ajouter une STORED générée), `mode`, colonnes
  `broadcast`, tables de détail (`playlist_static/_dynamic/_group` + fichiers/
  filtres/membres), famille (B) playlists (`episode_play`, `broadcast_log`,
  `playlist_suspension`). Arêtes de groupe par `id`, pas par ref.
- **Index UNIQUE sur `ref_effective` volontairement différé** : le code
  (`store::upsert`) ne peuple pas encore `handle` et laisse `name` libre →
  l'imposer casserait. Unicité au service pour l'instant ; migration ultérieure
  quand `handle` sera câblé. `rel_path` (0003) rétrogradé en localisation de
  fichier.
- ⚠ La migration ajoute la STRUCTURE, ne rétro-remplit pas (une migration SQL
  ne parse pas de TOML) : les colonnes/détails restent NULL/vides jusqu'à un
  `apply`/reload complet.

### — Playlists (sessions précédentes, inchangé) —
- Config `[playlist] path`, parser strict (`src/playlist.rs`), 5 modes, `id`
  UUID assigné (`assign_id` lossless), validation fichier + ensemble
  (`validate_set` : refs + cycles), `ref` = chemin relatif normalisé.
- gRPC `station.proto` : `Status`/`Quit`/`PlaylistAdd`/`PlaylistSync`/
  `PlaylistList`. Persistance `store.rs` (TOML brut en vue). `sync.rs` extrait,
  `tests/sync.rs` (4 tests). Structure lib+bin.
- ⚠ `proto/playlist_v1.proto` (slice Homestone : identité 3 étages, oneof
  sélection, groupe sequence, `member`) existe **mais n'est pas encore compilé
  par `build.rs` ni servi** — seuls `station.proto` et `schedule_v1.proto` le
  sont.

---

## Reste à faire

### Grille / scheduler (suite directe)
- **Confirmer build+tests** (tâche d'entrée), puis **glu CLI
  `stationctl schedule apply|validate|export`** (RPC prêts côté serveur, CLI à
  écrire).
- **`Preview`** : projection de grille sur une fenêtre (pas une sim
  piste-à-piste, durées dynamiques) — RPC déclaré, à implémenter.
- **Étage sélection** : `playlist_ref` → média concret (le `Decision.media_path`
  est vide pour l'instant ; c'est le moteur de sélection du slice playlist).
- **Refacto acteur** : `GridEngine` en tâche tokio possédante, grille en
  mémoire invalidée à l'apply, mutations par mpsc.
- **`DayPart` cross-minuit** : `window_covers` renvoie `None` (TODO signalé).

### Playlists (périmètre existant)
- Compiler+servir `playlist_v1.proto` (nouveau contrat) et migrer le code
  (`store`/`playlist`/`sync`) vers l'identité `name`/`handle` → alors seulement
  reposer l'index UNIQUE `ref_effective`.
- Vraie vue matérialisée **peuplée** (l'apply qui éclate le TOML dans les tables
  de `0004`). CRUD `remove`/`export`, reload/watch. Rapport de cycle exact
  (Tarjan). Points ouverts `Doc/playlists.md`.

### Hors périmètre (chantiers suivants)
- Câblage Liquidsoap/Icecast (`request.dynamic` + fallback). Scan biblio
  (tags). Rôles/permissions (différés). Plugins WASM, mTLS gRPC (reportés).
- Maintenance : `apalis` (scan, retry NFS) — `Doc/modele-programmation.md`.

---

## Pièges & points de vigilance

- **Base de dev à recréer** : migrations `0004`→`0006` sont neuves. En cas de
  souci de schéma/checksum sqlx, `rm -rf data/` + relancer (file-first, la vue
  est jetable). Ne JAMAIS éditer une migration déjà appliquée en prod.
- **`resolver.rs` doit rester pur / std-only** : c'est ce qui le rend testable
  et simulable sans horloge. Toute conversion de fuseau passe par `clock`.
- **`AtClock` soft sans `expiry`** peut se déclencher tard (au prochain bord de
  piste), repères intermédiaires sautés (fusion). Pour taper l'heure pile :
  `Mode::Hard` + `expiry` court.
- **`ref_effective` pas encore unique en base** (index différé, cf. 0004).
- **`signal::unix` dans `main.rs`** = Linux only. Dev Windows (`X:`) / build+run
  Linux (`/data/dev/stationd`). `X:\stationd` = `\\devradio.lan\dev\stationd`
  (même dépôt, lecteur mappé).
- **Chemins relatifs de config** résolus depuis le CWD (piège `WorkingDirectory`
  systemd).

---

## Fichiers clés

| Fichier | Rôle |
|---|---|
| `src/resolver.rs` | Cœur pur `resolve_next` + 4 familles (std-only, 13 tests) |
| `src/clock.rs` | Frontière temps epoch↔civil local, DST (`jiff`, 4 tests) |
| `src/grid_index.rs` | Index règles famille (A) : `load_grid`/`insert_rule`/`replace_grid` |
| `src/grid_toml.rs` | Grammaire `grid.toml` : `parse_grid`/`validate_refs`/`to_toml` (15 tests) |
| `src/grid_store.rs` | État durable famille (B) : compteurs Every / tokens AtClock |
| `src/grid_engine.rs` | Boucle vivante (résolution + persistance) |
| `src/schedule_grpc.rs` | Service gRPC scheduling (`ResolveNext` réel) |
| `src/config.rs` | Config TOML + fuseau station validé |
| `src/playlist.rs` | Modèle/parser/validation playlists |
| `src/store.rs` / `src/sync.rs` | Vue playlists / réconciliation |
| `src/grpc.rs` | Service `Station` (status/quit/playlist*) |
| `src/db.rs` | Init pool SQLite + migrations |
| `src/main.rs` | Daemon : démarrage, 2 services gRPC, shutdown |
| `src/bin/stationctl.rs` | CLI (station + `schedule next`) |
| `proto/station.proto` | Contrat `Station` |
| `proto/schedule_v1.proto` | Contrat `ScheduleService` (compilé/servi ; apply/validate/export réels) |
| `Doc/proposition-grammaire-grille-v1.md` | Contrat grammaire `grid.toml` (référence durable) |
| `proto/playlist_v1.proto` | Contrat playlist v1 (⚠ pas encore compilé/servi) |
| `migrations/0001→0006` | Schéma (0004 vue riche, 0005 état grille, 0006 règles) |
| `tests/sync.rs` | Intégration `sync` |
| `Doc/*.md` | Décisions d'architecture (référence durable) |
