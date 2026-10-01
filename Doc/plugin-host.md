# Système de plugins — la surface hôte

Document de référence (décisions, pas détail d'implémentation). Troisième et
dernier volet du contrat plugins, avec `plugin-events.md` (le core **notifie**
le plugin) et `plugin-hooks.md` (le core **appelle** le plugin). Ici :
**ce que le plugin appelle sur le core** — les capacités qui lui permettent
d'*agir*.

## Principe : mécanisme au core, politique au plugin

Le flux d'événements est en lecture seule ; les hooks influencent un calcul en
cours. **Agir** (interrompre l'antenne, arrêter la diffusion, persister un
état) passe **uniquement** par la surface hôte. C'est la seule voie d'effet
d'un plugin.

Ligne directrice, déjà posée pour le cas « stop quand auditeurs = 0 » : **le
core fournit le mécanisme, le plugin porte la politique.** Le core offre la
*capacité* `control(StopWhenIdle)` ; c'est un plugin qui décide *quand* l'armer
(en observant `ListenersSampled`). Aucune condition métier (« == 0 ») ne vit
dans le core.

La surface est **remise au plugin dans `on_load`** (le `ctx`). Un plugin ne
l'obtient pas autrement ; elle est donc naturellement scopée à ce plugin.

## Sandboxing : des capacités, jamais des chemins

Cohérent avec le choix WASM (`wasm32-unknown-unknown`, pas d'accès disque/réseau
direct) : le plugin ne reçoit **jamais** un chemin de fichier, un handle DB brut
ou un socket. Il reçoit des **capacités** — des fonctions hôte à l'ABI étroite.
Conséquences :

- un plugin ne peut pas toucher la base d'un autre plugin, ni celle du core ;
- un plugin ne peut pas lire/écrire le système de fichiers hors de ce que le
  core lui expose ;
- un plugin planté ou malveillant reste confiné (il ne peut pas planter
  `stationd` ni corrompre son état) — c'est la raison d'être du sandboxing.

## Les capacités

### `control` — piloter la diffusion

| Action | Effet | Nature |
|---|---|---|
| `Pause` / `Resume` | suspend / reprend | dur |
| `StopWhenIdle` | **mise en veille armée** : `sleeping` à la prochaine occasion propre (fin de piste **et** zéro auditeur), pas un kill | mou (armé) |
| `Wake` | quitte `sleeping` ; **no-op depuis tout autre état** (ne dépause jamais, n'annule pas un drain) | mou |

`control` est **first-class sur la station** : ces actions existent
indépendamment de tout plugin (`stationctl station pause|resume|
stop-when-idle|wake`), et un plugin peut les invoquer via `host.control(...)`.
Un plugin n'est donc qu'un émetteur parmi d'autres.

**Pas de `Stop`** (2026-09-28) : l'arrêt opérateur (`stationctl station stop`)
arrête stationd lui-même (marqueur `data/stationd.stopped`, s6 ne le relance
pas) — hors de portée de tout plugin ; `{"action":"stop"}` est refusé.

`StopWhenIdle` est le cas « MAJ quand plus personne n'écoute » : ce n'est pas
« stop maintenant » mais « en veille dès que c'est propre ». Il produit un état
de diffusion `draining` observable (cf. `BroadcastStateChanged`). Le réveil est
`Wake` (le plugin `stop-when-idle` l'appelle dès qu'un auditeur revient) ; le
core réveille aussi seul une station en veille après trois relevés d'audience
en échec consécutifs, ou immédiatement lorsqu'un DJ prend l'antenne.

Note : l'**état** de diffusion (`running`/`paused`/`draining`/`sleeping`) et la
**commande** existent au niveau contrat et sont testables tout de suite ; l'effet
réel sur le flux audio dépend du câblage Liquidsoap, mais l'état de la station,
lui, n'attend pas LS.

### `push_override` — pousser du contenu interruptif

Le plugin pousse un media ou une playlist dans une **file d'override** que
`next_media` consulte **avant** `resolve_next`. C'est la couche la plus
prioritaire du modèle de priorité de l'architecture :

```
override (plugin/humain)  >  one-shot  >  grille récurrente  >  fallback
```

Le résolveur pur (`resolve_next`) **n'est pas touché** : l'override court-circuite
au niveau `GridEngine`, en amont. Paramètres d'un override :

- **contenu** : un `media_path` concret **ou** un `playlist_ref` (résolu comme
  une activation qui prend la main — utile pour une séquence : une PL d'urgence,
  un bloc). Un media = un passage ; une PL = jusqu'à épuisement / son quota.
- **`soft` | `hard`** :
  - **`soft`** — s'insère à la **prochaine frontière de piste** (au prochain
    `next`). Entièrement dans `GridEngine`, aucune coupe. **Honoré maintenant.**
  - **`hard`** — coupe/duck la piste en cours pour taper l'instant. Suppose une
    source interruptrice **côté Liquidsoap** → **dépend du câblage LS**.
- **`expiry`** : péremption. Un override manqué (daemon occupé, redémarrage)
  au-delà de sa fenêtre est **abandonné**, pas rejoué en retard — même logique
  que la péremption des `AtClock`.

Tant que LS n'est pas là, un `hard` demandé est **dégradé en `soft` avec un
avertissement loggé** (une annonce d'urgence vaut mieux tard que jamais),
plutôt que refusé — **à confirmer** (cf. Encore ouvert).

### `db` — la base du plugin

Chaque plugin a **sa** base, mais **le core l'ouvre pour son compte** : le
plugin n'obtient jamais un chemin ni un handle SQLite, seulement trois appels
hôte scopés à sa base. Conséquences :

- **un fichier par plugin** (`<dossier de database.path>/plugins/<name>.db`) :
  un plugin ne corrompt pas les données d'un autre ni du core ;
- **single-writer préservé** : chaque fichier a un unique writer, le core, pour
  le compte d'un unique plugin ;
- WASM-compatible : pas d'I/O disque dans le plugin, tout passe par la fonction
  hôte.

C'est de la **famille (B) du point de vue du plugin** (état durable qui lui
appartient, jamais supprimé automatiquement), mais **hors du périmètre de vérité
du core** : un plugin stats reconstruit ses agrégats au-dessus du journal de
diffusion du core, il n'est jamais la source de vérité de quoi que ce soit
d'essentiel à l'antenne.

**Le plugin possède son schéma** : du vrai SQL (tables, index, `GROUP BY`), pas
un simple clé/valeur. Il le livre sous forme de **migrations ordonnées**
(natif : `Plugin::db_migrations` ; WASM : export `db_migrations`, sans entrée,
qui rend `["SQL", …]`), version = rang + 1. Le core les applique au démarrage,
**avant `on_load`**, chacune dans une transaction avec son enregistrement dans
`_stationd_migrations` (texte SQL complet, dans le fichier du plugin). Une
migration déjà appliquée puis modifiée, ou appliquée mais plus livrée → plugin
`failed` (phase `Migrate`), `on_load` pas appelé. Même règle que les migrations
du core : on n'édite jamais, on ajoute.

**Les appels** (JSON dans les deux sens, comme `station_control`) :

| Appel | Entrée | Réponse |
|---|---|---|
| `db_query` | `{"sql", "params"?}` — une instruction **en lecture seule** | `{"ok":true,"columns":[…],"rows":[[…],…]}` |
| `db_exec` | `{"sql", "params"?}` — **une** instruction | `{"ok":true,"changes":n,"last_insert_rowid":id}` |
| `db_batch` | `{"statements":[{"sql","params"?},…]}` | `{"ok":true,"results":[…]}` — **tout ou rien** |

`params` : tableau (positionnels `?`, `?1`) ou objet (nommés `:x`, `@x`, `$x` ;
préfixe `:` implicite). Chaque paramètre de l'instruction doit être lié (un
oubli est une erreur, jamais un NULL silencieux). Valeurs : `null`, booléen
(→ 0/1), nombre, chaîne, `{"blob": "<base64>"}` (même forme en sortie).
Un échec est une **donnée** (`{"ok":false,"error":…}`), jamais un trap ; dans
un `db_batch`, l'erreur nomme l'instruction (`statement 1: …`) et **rien**
n'est écrit.

