# État d'avancement — passation entre sessions

Journal de ce qui est **fait** et ce qui **reste**, pour reprendre le travail
sans reconstruire le contexte. À distinguer des docs de `Doc/` (décisions
d'architecture durables) : ce fichier-ci est volatil, à mettre à jour à
chaque session.

Dernière mise à jour : 2026-09-15.


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

La **grille est pilotable de bout en bout en CLI**, projection comprise :
`grid.toml` (4 familles) → `stationctl schedule validate|apply|export|list|next|preview`
→ gRPC → moteur → SQLite. Testé en vrai : apply de 4 règles, `next` rend
`AT_CLOCK_SOFT`, export round-trip stable, `preview` projette 24h avec rendu
UTC + local nommé (test anti-DST 2026-10-25 : 02:30 deux fois, epochs
distincts). Cœur pur (`resolve_next`, `clock`/DST `jiff`, familles A/B) vert,
grammaire + les 6 RPC réels. `cargo build` + `cargo test -p stationd` OK.
Biblio média scannée et pilotable au CLI (`library scan|list`). **Étage
sélection livré** : `playlist_ref` → `media_path` concret (shuffle, sequential,
newest/oldest via curseur, groupe `sequence`). Validé en réel de bout en bout :
grille → groupe intro / épisode le plus récent / outro → fichiers réels.
**Système de plugins** : A1 (registre, cycle de vie, `on_event`) + **hook
`filter_pool`** (le pool est matérialisé puis filtré par les plugins avant le
choix) + **runtime WASM (WASM-1)** — un `.wasm` externe (extism) implémente le
trait via JSON. `stationctl plugin list|start|stop|restart|reload`, plugins
`logger`/`blacklist` (natifs) et `require-title` (wasm) validés en réel.
Contrats figés dans `Doc/plugin-{events,hooks,host}.md`.

---

## ⭐ TÂCHE D'ENTRÉE PROCHAINE SESSION

**Plugins — suite, au choix :**

1. **Config → guest** (reporté de WASM-1) : plumbing config d'un plugin wasm
   (`Manifest` config extism côté host + lecture `extism_pdk::config` côté
   guest), puis réécrire `blacklist` en `.wasm` configurable. **+ crate de
   types partagé** (`Candidate`/`PluginEvent`) pour ne plus les dupliquer entre
   host et guest (aujourd'hui recopiés dans `plugins/require-title-wasm`).
2. **A2 — surface hôte** (`Doc/plugin-host.md`, *host functions* extism) :
   `control` (Stop/Pause/Resume/StopWhenIdle, first-class station + invocable
   plugin), `push_override` (file lue par `next_media`, `soft` honoré /
   `hard`→LS), base par plugin. Puis le plugin `stop-when-idle` en démo
   (`on_event(ListenersSampled)` + `control`) — note : `ListenersSampled` n'aura
   de vraie source qu'avec Icecast, tester par injection.
3. **`on_scan`** : dernier hook non câblé (enrichissement au scan biblio).

Autres chantiers indépendants : câblage Liquidsoap (le vrai « ça diffuse »),
refacto acteur `GridEngine`, `DayPart` cross-minuit, historique de diffusion
(famille B) qui débloque `constraints`/`unplayed_only`/`limit`.

---

## Fait

### — filter_pool + runtime WASM (WASM-1) (2026-09-15) —

Le hook qui *influence* la décision, puis le premier plugin `.wasm` externe.
Validé en réel : `require-title` (wasm) retire du pool les fichiers sans titre,
round-trip host→wasm→host confirmé (JSON, titres préservés).

- **`filter_pool`** câblé dans `selection.rs` : le pool est **matérialisé**
  (`Vec<Candidate>` : rel_path/artist/title/album/year/duration/**genres**/mtime,
  genres via 2ᵉ requête) → passé aux plugins → puis choix (shuffle=`rand`,
  sequential/oldest=curseur, newest=tête). `resolve_ref_with_plugins` côté
  moteur, `resolve_ref` (plugins=None) côté tests. **Fail-closed (choix (b))** :
  un filtre qui vide un pool non-vide → `PoolEmpty` remonte (fallback grille),
  **+ warning** `tracing::warn` explicite avec le playlist responsable.
- **Acteur plugins** : message `FilterPool` synchrone (oneshot), chaînage par
  `order`, panique → pass-through + quarantaine. `Plugin::filter_pool` (défaut
  identité). `Candidate` + `PluginEvent` dérivent `Serialize`/`Deserialize`.
- **WASM** : `Cargo.toml` + `extism = "1"` (tire wasmtime 43, build lourd) +
  `serde_json`. `WasmPlugin` (host) : `Manifest::new([Wasm::file])` →
  `Plugin::new(&m, [], false)` → `call::<&str,String>(export, json)`.
  `extism::Plugin` est **Send+Sync** → vit dans l'acteur tokio sans thread
  dédié. Exports optionnels `filter_pool`/`on_event` (absent → pass-through /
  ignore), erreur de frontière → dégradé. `PluginDecl.wasm = Option<String>`
  (chemin) ; présent → wasm, absent → built-in par `name`.
- **Plugin natif `blacklist`** : `exclude_path_prefixes`/`exclude_artists`
  (via `filter_pool`), + test.
- **Crate guest** `plugins/require-title-wasm/` (SÉPARÉ, `[workspace]` vide,
  cible `wasm32-unknown-unknown`, extism-pdk) : exporte `filter_pool`. Struct
  `Candidate` DUPLIQUÉE de l'host (→ crate de types partagé = TODO).
- `.gitignore` : `target/` récursif (attrape le `target/` du crate wasm).

Non fait (reporté) : config→guest (`Manifest.with_config`), host functions
(A2), `on_scan`. Pas de test host du `WasmPlugin` (`cargo test` ne compile pas
le wasm) — validé en réel.

### — Système de plugins A1 : registre, cycle de vie, events (2026-09-15) —

Natif (pas de WASM), in-process. Contrats figés d'abord dans
`Doc/plugin-events.md` (le core notifie), `Doc/plugin-hooks.md` (le core
appelle + cycle de vie + statut), `Doc/plugin-host.md` (le plugin appelle —
pour A2). Validé en réel (`logger` chargé, `track resolved` loggé, stop/start).

- `src/plugin.rs` (7 tests) : trait `Plugin` (`on_load`/`on_unload`/`on_event` ;
  `on_scan`/`filter_pool` PAS encore câblés — hooks synchrones, design distinct
  de l'acteur). **Acteur possédant** (gabarit `library_actor`) : `spawn(decls)`
  → `PluginHandle` clonable. `emit` = fire-and-forget (`try_send`, ne bloque
  jamais la décision). États `Loaded`/`Disabled`/`Failed{phase,reason}`/
  `Quarantined{reason,failures}`. **Quarantaine** : fenêtre glissante, N=3
  échecs → hooks coupés jusqu'à `restart` explicite (jamais de retry auto).
  `catch_unwind` : un plugin qui panique ne fait pas tomber le core. Plugin
  `logger` (logge chaque event ; `fail_on_load` pour tester le chemin `Failed`).
- `PluginEvent` `#[non_exhaustive]` : A1 émet `TrackResolved` (seule source
  réelle). Un plugin ignore les variantes inconnues (`_ => {}`).
- `proto/plugin_v1.proto` + `src/plugin_grpc.rs` : `PluginService { List,
  Control{Start|Stop|Restart|Reload} }`. `reload == restart` en natif.
- `src/config.rs` : `[[plugin]]` (**clé singulier**, `#[serde(rename)]` ;
  champ Rust `plugins`) — `name`/`enabled`/`order` (défaut 50)/`config` opaque.
- `src/grid_engine.rs` : `with_plugins` + émission `TrackResolved` dans
  `next_media` (best-effort).
- `src/main.rs` : acteur spawné, branché au moteur, 5ᵉ service gRPC.
- `stationctl plugin list|start|stop|restart|reload`.

### — Étage sélection : playlist_ref → média concret (2026-09-15) —

Bout-en-bout **grille → média concret** enfin bouclé. `GridEngine::next_media`
enrichit la décision d'un `media_path` ; `schedule next` l'affiche. Validé en
réel : groupe `homestone-chronicles` → intro (un parmi N) / dernier épisode /
outro, sur des fichiers réels rangés en sous-dossiers.

- `src/selection.rs` (traducteur filtres pur + résolution DB, ~20 tests) :
  `resolve_ref(pool, ref)` → charge le TOML de la vue, parse, résout.
  - **Filtres dynamiques** → SQL paramétré, catalogue FERMÉ (path/title/artist/
    album/genre/year/duration) ; field/op/valeur hors catalogue = erreur
    bruyante. `match all/any`.
  - **shuffle** stateless (`ORDER BY random()`). **newest** sans `unplayed_only`
    = toujours le plus récent (tête, stateless). **sequential**/**oldest** =
    curseur de parcours qui avance et boucle.
  - **groupe `sequence`** : une piste par tour, `take` respecté, wrap = nouvelle
    activation ; membres feuilles résolus SANS récursion async (via
    `resolve_leaf`). `weighted`/`rotate`/imbriqués → erreurs explicites.
  - Non honorés : `unplayed_only`, `order_by=published` (loud errors) ;
    `constraints`, `limit` (tolérés, pas appliqués — famille B absente).
- `migrations/0008_playlist_cursor.sql` + `src/playlist_cursor.rs` : curseur
  (dernier rel_path rendu, robuste aux changements de pool). **Famille (B)**.
- `migrations/0009_group_state.sql` + `src/group_state.rs` : état de passage
  d'un groupe (member_idx, take_count). **Famille (B)**.
- `src/grid_engine.rs` : `next_media` + `ResolvedDecision`. Caveat documenté :
  `next()` persiste ses effets AVANT la sélection (chemin dev/CLI).
- `src/schedule_grpc.rs` : `resolve_next` remplit `media_path` ; mapping erreurs
  (ref inconnue → `failed_precondition`, non-supporté → `unimplemented`, valeur
  → `invalid_argument`). `stationctl schedule next` affiche `media:`.
- `src/store.rs` : `playlist_toml_by_ref`.

### — Slice gRPC + CLI de la biblio (2026-09-14) —

Le scan est atteignable au contrat (CLI-first), validé en réel (3 fichiers
Homestone : durée lue partout, tags là où ils existent, WAV sans tag → stocké
comme dégradé, pas d'erreur).

- `proto/library_v1.proto` : `LibraryService { Scan, ListMedia }`. `Skip`
  (path + Reason + detail) remonte les fichiers écartés ; `Media` = champs
  standard uniquement.
- `src/library_actor.rs` : **acteur possédant** — `spawn(pool, root) ->
  LibraryHandle` + `mpsc<Command>`. Scan lourd en `spawn_blocking` ; les scans
  sont sérialisés par la boucle mono-consommateur (garantie anti-concurrence
  *par construction*, pas de flag). C'est le **gabarit** du futur refacto
  `GridEngine`. 2 tests (dir vide, racine absente via le canal).
- `src/library_grpc.rs` : traducteur mince acteur↔proto. BadRoot →
  `failed_precondition`, reste → `internal`. Un skip est diagnostic → le scan
  sort en code 0 (contrairement à un apply rejeté).
- `build.rs`/`proto.rs`/`lib.rs`/`main.rs` : câblage (3ᵉ service sur le port).
- `stationctl library scan|list [--all]`.

### — Socle scan bibliothèque média (2026-09-14) —

Nouveau chantier « biblio », côté données uniquement (gRPC/CLI = slice suivant).
Table `media` = **famille (A)** reconstructible ; l'historique (B) référencera
par identité (rel_path + garde-fou), jamais par FK.

- `Cargo.toml` : + `lofty` 0.24 (tags + durée, pur-Rust → build statique/cross OK).
- `migrations/0007_media_library.sql` : `media` (rel_path clé, **casse
  conservée**, `duration_ms > 0`, garde-fou `size_bytes`+`mtime_ns`, `available`)
  + `media_genre` (genre = ensemble). Index `available`/`artist`.
- `src/media.rs` (**pur**, walkdir + lofty, ni sqlx ni tokio ; 5 tests dont un
  WAV minimal généré à la volée) : `scan_library(root) -> ScanReport { media,
  skipped }`. No-silent-failure : durée nulle / fichier illisible → `ScanSkip`
  remonté, jamais avalé ; extension non-audio simplement ignorée (pas une
  erreur). Racine absente = seule erreur dure. À envelopper dans `spawn_blocking`.
- `src/media_index.rs` (persistance famille A ; 4 tests DB migrée) :
  `replace_library` = réconciliation en **une transaction** (tout `available=0`
  puis ré-affirme les vus) — un disparu reste connu, marqué indisponible (pas de
  DROP). `list(only_available)`. Remplacement explicite du set de genres.
- `src/lib.rs` : `media` + `media_index` exposés.

⚠ **Anomalie relevée** : `migrations/0004_playlist_materialized_view.sql` est
ABSENT du dossier (0001-0003, 0005-0006 seulement) alors qu'ETAT le décrit
(vue riche playlists + famille B `episode_play`/`broadcast_log`/
`playlist_suspension`). Ces tables **n'existent donc pas** en base : bloquant
pour `unplayed_only`/historique le jour venu. À retrancher (recréer 0004 ou
renuméroter). N'impacte pas le scan média (tables neuves en 0007).

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
- `src/bin/stationctl.rs` : sous-commandes `schedule validate|apply|export`
  (pré-check TOML côté client, `GridFile` envoyé au même endpoint, sortie
  non-zéro sur rejet). Boucle CLI complète, invariant CLI-first tenu.
- `grid.toml` (racine) : exemple des 4 familles, calé sur les refs de la vue.
- **Validé en vrai** : `validate`/`apply` (4 règles) → `list` → `next`
  (`AT_CLOCK_SOFT` à 00:00, ordre de collision correct) → `export` (round-trip
  stable, `enabled=true` omis). `cargo build` + `cargo test -p stationd` verts.

### — Preview : projection de la grille (2026-09-14) —

- `GridEngine::preview(from, window_secs)` (2 tests) : balayage minute par
  minute, une `PreviewOccurrence` à **chaque changement** de décision. Marks
  `AtClock` consommés au fil de l'eau (comme la boucle live) → un repère est un
  **instant**, pas un segment. **`Every` exclu** (cadence pilotée par la
  lecture, non projetable sur l'horloge). Fenêtre bornée (≤ 31 j), rendu local
  via `clock` (DST réel).
- `src/schedule_grpc.rs` : `preview` réel (occurrences `at_utc` + `at_local`
  nommé). **Les 6 RPC sont désormais réels ; plus aucun `UNIMPLEMENTED`.**
- `src/bin/stationctl.rs` : `schedule preview [--at <epoch>] [--window <secs>]`
  (défaut 24 h), colonne UTC + local.
- **Validé en vrai** : projection 24 h (alternance repère `:00/:30` → plancher)
  et test anti-DST 2026-10-25 (02:30 rejoué, epochs distincts).

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

#### Protos & câblage gRPC (**les 6 RPC réels**)
- `proto/schedule_v1.proto` : `ScheduleService` (grille + `ResolveNext` +
  `Preview`), 4 familles, `soft|hard`, péremption, portée de validité.
  Compilé par `build.rs` (+ `prost-types`).
- `src/schedule_grpc.rs` : handler mince sur `GridEngine`. `resolve_next`,
  `list_rules`, `apply_grid`, `validate_grid`, `export_grid`, `preview` **tous
  réels** — plus aucun `UNIMPLEMENTED`.
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
- **Refacto acteur** : `GridEngine` en tâche tokio possédante, grille en
  mémoire invalidée à l'apply, mutations par mpsc (gabarit `library_actor`).
- **`DayPart` cross-minuit** : `window_covers` renvoie `None` (TODO signalé).
- **Sélection — suite** : anti-répétition (`constraints`) + `unplayed_only`
  (réclament l'historique famille B), `limit`/quota par activation, groupes
  `weighted`/`rotate`/imbriqués, `queue`/`remote`.
- **Refs de membres relatives au dossier du groupe** : `normalize_ref` ne
  résout pas un `ref` de membre relativement à l'emplacement du groupe — il
  faut aujourd'hui le chemin complet
  (`homestone-chronicles/homestone-chronicles-intro`). À trancher : résolution
  relative ou refs toujours absolues.

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

- **Base de dev à recréer** : migrations `0005`→`0009` sont neuves. En cas de
  souci de schéma/checksum sqlx, `rm -rf data/` + relancer (file-first, la vue
  est jetable). Ne JAMAIS éditer une migration déjà appliquée en prod.
- **`0004` manquant** : le fichier de migration de la vue riche playlists est
  absent du dossier (cf. Fait). `unplayed_only`/historique n'ont donc pas leur
  socle SQL tant que ce n'est pas retranché.
- **`lofty` ajouté** : premier `cargo build` doit actualiser `Cargo.lock`
  (Rust indispo dans l'env de préparation).
- **Refs de membres = chemin complet** : un `ref` de membre de groupe doit être
  le rel_path complet (`homestone-chronicles/homestone-chronicles-intro`), pas
  relatif au dossier du groupe. Un ref court résout une clé absente → PoolEmpty
  ou PlaylistNotFound. (cf. Reste à faire.)
- **Casse des chemins** : `media.rel_path` CONSERVE la casse (fichiers réels,
  FS potentiellement sensible à la casse) ; les refs playlists sont, elles,
  normalisées en minuscules. Ne pas traiter les deux pareil.
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
| `src/grid_engine.rs` | Boucle vivante (résolution + persistance) + `preview` (projection) |
| `src/schedule_grpc.rs` | Service gRPC scheduling (6 RPC réels, `preview` compris) |
| `src/config.rs` | Config TOML + fuseau station validé |
| `src/playlist.rs` | Modèle/parser/validation playlists |
| `src/store.rs` / `src/sync.rs` | Vue playlists / réconciliation |
| `src/media.rs` | Scan biblio **pur** (walkdir + lofty → `ScanReport`, 5 tests) |
| `src/media_index.rs` | Vue média famille (A) : `replace_library` (réconciliation) / `list` (4 tests) |
| `src/selection.rs` | Étage sélection `playlist_ref`→média : filtres, ordres, curseur, groupe sequence (~20 tests) |
| `src/playlist_cursor.rs` | Curseur de parcours famille (B) : dernier média rendu |
| `src/group_state.rs` | État de passage d'un groupe sequence famille (B) |
| `src/library_actor.rs` | Acteur biblio possédant (mpsc, spawn_blocking) — gabarit refacto |
| `src/library_grpc.rs` | Transport gRPC biblio (traducteur mince acteur↔proto) |
| `src/plugin.rs` | Système de plugins : trait, acteur à état, quarantaine, `filter_pool`, `WasmPlugin` (extism), natifs logger/blacklist |
| `src/plugin_grpc.rs` | Transport gRPC plugins (list + control) |
| `plugins/require-title-wasm/` | Crate guest WASM de démo (séparé, cible wasm32) : exporte `filter_pool` |
| `src/grpc.rs` | Service `Station` (status/quit/playlist*) |
| `src/db.rs` | Init pool SQLite + migrations |
| `src/main.rs` | Daemon : démarrage, 5 services gRPC, shutdown |
| `src/bin/stationctl.rs` | CLI (station + `schedule …` + `library …` + `plugin …`) |
| `proto/station.proto` | Contrat `Station` |
| `proto/schedule_v1.proto` | Contrat `ScheduleService` (compilé/servi ; 6 RPC réels) |
| `proto/library_v1.proto` | Contrat `LibraryService` (compilé/servi ; Scan + ListMedia) |
| `proto/plugin_v1.proto` | Contrat `PluginService` (compilé/servi ; List + Control) |
| `Doc/plugin-{events,hooks,host}.md` | Contrats du système de plugins (référence durable) |
| `Doc/proposition-grammaire-grille-v1.md` | Contrat grammaire `grid.toml` (référence durable) |
| `proto/playlist_v1.proto` | Contrat playlist v1 (⚠ pas encore compilé/servi) |
| `migrations/0001→0009` | Schéma (0005 état grille, 0006 règles, 0007 biblio, 0008 curseur, 0009 groupe ; ⚠ 0004 absent) |
| `tests/sync.rs` | Intégration `sync` |
| `Doc/*.md` | Décisions d'architecture (référence durable) |
