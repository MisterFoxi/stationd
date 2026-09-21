# État d'avancement — passation entre sessions

Journal de ce qui est **fait** et ce qui **reste**, pour reprendre le travail
sans reconstruire le contexte. À distinguer des docs de `Doc/` (décisions
d'architecture durables) : ce fichier-ci est volatil, à mettre à jour à
chaque session.

Dernière mise à jour : 2026-09-21.


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
grammaire + les 6 RPC réels. `cargo build` + `cargo test -p stationd` étaient verts
au dernier build compilé ; les ajouts récents (shuffle/runtime de groupe,
décomposition preview, fix casse de ref) **attendent un `cargo build`** (Rust
indispo dans l'env de préparation).
Biblio média scannée et pilotable au CLI (`library scan|list`). **Étage
sélection livré** : `playlist_ref` → `media_path` concret (shuffle, sequential,
newest/oldest via curseur, groupe `sequence`/`shuffle` + quota `take`/`runtime`,
membres décomposés au `preview`). Validé en réel de bout en bout :
grille → groupe intro / épisode le plus récent / outro → fichiers réels.
**Système de plugins** : A1 (registre, cycle de vie, `on_event`) + **hook
`filter_pool`** (le pool est matérialisé puis filtré par les plugins avant le
choix) + **runtime WASM (WASM-1)** — un `.wasm` externe (extism) implémente le
trait via JSON. `stationctl plugin list|start|stop|restart|reload`, plugins
`logger`/`blacklist` (natifs) et `require-title` (wasm) validés en réel.
Contrats figés dans `Doc/plugin-{events,hooks,host}.md`.
**Fallthrough grille** : une source au pool vide retombe sur la priorité
inférieure jusqu'au plancher (fini le dead-air par sélection vide) ; groupe
`sequence` avec `on_member_unavailable = abort|skip`. **Config→guest wasm** :
`[plugin.config]` traverse jusqu'au `.wasm` (plugin `blacklist` wasm configurable).

---

## ⭐ TÂCHE D'ENTRÉE PROCHAINE SESSION

**Preview — statistiques de pool : patch préparé et testé (2026-09-19).**
Voir la section Fait ci-dessous et `Doc/preview-pools.md`.

**Plugins A2 — surface hôte** (`Doc/plugin-host.md`, *host functions* extism) :
`control` (Stop/Pause/Resume/StopWhenIdle, first-class station + invocable
plugin), `push_override` (file lue par `next_media`, `soft` honoré / `hard`→LS),
base par plugin. Puis le plugin `stop-when-idle` en démo (`on_event(
ListenersSampled)` + `control`) — `ListenersSampled` n'aura de vraie source
qu'avec Icecast, tester par injection.

Autres, indépendants :
- **`on_scan`** : dernier hook non câblé (enrichissement au scan biblio).
- **Crate de types partagé** `Candidate`/`PluginEvent` (host + guests wasm ne
  les dupliquent plus — aujourd'hui recopiés dans les 2 crates guest).
- **`DayPart` cross-minuit** : `window_covers` renvoie `None` (TODO).
- **Refacto acteur `GridEngine`** (gabarit `library_actor`).
- **Câblage Liquidsoap** (le vrai « ça diffuse » ; débloque aussi `hard`).
- **Refs de membres relatives** au dossier du groupe (chemin complet requis).

---

## A modifier/Changer
## Preview / validation de la grille avant diffusion

La grille est appliquée en avance (cron chargeant un TOML à la demande) et les
PL dynamiques sont résolues à la lecture. Pour valider une grille *avant*
qu'elle passe à l'antenne, on produit un preview autoritatif.

### Gel = preview autoritatif, même résolveur

- Le `preview` n'est pas une photo indicative : c'est le résolveur
  (`resolve_next`, quasi-pur) exécuté sur **horloge virtuelle**, qui produit le
  playout complet du segment. **Un seul chemin de code** — jamais une simulation
  parallèle, sinon la garantie devient fausse dès la première divergence.
- La sim balaie **tout le jeu de règles de la station** (base, DayPart,
  AtClock, Every, one-shots, interrupts), pas seulement le segment regardé —
  sinon l'anti-répétition simulée ≠ celle diffusée.
- Le résultat est un **artefact de validation** (un log), pas un état persisté
  qui rejoue. On regarde → ça plaît, on commit la grille ; sinon on retravaille.
- **Le dynamisme est préservé** : la requête reste la source, ré-évaluée à
  chaque activation. Le gel ne fige pas la *nature* de la PL, il cristallise
  *une* résolution pour l'inspecter. En streaming pur, les causes externes sont
  marginales → le dry-run est fidèle à ce qui sortira.
- Exception `queue` (on-demand) : tampon vide au moment du bake, rempli au
  runtime → non simulable, exclu par nature.

### Ce qui est dur vs estimé

- **Dur** : l'ensemble des médias, l'ordre résolu (permutation du shuffle,
  résultat des tirages pondérés), les départs ancrés-règle (`scheduled`,
  `AtClock`).
- **Estimé** : le wall-clock **par piste** (cumul des durées) — durée réelle,
  cooldowns contre l'historique, interrupts intercalés sont des faits de
  runtime. Cohérent avec les bornes molles.
- Le preview promet donc « exactement ces médias, dans cet ordre, dans ce bloc
  qui démarre à cette heure » — pas « ce titre à 20:03:47 ». Ne pas survendre le
  calage seconde dans l'UI.

### Preview log multi-granularité

Trois profondeurs d'un même bake, via flags CLI (modèle `git log --oneline /
--stat / full`) :

1. **Tranche horaire** (`--summary`) : index scannable, une colonne DIAG qui
   indique *où* déplier avant de lire le détail.
2. **Bloc** (défaut) : un bloc = une activation. Porte la règle et ses params,
   **pourquoi il gagne** (résolution de collision), et le diagnostic au bon
   grain (un trou apparaît comme bloc fantôme, pas noyé dans les pistes).
3. **Médias** (`--tracks` / `-v`) : piste concrète + **colonne raison**
   (`[newest]`, `[shuffle #7]`, `SKIP … contrainte`, `fallback`) — transforme
   « ce qui sort » en « pourquoi ça sort, et où le quota a cédé ».

- Les secondes ne s'affichent qu'au niveau 3, marquées estimées (`~`).
- Même structure en `--format json` (arbre tranche→bloc→piste, diagnostics par
  nœud) pour un futur rendu web. CLI-first : le log est produit par le contrat
  gRPC, l'affichage n'est qu'un rendu.

### Diagnostic de couverture inter-blocs

- Départs implicites (B1@20:00, B2@21:30). La **fin de B1 est une borne
  dérivée** (= le `start` de B2), calculée au preview, **jamais un champ TOML**.
- `end` reste **interdit en entrée**, raison inchangée : composer une liste de
  médias dont la somme des durées tombe pile sur une cible est impossible
  (packing exact + durées = runtime). Un `end` normatif est une promesse
  mensongère.
- Ce qu'on rapporte est l'**inverse** — observationnel, en sortie. Trois états,
  tous dérivables sur horloge virtuelle :
  - **débord** (contenu > fenêtre implicite) — `⚠`, souvent voulu (borne molle) ;
  - **sous-couverture / trou** (contenu < fenêtre, rien pour combler) — `✗` si
    trou sec sans fallback ;
  - **OK** (tient, avec la marge affichée).
- **Signalé, pas jugé** : le preview rend le fait visible, l'humain tranche.

### Encore ouvert (à fusionner dans « Encore ouvert »)

- Liste **fermée** des codes de diagnostic (doit exister au contrat gRPC pour
  être atteignable CLI + web) : trou d'antenne, pool vide, média/ref cassée,
  cycle de groupe, cooldown non satisfait, épuisement (`on_exhausted`),
  collision surprenante, débord inter-blocs, sous-couverture, anomalie DST.
- Politique **bloquant vs non-bloquant** : quels codes empêchent le commit de la
  grille, lesquels ne sont qu'un avertissement.

## Fait

### — Groupes `weighted` + `rotate` : résolution (2026-09-21) —

Point 3 du todo « B » (débloquer weighted/rotate ; imbriqués = sous-pas suivant).
Le modèle (`Strategy::Weighted`/`Rotate`) et la validation (`weight` en weighted
seul, `take`/`runtime` en sequence/shuffle seul) existaient déjà ; seul
`selection.rs::resolve_media` renvoyait `Unsupported` sur ces deux stratégies.

- **`rotate`** → routé vers `resolve_group_rotation(shuffle = false)`. Ses
  membres sont forcément nus (validation), donc la marche sequence avec `take`
  défaut = 1 EST un round-robin ; position persistée via `group_state`. Aucune
  mécanique neuve.
- **`weighted`** → `resolve_group_weighted` (neuve, sans état persisté, tirages
  indépendants) : éligibles = `weight > 0`, tirage `SliceRandom::choose_weighted`,
  puis un média du membre tiré. Poids par défaut **15** (`DEFAULT_WEIGHT`, milieu
  de la plage 0-50 de la proposition), `0` = exclu du tirage (pas un disable
  global). Membre tiré vide → `on_member_unavailable` : `skip` re-tire sans lui
  (borné à N membres), `abort` (défaut) remonte `PoolEmpty` → fallthrough grille.
  Membres feuilles uniquement (nested → `Unsupported` via `resolve_member`).
- `match sel.strategy` désormais **exhaustif** (les 4 stratégies + `None`), plus
  de bras fourre-tout.
- Tests : 5 ajoutés (`group_weighted_draws_from_its_members`,
  `_excludes_zero_weight`, `_skip_redraws_past_an_empty_member`,
  `_abort_bubbles_when_drawn_member_is_empty`,
  `group_rotate_round_robins_one_per_member`) ; écrits déterministes malgré le
  hasard (exclusion poids 0, skip vers l'unique membre non vide). Ancien
  `group_mode_weighted_is_rejected_for_now` supprimé.
- Décisions de sémantique assumées : défaut de poids = 15 ; borne [0,50] **non**
  validée ici (scope validation, séparé) ; `0` exclut.

Non fait (sous-pas restant) : **groupes imbriqués** (membre = groupe) — encore
`Unsupported` ; la détection de cycle existe déjà côté `validate_set`.

### — Preview : nombre de médias et durée du pool par occurrence (2026-09-19) —

Patch basé sur `dev` / `42a5231` :
- `pool_inspection::inspect_ref` réutilise `materialize_dynamic`/`materialize_static`
  et somme les durées en millisecondes ; aucun choix de piste, curseur,
  `group_state` ou plugin `filter_pool`.
- `dynamic`/`static` : totalité du pool disponible, zéro explicite si vide.
  Groupes : par membre et somme, y compris weighted/rotate sans quota inventé.
  Un média partagé compte une fois par membre ; aucun plafonnement take/runtime.
- `remote`/`queue` : count inconnu ; **runtime du membre repris comme durée si
  défini**, sinon durée inconnue. Count et durée ont des présences indépendantes.
- Contrat additif `Occurrence`/`GroupMember` : `selected_count` optionnel et
  `total_duration`. CLI : `pool: N media, duration HH:MM:SS[.mmm]`, `unknown`
  explicite. Timeline, offsets et every indicatifs conservés.
- Cache limité à la requête, index actuel (pas de prédiction de scans futurs).
  Références absentes, filtres invalides et groupes imbriqués : erreurs explicites.
- **Validation** : `cargo test --locked` : **169 tests passent** ;
  `cargo build --locked --features tui` : **OK**. Les tests nouveaux vérifient
  notamment une connexion SQLite en lecture seule et un plugin actif qui
  viderait le pool, ainsi que les durées remote/queue définies.
- `cargo test --locked --features tui` reste bloqué par **5 erreurs préexistantes**
  dans les tests de `src/bin/stationd-tui/playlist_form.rs` : accès à l'ancien
  `Broadcast` (`limit`, `every_tracks`, `weight`, `schedule`). Hors de ce patch.


### — Fix : ref de grille sensible à la casse à la résolution (2026-09-19) —

Bug : une règle de grille `playlist_ref = "Filler"` (majuscule) n'était pas
prise en compte (source jamais résolue → erreur `PlaylistNotFound` sur son
créneau ; membres non décomposés au preview), alors que
`homestone-chronicles/…` (déjà minuscule) marchait.

Cause : la vue est clé­e en minuscules (`sync` stocke `normalize_ref(...)`),
mais `store::playlist_toml_by_ref` matchait la ref **brute**
(`WHERE rel_path = ?`, sensible à la casse). Deux chemins de lookup touchés :
la résolution à l'antenne (`resolve_inner`) ET la décomposition preview
(`grid_engine::group_projection`, qui lit le store en direct). L'`apply` de la grille, lui,
normalise pour vérifier l'existence → il acceptait `"Filler"`, d'où le décalage
apply-OK / résolution-KO. Les refs de **membres**, déjà normalisées, n'étaient
pas touchées (seule la ref top-level l'était).

Fix (2 passes — la 1ʳᵉ, sur `resolve_inner` seul, ratait le preview) : la
normalisation vit désormais dans le **point de choke unique**
`store::playlist_toml_by_ref` → tous les lookups (antenne, membres,
décomposition preview) sont insensibles à la casse. `resolve_inner` normalise
aussi pour propager la clé canonique en aval (curseur / group_state ne se
dédoublent plus selon la casse). Tests régression :
`store::lookup_by_ref_is_case_insensitive` +
`selection::resolves_a_top_level_ref_case_insensitively`. Contournement sans
patch : écrire la ref en minuscule dans le `grid.toml`.

### — Preview : décomposition des groupes (membres + offsets runtime) (2026-09-19) —

`stationctl schedule preview` affiche désormais, sous une occurrence dont le
`playlist_ref` est un groupe `sequence`/`shuffle`, la liste de ses membres. Le
preview reste une **projection horloge pure** (durée des pistes inconnue) :

- membre `take` (per-track) → listé **sans TS** (impossible : N pistes de durée
  inconnue) ;
- membre `runtime` d'un `sequence` → **offset relatif** au début du groupe
  (`+0`, `+20m`, …), tant que ses prédécesseurs sont tous `runtime` ; un `take`
  intercalé rompt la chaîne (offsets suivants absents) ;
- `shuffle` → **budgets seulement**, aucun offset (ordre tiré au runtime) ; le
  groupe est marqué `(shuffle)`.

Chemin CLI-first : décomposition calculée côté moteur, exposée au contrat.
- `playlist.rs` : `Playlist::project_group_members() -> Option<GroupProjection>`
  (pur : `Strategy` + `Vec<ProjectedMember { ref, MemberQuota::Take|Runtime,
  offset_secs }>`), `None` hors groupe sequence/shuffle. 5 tests.
- `schedule_v1.proto` : `Occurrence` += `strategy` + `repeated GroupMember`
  (`ref`, oneof `take`|`runtime`, `offset`). Ajout **additif** — la timeline
  `occurrences` est inchangée (TUI agenda non impacté).
- `grid_engine.rs` : `PreviewOccurrence.group`, décomposition mémoïsée par ref
  dans la marche du preview (helper `group_projection`). 1 test.
- `schedule_grpc.rs` : mapping projection → proto.
- `stationctl.rs` : affichage arborescent (`├`/`└`) sous la ligne d'occurrence.

Rust indispo ici → **compilation/tests + regen proto (`build.rs`) à confirmer au
prochain build.** Groupes `weighted`/`rotate` non décomposés (pas de timeline
take/runtime) — à ajouter si besoin.

### — Rotation de groupe : `shuffle` + budget `runtime` par membre (2026-09-18) —

Point 5 du triage bug. **FAIT** (édité ; **compilation/tests à confirmer au
prochain build** — Rust indispo dans l'env de préparation).

Décision de sémantique (tranchée avant code) : concept **playlist** (quota par
membre de groupe), pas grille. Budget = **temps mural écoulé** depuis le début
du membre courant (catch-up : périme tout seul après un downtime > budget),
**soft** (la dernière piste déborde, bascule au bord suivant — comme
`DayPart.end`). Aucun timer mural, aucun `sleep`.

- **Nouvelle stratégie `shuffle`** (`playlist.rs` `Strategy::Shuffle`) : mêmes
  membres qu'une `sequence`, parcourus en **permutation** (chaque membre une
  fois par cycle), re-tirée à chaque cycle. Permutation **persistée** → un
  redémarrage en milieu de cycle ne rebat pas les cartes.
- **Quota par membre** : `take` (N pistes, existant) **XOR** `runtime` (durée
  `"20m"`, neuf) — les deux sur le même membre = erreur de parse. Autorisés sur
  `sequence` ET `shuffle`. `runtime` parsé via `parse_duration_secs` (même
  grammaire `[1-9][0-9]*(s|m|h|d)` que la grille ; **doublon assumé** dans
  `playlist.rs`, comme `parse_date`).
- **`selection.rs`** : `resolve_group_sequence` → **`resolve_group_rotation(…,
  shuffle: bool)`** (sequence et shuffle partagent tout sauf l'ordre). Budget
  vérifié **avant** l'emit (bascule soft) ; `runtime` stampe le départ du membre
  sur sa 1ʳᵉ piste, avance quand `now - started ≥ budget`. La trace TEMP de
  l'ancienne fonction disparaît avec le renommage.
- **Threading de `now`** : le budget a besoin de l'**horloge contrôlable** (pas
  d'un `SystemTime::now()` local), sinon il échappe à `schedule next --at` /
  preview / tests. `resolve_ref_with_plugins` prend un `now: i64` (epoch s),
  passé par `grid_engine::next_media` (`now.0`) — unique call-site vivant.
  `resolve_ref_at(pool, now, ref)` neuf pour les tests ; `resolve_ref` garde
  l'horloge murale.
- **`migrations/0010_group_runtime_shuffle.sql`** : `group_state` +=
  `member_started_at INTEGER` (budget) + `permutation TEXT` (indices CSV,
  shuffle). Nullables → lignes existantes valides. **Famille (B)**.
  `src/group_state.rs` réécrit : `get`/`set` sur une struct `GroupState`
  (member_idx, take_count, member_started_at, permutation).
- Tests ajoutés : `playlist.rs` (shuffle+runtime valides, XOR, runtime hors
  quota-group, mauvaise durée, grammaire) ; `selection.rs`
  (`group_shuffle_visits_each_member_once_per_cycle`,
  `group_sequence_runtime_budget_switches_after_elapsed`,
  `group_shuffle_runtime_holds_a_member_then_moves_on`) ; `group_state.rs`
  (roundtrips take / runtime+permutation / CSV).

### — Preview des `every` : projection elapsed + indicatif tracks (2026-09-18) —

Point 2 du triage bug. Le `preview` ne jette plus les `every`.

- **`elapsed` projeté** dans la timeline : réinjecté dans la marche
  minute-par-minute de `grid_engine::preview` (semé `last_played = from` → 1er
  repère à `from + cadence`, `last_played` avancé à chaque tir). Ponctue comme
  un `AtClock` (instant, pas segment) ; priorité `AtClock > Every > base`
  conservée (passe par `resolve_next`). Granularité minute assumée.
- **`tracks` indicatif** : non projetable sur l'horloge → renvoyé à part.
  `grid_engine::preview` renvoie `GridPreview { occurrences, indicative }` ;
  `occurrences` reste une timeline pure et monotone (le TUI agenda en dépend),
  `indicative` = une entrée par règle `every` au compteur (`IndicativeRule`).
- **Contrat** `proto/schedule_v1.proto` : `Occurrence` inchangé, nouveau message
  `IndicativeRule`, `PreviewResponse { occurrences, indicative }`.
- **CLI** `stationctl schedule preview` : plancher/`day_part` affichés une seule
  fois par segment réel — les reprises après une marque ne sont plus
  réimprimées (lisibilité) ; `indicative` listé une fois en fin de sortie.
- **TUI agenda** : pied de page liste les `every` au compteur par nom de
  playlist (avant : comptait *tous* les `every` en disant « not projected »,
  faux depuis que les `elapsed` sont projetés). `entries()`/`occurrences`
  inchangés (le pied de page dérive de `ListRules`).
- Tests verts : `grid_engine` (`preview_projects_an_elapsed_every_at_its_cadence`
  neuf ; `preview_projects_base_daypart_and_marks` et
  `preview_of_a_bare_floor_is_a_single_segment` adaptés à `GridPreview`) ;
  `tests/agenda_preview.rs` (`occurrences` monotone sans `Every`, `indicative`
  vérifié). `--features tui` : littéraux `PreviewResponse` complétés.

### — REPRISE (bug LIFO en cours-> non reproductible) + contrat broadcast + clock (2026-09-16) —

**À FAIRE EN PREMIER À LA REPRISE :**
- `cargo test -p stationd` (reconfirmer le vert après les derniers edits store.rs/tests/sync.rs).
- **Retirer la trace TEMP** dans `selection.rs` `resolve_group_sequence` :
  `tracing::info!(... "group sequence pick")` (posée pour diagnostiquer le bug 1).
  **Fait (2026-09-18)** — fonction renommée `resolve_group_rotation`, trace supprimée.
- Nettoyer les `.toml` de playlists sur disque (voir Bug ci-dessous) : retirer
  `type`/`weight`/`[[broadcast.schedule]]`/`every_*` ; typo `ClassicFM.toml`
  `mode = "remore"` → `"remote"`.

**Bug.txt — triage (5 points) :**
1. **LIFO groupe** (membres joués dernier→premier, visible en `schedule next`).
   **Résolu / non reproductible (2026-09-18).** Clos.
2. **preview des `every`** : **FAIT (2026-09-18, cf. section Fait).** `elapsed`
   projeté dans `occurrences` à sa cadence ; `tracks` renvoyé à part dans
   `PreviewResponse.indicative` (jamais dans la timeline), listé une fois.
3. **`expiry="30m"`** : valide UNIQUEMENT sur `at_clock` (péremption d'un top,
   déjà supporté). La « fin fixe » voulue = point 5.
4. **`weight` en grid** : non supporté par design (grille = priorité). Pondération
   = playlist `group strategy="weighted"`.
5. **Rotation à budget de temps** : **FAIT (2026-09-18, cf. section Fait).**
   Nouvelle stratégie `shuffle` (permutation persistée) + quota par membre
   `take` XOR `runtime` (temps mural écoulé, soft, catch-up), sur `sequence` et
   `shuffle`. Migration 0010 (`member_started_at`/`permutation`).

### — Fallthrough grille + on_member_unavailable + contrat broadcast + choisi+flip + clock (2026-09-15/16) —

- **Fallthrough grille** (`grid_engine::next_media`) : source au pool vide →
  retombe sur la priorité inférieure jusqu'au plancher ; effets AtClock/Every
  persistés seulement quand une source produit. `resolver::resolve_ranked`
  (sources classées) + `resolve_next` = son premier (pur, 13+1 tests).
- **`on_member_unavailable = "abort"(défaut)|"skip"`** sur groupe `sequence`
  (`playlist.rs` + `selection.rs`).
- **Contrat `broadcast` playlist refondu** : politique de consommation seule
  (`limit`/`repeat`/`on_exhausted`/`constraints`), **optionnel** ;
  `type`/`weight`/`schedule`/`every_*` → **erreur de parse** (deny_unknown_fields).
  L'ordonnancement vit UNIQUEMENT dans `grid.toml`. Fixtures de tous les fichiers
  de test nettoyées (playlist/selection/grid_engine/store/tests-sync).
- **choisi+flip** : `next_media` vérifie l'existence disque du média choisi
  (`with_media_root`, câblé dans main) ; absent → `media_index::mark_unavailable`
  + re-pick borné (32) sur la même source, sinon fallthrough. Off en tests
  (media_root None).
- **Horloge manuelle** : `GridEngine` override + `effective_now` + `set_clock_civil`
  ("HH:MM" = aujourd'hui / "YYYY-MM-DD HH:MM") ; `clock::civil_to_epoch` ;
  RPC `SetClock` ; `stationctl clock set|show|reset`.

### — Fallthrough grille + on_member_unavailable + config→guest wasm (2026-09-15) —

Règle le dead-air : une source qui ne produit pas retombe sur la priorité
inférieure jusqu'au plancher, au lieu de remonter une erreur.

- `resolver.rs` : `resolve_ranked(now, grid, state) -> Vec<GridDecision>`
  (sources classées par priorité) ; `resolve_next` = premier de la liste
  (sémantique inchangée, 13 tests + 1 d'ordre). Reste **pur**.
- `grid_engine.rs` : `next_media` essaie les sources classées, saute un
  `PoolEmpty` et retombe jusqu'au plancher ; effets (mark AtClock / reset Every)
  persistés **seulement quand une source produit** (corrige l'ancien caveat
  « persiste avant sélection »). Erreur seulement si tout — plancher compris —
  est vide ; `Fallback` (média None) si aucune règle ne couvre `now`.
  `persist_effects`/`emit_resolved` factorisés. + test fallthrough.
- `playlist.rs` : `on_member_unavailable = "abort"` (défaut) | `"skip"`.
- `selection.rs` : groupe `sequence` — `skip` saute le membre vide et continue,
  `abort` fait échouer le groupe (→ fallthrough grille). + 2 tests.
- **Config→guest wasm** : `WasmPlugin::new` injecte `[plugin.config]` (JSON) dans
  le `Manifest` extism (`with_config(...).into_iter()`) ; guest lit
  `config::get("config")`. Crate guest `plugins/blacklist-wasm/` (blacklist wasm
  configurable). Validé en réel (le filtrage suit la config).

**Conséquence pour la grille** : une vraie grille doit avoir un **plancher
musical distinct** (`base_rotation` sur une rotation musique), l'émission en
`day_part`/`scheduled` au-dessus — sinon « le plancher EST le groupe » et il n'y
a rien sous quoi retomber.

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

ℹ **Incident 0004 — clos (2026-09-21).** Le fichier
`migrations/0004_playlist_materialized_view.sql` a été effacé par erreur.
Décision : baseline = schéma courant (0001-0003 + 0005-0010), **0004 est
volontairement nul et non avenu** — un trou de numérotation ne gêne pas sqlx
(tri par version, aucune contiguïté requise ; `rm -rf data/` rejoue proprement).
Rien à reconstruire. Le socle famille B (`episode_play`/`broadcast_log`/
`playlist_suspension`) et la vue éclatée n'ont jamais existé en base ; ils
seront (re)créés dans une **future migration** (p.ex. 0011) le jour où
`unplayed_only`/historique arriveront — pas en ressuscitant 0004. N'impacte ni
le scan média ni l'étage sélection (qui lit le TOML brut).

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
  **imbriqués** (weighted/rotate faits — cf. Fait 2026-09-21), `queue`/`remote`.
- **Refs de membres relatives au dossier du groupe** : `normalize_ref` ne
  résout pas un `ref` de membre relativement à l'emplacement du groupe — il
  faut aujourd'hui le chemin complet
  (`homestone-chronicles/homestone-chronicles-intro`). À trancher : résolution
  relative ou refs toujours absolues.

### Playlists (périmètre existant)
- Compiler+servir `playlist_v1.proto` (nouveau contrat) et migrer le code
  (`store`/`playlist`/`sync`) vers l'identité `name`/`handle` → alors seulement
  reposer l'index UNIQUE `ref_effective`.
- Vraie vue matérialisée **peuplée** (l'apply qui éclate le TOML dans des tables
  de détail) + socle famille B (`episode_play`/`broadcast_log`/
  `playlist_suspension`) → **future migration** (0004 étant nul). CRUD
  `remove`/`export`, reload/watch. Rapport de cycle exact (Tarjan). Points
  ouverts `Doc/playlists.md`.

### Hors périmètre (chantiers suivants)
- Câblage Liquidsoap/Icecast (`request.dynamic` + fallback). Scan biblio
  (tags). Rôles/permissions (différés). Plugins WASM, mTLS gRPC (reportés).
- Maintenance : `apalis` (scan, retry NFS) — `Doc/modele-programmation.md`.

---

## Pièges & points de vigilance

- **Base de dev à recréer** : migrations `0005`→`0010` sont neuves. En cas de
  souci de schéma/checksum sqlx, `rm -rf data/` + relancer (file-first, la vue
  est jetable). Ne JAMAIS éditer une migration déjà appliquée en prod.
- **`0004` volontairement nul (incident clos, 2026-09-21)** : fichier effacé
  par erreur, non reconstruit — baseline = schéma courant. Le socle SQL de
  `unplayed_only`/historique (tables famille B) viendra dans une future
  migration, pas dans un 0004 ressuscité. Un trou de numérotation ne gêne pas
  sqlx.
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
| `src/selection.rs` | Étage sélection `playlist_ref`→média : filtres, ordres, curseur, groupe sequence/shuffle + budget runtime (~24 tests) |
| `src/playlist_cursor.rs` | Curseur de parcours famille (B) : dernier média rendu |
| `src/group_state.rs` | État de passage d'un groupe (sequence/shuffle) famille (B) : idx, take_count, member_started_at, permutation |
| `src/library_actor.rs` | Acteur biblio possédant (mpsc, spawn_blocking) — gabarit refacto |
| `src/library_grpc.rs` | Transport gRPC biblio (traducteur mince acteur↔proto) |
| `src/plugin.rs` | Système de plugins : trait, acteur à état, quarantaine, `filter_pool`, `WasmPlugin` (extism), natifs logger/blacklist |
| `src/plugin_grpc.rs` | Transport gRPC plugins (list + control) |
| `plugins/{require-title,blacklist}-wasm/` | Crates guest WASM de démo (séparés, cible wasm32) : `filter_pool` |
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
| `migrations/0001→0010` | Schéma (0005 état grille, 0006 règles, 0007 biblio, 0008 curseur, 0009 groupe, 0010 runtime/shuffle groupe ; ⚠ 0004 absent) |
| `tests/sync.rs` | Intégration `sync` |
| `Doc/*.md` | Décisions d'architecture (référence durable) |
