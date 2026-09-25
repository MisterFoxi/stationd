# Format des playlists (`playlist.toml`)

Section du document de référence d'architecture. Récapitule les décisions
prises sur le format de description des playlists, pas le détail
d'implémentation.

## Principe général

Une entrée de playlist = un `id` + un `name` + un bloc **`selection`** (le
*quoi*) + un bloc **`broadcast`** (le *quand / comment*).

- **`id`** : clé stable, poignée dans le contrat gRPC. Référencée par les
  groupes, la CLI, l'historique SQLite. Ne change pas.
- **`name`** : affichage seulement. Peut changer, être traduit.

Un **groupe** n'est pas un concept distinct : c'est une playlist dont la
`selection` porte sur d'autres playlists (`mode = "group"`) au lieu de porter
sur des pistes. Un seul concept uniforme, aligné sur le modèle Liquidsoap
(tout est une source qui en combine d'autres).

## Source de vérité : SQLite autoritatif, TOML = format d'apply/export

`stationd` étant *single writer* et l'état vivant en SQLite, `playlist.toml`
n'est **pas** une config déclarative que `stationd` réécrirait à chaque
mutation. C'est un format d'échange, sur le modèle `kubectl apply` :

- `stationd apply playlist.toml` — réconcilie le fichier vers SQLite.
- `stationd export` — redéverse l'état courant en TOML.

Les deux sont de simples appels gRPC → la garantie de complétude CLU est
préservée (rien ne vit uniquement dans `api`). Ce choix évite la double
source de vérité qu'imposerait un TOML « déclaratif façon `nginx.conf` ».

**Le fichier porte des critères, jamais l'état résolu.** Pour une sélection
`dynamic`, `stationd` résout le pool en scannant la bibliothèque ; la liste
effective de pistes, l'historique et les cooldowns (anti-répétition) vivent
en SQLite. `stationd` ne réécrit jamais le fichier avec des pistes résolues.

**Alignement TOML ↔ contrat gRPC.** Chaque champ ci-dessous a son équivalent
dans le message `Playlist` du contrat `stationd` — même modèle, deux
sérialisations. Le jeu de `field` autorisés en filtre fait donc partie du
contrat, pas d'une convention locale à `api`.

## Bloc `selection` — le *quoi*

| Champ | Valeurs | Portée | Rôle |
|---|---|---|---|
| `mode` | `static` \| `dynamic` \| `queue` \| `group` | tous | nature de la source |
| `order` | voir matrice ci-dessous | tous sauf `group` | discipline de parcours |
| `match` | `all` (ET) \| `any` (OU) | `dynamic` | combinaison des filtres |
| `filter[]` | `field` / `op` / `value` | `dynamic` | critères de sélection |
| `files[]` | liste de chemins | `static` | pistes explicites |
| `order_by` | `mtime` \| `filename` \| `published` | `order` datée | clé de tri temporel |
| `unplayed_only` | booléen | `order` datée | play-once (voir épisodes) |
| `max_len` | entier | `queue` | capacité de la file |
| `strategy` | `weighted` \| `rotate` \| `sequence` | `group` | combinaison des membres |
| `members[]` | `ref` (+ `weight` \| `take`) | `group` | playlists membres |

`order` a été placé dans `selection` (et non `broadcast`) parce que ses
valeurs valides **dépendent du `mode`** — `stationd` valide la combinaison au
niveau du contrat :

| `mode` | `order` autorisés | Sens |
|---|---|---|
| `static` | `shuffle`, `sequential` | ensemble fixe |
| `dynamic` | `shuffle`, `sequential`, `newest`, `oldest` | pool résolu par scan |
| `queue` | `fifo`, `lifo` | tampon volatil rempli au runtime |
| `group` | — | c'est `strategy` qui gouverne |

Filtres : structurés (`field`/`op`/`value`), pas de DSL en chaîne, pour
réduire la surface de parsing. Limite assumée : les booléens complexes
(parenthèses, OU imbriqués) ne sont pas exprimables — groupes de filtres
imbriqués à ajouter plus tard si le besoin apparaît.

## Bloc `broadcast` — le *quand / comment*

| Champ | Valeurs | Rôle |
|---|---|---|
| `type` | `general` \| `interval` \| `scheduled` | mode de participation à l'antenne |
| `weight` | entier | fréquence relative (`general`, et membres de groupe `weighted`) |
| `limit` | entier | nombre de pistes émises par activation, puis rend la main |
| `every_tracks` / `every_time` | entier / durée | cadence d'intercalage (`interval`) |
| `schedule[]` | `start`/`end`/`days` (+ `date_start`/`date_end`) | créneaux (`scheduled`) |
| `repeat` | booléen | source finie : reboucler ou non |
| `on_exhausted` | `fallthrough` \| `stop` \| `disable` \| `hold` | comportement à épuisement |
| `constraints` | `no_same_artist_within`, `no_same_track_within` | anti-répétition au séquençage |

## Compteurs et tour-de-rôle

Quatre champs distincts, volontairement non recouvrants :

- **`limit`** (`broadcast`) — N pistes par activation avant de céder la main.
  Pool `dynamic` ou `static` en rotation. `limit = 1` = un élément par tour.
- **`take`** (membre, groupe `sequence` **uniquement**) — N pistes
  consécutives d'un membre avant de passer au suivant (3 rock → 1 jingle → 3
  pop). `take` hors d'un groupe `sequence` = erreur de validation.