**Confinement** (le SQL du plugin n'est pas de confiance) :

- `ATTACH` / `DETACH` refusés (autorisation + `SQLITE_LIMIT_ATTACHED = 0`),
  `VACUUM` refusé (`VACUUM INTO` écrirait un fichier ailleurs),
  `load_extension` refusé, mode défensif SQLite ;
- `PRAGMA` refusés sauf introspection en lecture (`table_info`, `index_list`,
  `foreign_key_list`…) — un plugin ne relève pas son quota ;
- pas de `BEGIN` / `COMMIT` / `SAVEPOINT` : les transactions sont au core
  (`db_batch`), pas de transaction ouverte à cheval sur deux appels ;
- tables `_stationd*` lisibles, jamais modifiables ;
- bornes `[plugin.db]` : `max_size_mb` (64 — une écriture qui dépasse échoue,
  `database or disk is full`), `query_timeout_ms` (200 — par appel, un
  `db_batch` entier compris ; au-delà l'instruction est interrompue et le batch
  annulé), `max_rows` (10 000 — plus de lignes = erreur, jamais une troncature
  silencieuse).

**CLI** (`stationctl plugin db <name> …`, connexion séparée en lecture seule,
le plugin n'est pas touché) : `info` (fichier, taille, version du schéma,
tables et lignes, bornes), `query "<SELECT>"` (même confinement, 10 s),
`reset --yes` (supprime le fichier — refusé tant que le plugin est chargé ; le
prochain démarrage recrée la base et rejoue les migrations).

Implémentation : `src/plugin_db.rs` (rusqlite synchrone : les plugins sont
appelés de façon synchrone depuis leur tâche, un appel hôte ne peut pas
attendre sqlx). Démo : `plugins/play-stats-wasm`.

## Ce que la surface hôte n'est pas

- Pas un moyen de **modifier la grille ou les playlists** : un plugin n'écrit
  pas la config (single-writer TOML = humain + stationd, jamais un tiers). S'il
  veut influencer la sélection, c'est par le hook `filter_pool`, pas en
  réécrivant des fichiers.
- Pas un accès **réseau/FS** général : seulement les capacités listées.

## Décisions (tranche A2, 2026-09-23)

- **`hard` sans Liquidsoap** : **dégradé en `soft` + avertissement**. Le push
  est accepté, la réponse porte `degraded = true`, le mode demandé (`hard`)
  reste visible dans la file. « L'annonce passe », en retard (prochaine
  frontière).