- **`repeat` / `on_exhausted`** (`broadcast`) — comportement d'une source
  *finie* une fois vidée.
- **`max_len`** (`selection`, mode `queue`) — capacité de la file, borne
  l'accumulation ; refuse au-delà (0/absent = illimité).

## File d'attente vs sélection datée : deux axes séparés

Deux besoins qui répondent tous deux à « quel bout ? » mais qu'on ne fusionne
pas :

- **`queue` (`fifo`/`lifo`)** — discipline de consommation d'un **tampon
  volatil** poussé au runtime (demandes auditeurs, injection DJ). L'ordre =
  l'ordre d'arrivée ; l'élément est consommé puis disparaît.
- **`dynamic` daté (`newest`/`oldest`)** — sélection ordonnée par date sur une
  **série persistante qui grandit** (épisodes, chroniques). L'ordre = un
  attribut stable ; l'ensemble reste ; mémoire du déjà-diffusé en SQLite.

Vocabulaire : sur une liste `static`, « du haut vers le bas » est déjà
`sequential` — `fifo`/`lifo` n'ont de sens que sur une file qui se remplit et
se vide dynamiquement. Sur des épisodes, « le plus ancien d'abord » se dit
`oldest`, pas `fifo`.

### Épisodes — combinaisons `order` × `unplayed_only`

| `order` | `unplayed_only` | Comportement |
|---|---|---|
| `newest` | `false` | **Dernier en cours** : toujours le plus récent, rediffusé jusqu'à l'arrivée d'un plus neuf (bulletin, édito du jour) |
| `oldest` | `true` | Rattrapage chronologique, une fois chacun (feuilleton, cours) |
| `newest` | `true` | Chaque nouveauté une seule fois, du plus récent au plus ancien (dépile un backlog) |
| `oldest` | `false` | Boucle du plus ancien au plus récent (cohérent, peu utile) |

`unplayed_only` = une contrainte anti-répétition à expiration **infinie** :
même mécanique que `constraints`, mais le cooldown ne retombe jamais.
Stockage en SQLite, keyé par une identité stable d'épisode.

**Identité d'épisode** : chemin fichier + garde-fou (invalidation si
taille/mtime divergent). Le chemin est simple mais casse au
renommage/déplacement ; le hash de contenu survit mais coûte un scan. Le
garde-fou est le compromis retenu, vu le passif NFS.

**`order_by`** fait partie du contrat. Pour les fichiers locaux, défaut
recommandé = `filename` (préfixe `2026-03-14_…` ou `ep017_…`), plus robuste
que `mtime` (qu'une copie NFS ou un `rsync` réécrit, réordonnant la série).
`published` n'a de sens que pour une source feed.

## Groupes

- **`strategy`** : `weighted` (→ `random(weights=…)`), `rotate`, `sequence`.
- **`members`** : liste de `{ ref, weight? , take? }`. `weight` en
  `weighted`, `take` en `sequence`.
- **Priorité diffusion** : le groupe impose le *quand* (son `schedule`), le
  membre garde son *quoi* (`selection`) et son intercalage propre (jingles).
  Le `type`/`schedule` propre d'un membre est **ignoré** dans le contexte du
  groupe.
- **Groupes imbriqués** : techniquement possibles (sources Liquidsoap
  imbriquées), mais imposent une **détection de cycle** côté `stationd`
  (valider le graphe comme un DAG).

## Relais (`mode = "remote"`, `url`)

Une playlist `remote` relaie un flux externe (`input.http` côté Liquidsoap,
cf. `Doc/liquidsoap.md`) : pas de piste, pas de fin. Le relais prend
l'antenne à la fin de la piste en cours et la garde **tant que la règle qui
le désigne gagne** ; il cède dans les ~2 s quand la grille donne autre chose.

- Fait pour une **tranche** : `day_part`, `base_rotation`. En `at_clock` ou
  `every`, la règle ne gagne qu'un tour (repère consommé, cooldown relancé) :
  le relais ne durerait qu'une interrogation (~2 s).
- **Membre de groupe** : lui donner un budget **`runtime`** (durée du
  relais). Avec `take`, chaque interrogation (~2 s) compterait pour une
  piste.
- Pas d'historique anti-répétition, pas d'`unplayed_only` (un flux n'a pas
  d'identité de fichier). Un override ou un `at_clock` hard ne peut pas
  insérer un flux (insert sans fin) : refusé, journalisé.

## Correspondance moteur

Ce qui est du Liquidsoap direct : `queue` → `request.queue`, `scheduled` →
`switch` sur prédicat temporel, poids → `random(weights=…)`.

Ce qui relève du **séquenceur `stationd`** (aucun opérateur Liquidsoap unique
ne fait « N puis cède la main ») : `limit`, `take`, `on_exhausted`, le
tour-de-rôle des groupes. `stationd` émet N pistes puis bascule la source
active — cohérent avec le *single writer*.

## Exemples de référence

```toml
# Rotation générale, pool dynamique, quota par activation
[[playlist]]
id = "hits-recents"
name = "Hits récents"
enabled = true
[playlist.selection]
mode = "dynamic"; order = "shuffle"; match = "all"
[[playlist.selection.filter]]
field = "year"; op = ">="; value = 2018
[playlist.broadcast]
type = "general"; weight = 5; limit = 15
[playlist.broadcast.constraints]
no_same_artist_within = "30m"

# File de demandes, consommée FIFO
[[playlist]]
id = "demandes"
name = "Demandes auditeurs"
enabled = true
[playlist.selection]
mode = "queue"; order = "fifo"; max_len = 20
[playlist.broadcast]
type = "interval"; every_tracks = 3

# "Le dernier en cours" : toujours le plus récent, rediffusé
[[playlist]]
id = "chronique-jour"
name = "La chronique du jour"
enabled = true
[playlist.selection]
mode = "dynamic"; order = "newest"; order_by = "filename"; unplayed_only = false
[[playlist.selection.filter]]
field = "path"; op = "prefix"; value = "chroniques/"
[playlist.broadcast]
type = "scheduled"; limit = 1; on_exhausted = "fallthrough"
[[playlist.broadcast.schedule]]
start = "08:00"; end = "08:15"; days = ["mon","tue","wed","thu","fri"]

# Feuilleton : rattrapage chronologique, une fois chacun
[[playlist]]
id = "feuilleton"
name = "Feuilleton radio"
enabled = true
[playlist.selection]
mode = "dynamic"; order = "oldest"; order_by = "filename"; unplayed_only = true
[[playlist.selection.filter]]
field = "path"; op = "prefix"; value = "feuilleton/"
[playlist.broadcast]
type = "scheduled"; limit = 1; on_exhausted = "hold"
[[playlist.broadcast.schedule]]
start = "20:00"; end = "20:30"; days = ["sun"]

# Groupe séquentiel avec quota par membre
[[playlist]]
id = "sequence-matin"
name = "Séquence matin"
enabled = true
[playlist.selection]
mode = "group"; strategy = "sequence"
members = [
  { ref = "hits-recents", take = 3 },
  { ref = "jingles",      take = 1 },
  { ref = "classiques",   take = 3 },
]
[playlist.broadcast]
type = "scheduled"
[[playlist.broadcast.schedule]]
start = "06:00"; end = "10:00"; days = ["mon","tue","wed","thu","fri"]
```

## Encore ouvert

- **Partage vs exclusivité** d'une playlist membre : peut-elle aussi tourner
  seule en rotation générale, ou l'appartenance à un groupe la rend-elle
  exclusive ?
- **Précédence `limit` vs `constraints`** : si `limit = 15` mais que
  l'anti-répétition ne peut fournir 15 pistes distinctes → émettre moins,
  relâcher la contrainte, ou piocher hors quota ? Règle à expliciter.
- **Défaut de `on_exhausted`**, et : une playlist finie épuisée se `disable`
  automatiquement (réécriture SQLite via `apply`/`export`) ou reste inactive
  en mémoire jusqu'au prochain `apply` ?
- **LIFO public = piège** (le dernier arrivé double tout le monde). Le besoin
  « joue ça tout de suite » est mieux modélisé comme un *push prioritaire en
  tête* (opération gRPC ponctuelle) que comme un mode LIFO permanent. LIFO
  reste pertinent pour une file d'injection DJ, à confirmer selon qui
  alimente la file.
- **Sémantique du `schedule`** : le créneau est-il une fenêtre stricte
  (épisode coupé à la fin) ou une heure de démarrage (l'épisode va au bout,
  quitte à mordre sur le segment suivant) ? Dépasse le seul cas épisode.
- **Source feed (podcast RSS)** hors bibliothèque locale : `order_by =
  "published"` n'a de sens que là. Probablement un plugin WASM qui alimente
  le pool plutôt qu'un mode natif. Périmètre à décider.
- **Mode `episodic` dédié vs `dynamic` étendu** : rester sur `dynamic`
  (philosophie « un seul concept ») ou introduire un mode explicite pour
  rendre la validation plus lisible (interdire `shuffle` avec
  `unplayed_only`, exiger `order_by`) ? Défaut retenu : `dynamic` étendu,
  quitte à ajouter le mode dédié si les règles deviennent trop tordues.