- **Capacités déclarées** : `capabilities = ["control", "push_override", "db"]` dans
  le `[[plugin]]` (ensemble fermé ; nom inconnu = config refusée au démarrage).
  Un appel hors capacités est **refusé et loggué**, jamais exécuté ; en WASM
  le refus revient au guest comme donnée (`{"ok":false,…}`), pas comme trap.
  `plugin list` affiche les capacités.
- **`media_path` arbitraire autorisé** : chemin relatif à `media/` (`\` → `/`,
  absolu / lecteur / `..` / NUL refusés au push), pas forcément indexé ;
  **vérifié sur disque au passage** — absent → override abandonné (loggué),
  la grille reprend.
- **Portée d'un override PL** : `tracks` pistes (défaut 1), résolues comme une
  source de grille (plugins et contraintes compris). Pool vide / ref inconnue
  au passage → abandonné, loggué, la grille reprend.
- **Rate-limit** : file bornée (64 overrides en attente), refus explicite
  au-delà. Pas de quarantaine sur refus (ce n'est pas un crash).
- **File volatile** : en mémoire. Un arrêt du daemon perd les overrides en
  attente — **annoncé** dans les logs d'arrêt (nombre perdu), pas silencieux.
  L'état de diffusion, lui, est persisté (une veille reste une veille après
  redémarrage ; migration 0021 : un ancien `stopped` devient `sleeping`).
- **`StopWhenIdle`** : `draining` devient `sleeping` au prochain bord de piste
  si le **dernier** échantillon d'auditeurs vaut 0. Jamais échantillonné →
  aucune veille (pas de signal d'audience). `resume` annule un drain.
- **Connexions anciennes** (plugins `stop-when-idle` et `stop-when-idle-wasm`) :
  `max_connection_age = "12h"` active une alternative au compteur, avec
  `[icecast] listener_snapshots = true`. Le core revérifie au bord de piste
  que toutes les connexions ont atteint le seuil et que le relevé est complet
  et frais. La veille conserve les clients sur le bruit et se termine à
  l'arrivée d'un nouveau couple (mount, id), ou après trois collectes détaillées
  en échec consécutives. Un relevé réussi remet à zéro le compteur de sa collecte.
  La baseline reste en mémoire : le chargement du plugin avec cette option
  réveille une station sleeping par prudence.
  `Host::listener_connections()` / WASM `listener_connections("{}")` expose
  uniquement mount, id et connected_seconds, sans capacité `listener_details`.
  `Host::stop_when_connections_old(seconds)` nécessite `control` ;
  en WASM, utiliser `station_control({"action":"stop_when_idle","max_connection_age":43200})`.
  Voir [la spécification](spec-veille-age-connexions.md).
- **Réveil** (`Wake`, ou `resume`) : la piste du créneau à l'heure du réveil
  (un groupe tenu au moment de la veille est libéré et remis en tête de
  cycle). Pendant la veille, Icecast est lu toutes les
  `[icecast] poll_interval_sleeping` s (défaut 3).
- **Contrat** : `broadcast_v1.proto` (`GetState` / `Control` /
  `SampleListeners` / `PushOverride` / `ListOverrides` / `ClearOverrides`),
  CLI `stationctl station|override|debug listeners`. `ResolveNext` rend
  `OVERRIDE` (+ `override_source`) ou `HALTED` (+ `halted_state`, aucun média,
  Liquidsoap ne doit pas combler).

## Décisions (tranche `db`, 2026-09-27)

- **SQL scopé plutôt que clé/valeur** : le plugin déclare ses tables (un
  plugin stats a besoin de `GROUP BY` et d'index) ; un clé/valeur se réécrit
  côté guest si besoin. `db_get` / `db_put` prévus au départ abandonnés.
- **Transactions** : `db_batch` (plusieurs instructions, tout ou rien) répond
  à la question des écritures multi-clés ; aucune transaction exposée à
  cheval sur plusieurs appels.
- **Schéma = migrations livrées par le plugin**, appliquées avant `on_load`,
  jamais modifiées une fois appliquées.
- **Capacité `db` déclarée** ; nom du plugin = nom du fichier (ASCII, `-`,
  `_`) ; noms de plugins uniques (vérifié au chargement de la config, comme
  `[plugin.db]` sans la capacité).

## Encore ouvert

- **Persistance de la file d'override** si un cas réel l'exige (aujourd'hui la
  péremption rend la perte au redémarrage acceptable).
