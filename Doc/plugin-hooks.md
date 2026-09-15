# Système de plugins — hooks et cycle de vie

Document de référence (décisions, pas détail d'implémentation). Volet **hooks
synchrones** + **cycle de vie** + **statut et contrôle** du système de plugins.
Complément de `plugin-events.md` (le flux d'événements, unidirectionnel et
observationnel). La **surface hôte** — ce que le plugin peut *appeler*
(`push_override`, `control`, base de données) — fera l'objet de sa propre
section ; elle est remise au plugin dans `on_load` (voir ci-dessous).

## Trois natures de points d'accroche, à ne pas confondre

| Nature | Exemples | Sens | Retour | Dans le chemin ? | Peut bloquer ? |
|---|---|---|---|---|---|
| **Cycle de vie** | `on_load`, `on_unload` | init / arrêt propre | échec possible | — | oui, **borné** (timeout) |
| **Hook synchrone** | `on_scan`, `filter_pool` | *influence* le résultat | une valeur | oui (scan / décision) | oui, **borné** |
| **Événement** | `on_event` | *observe* un fait acté | rien | non | **non** (best-effort) |

C'est la ligne structurante : un hook synchrone **retourne** et modifie le
chemin qui l'appelle (donc il est dans le chemin critique et doit être borné) ;
un événement ne retourne rien et ne doit jamais retenir l'antenne. `push_override`
(pousser un media interruptif) **n'est pas un hook** : c'est une capacité de la
surface hôte que le plugin *appelle*, pas un point que le core invoque.

## Le trait `Plugin`

Points que le core appelle sur un plugin (sens, pas signature figée) :

- **`on_load(ctx)`** — le plugin est activé. Il reçoit son **contexte** : sa
  surface hôte et l'accès à *sa* base. Il s'initialise (config, ouverture de
  base). **Synchrone, borné, peut échouer** → un échec refuse le plugin (état
  `Failed`), il n'est pas activé. Couvre aussi le boot : au démarrage du daemon,
  chaque plugin déclaré est « chargé ». Nommé d'après le *plugin*, pas le
  daemon, pour ne pas coder en dur « plugins = durée de vie du daemon » (le
  rechargement à chaud casserait cette hypothèse).
- **`on_unload()`** — arrêt propre : flush, fermeture de base. **Borné** : un
  `on_unload` qui traîne ne fige pas l'arrêt du daemon (timeout → passage forcé
  en `Disabled`, loggé).
- **`on_scan(media) -> attributs`** — enrichissement au scan (attributs non
  standard, cf. périmètre plugins). Dans le chemin de scan.
- **`filter_pool(candidates) -> candidates`** — filtre / pondère un pool résolu,
  juste avant le choix final. Dans le chemin de décision (`selection`).
- **`on_event(event)`** — réaction à un événement (cf. `plugin-events.md`).
  Ne retourne rien, best-effort.

## États d'un plugin

Un plugin est une entité à **état observable** (no-silent-failure : un plugin
en échec ne disparaît pas, il reste visible avec sa raison) :

| État | Signification |
|---|---|
| `Loaded` | `on_load` a réussi ; reçoit events et hooks |
| `Disabled` | désactivé volontairement (config ou `plugin stop`) — pas une erreur |
| `Failed { phase, reason }` | `on_load` ou un hook a échoué ; **inscrit mais inactif**, raison conservée |
| `Quarantined { reason, failures }` | trop d'échecs en cours de route (voir seuil) ; hooks et events **plus appelés** jusqu'à réarmement explicite |

`phase` situe l'échec : `load` / `unload` / `scan` / `filter_pool` / `event`.
Un plugin `Failed`/`Quarantined` **reste dans la liste** — c'est ça, « statut
visible ». `Quarantined` est distinct de `Disabled` (volontaire) et de `Failed`
(échec ponctuel, typiquement au chargement) : il dit « désactivé *par le core*
pour crash répété ».

## Déclaration vs état runtime

Deux registres distincts, à croiser :

- **Déclaration** — la liste des plugins voulus, en **config station** (nom,
  activé ou non). Indépendante du succès de chargement : sans elle, un plugin
  qui rate `on_load` n'aurait nulle part où s'afficher.
- **État runtime** — `Loaded` / `Disabled` / `Failed{…}` / `Quarantined{…}` +
  dernière raison + compteur d'échecs (fenêtre glissante).

`plugin list` montre les deux : « déclaré X, activé, état = `Failed`(load :
base verrouillée) ».

Forme de la déclaration (config station) :

```toml
[[plugin]]
name    = "stats"
enabled = true
order   = 50          # ordre d'application des hooks, croissant ; défaut 50
# … config propre au plugin, opaque au core, remise au plugin dans on_load
```

**`order`** fixe l'ordre d'application quand plusieurs plugins s'accrochent au
même point (les `filter_pool` se **chaînent** : le pool sort de l'un pour entrer
dans le suivant). Application par `order` **croissant**, **départage par `name`**
pour un ordre totalement déterministe. Défaut **50** (rang neutre, comme les poids
de playlist) : un plugin sans `order` se range au milieu. Un seul `order` par
plugin (couvre `filter_pool` et `on_scan` ensemble ; un ordre par-hook serait un
raffinement si un cas réel l'exige). C'est un **ordre**, pas un poids : on veut
une séquence déterministe, pas une combinaison proportionnelle.

## Contrôle du cycle de vie (CLI-first)

Piloter les plugins à chaud, dans le contrat gRPC (donc atteignable au CLI
comme tout le reste) :

```
stationctl plugin list                 # nom, déclaré, activé, état, dernière raison
stationctl plugin start   <name>       # réactive un Disabled/Failed  → on_load
stationctl plugin stop    <name>       # désactive un Loaded           → on_unload, Disabled
stationctl plugin restart <name>       # stop puis start, MÊME artefact (reset d'état runtime)
stationctl plugin reload  <name>       # relit l'artefact depuis le disque, puis on_load
```

RPC correspondants : `PluginList`, `PluginControl { Start | Stop | Restart |
Reload }`.

**`start`/`stop` sont une paire symétrique** (sans `start`, `stop` serait un
aller sans retour). **`restart`** ré-exécute `on_load` sur l'artefact déjà
chargé (réarmer un `Failed` après correction de sa base/config). **`reload`**
relit l'artefact lui-même depuis le disque (le `.wasm`) puis `on_load` — plus
fort que `restart`. **En natif (Rust compilé en dur), `reload` == `restart`**
(rien à relire) ; la divergence réelle arrive avec WASM. Les deux sont définis
au contrat dès maintenant pour ne pas mentir sur la CLI plus tard.

Transitions (toutes observables via `plugin list`) :

```
Disabled --start----------> Loaded | Failed{load}
Loaded   --stop-----------> Disabled          (on_unload borné)
Failed   --restart/reload-> Loaded | Failed{nouvelle raison}
Loaded   --reload---------> Loaded (nouvel artefact) | Failed
Loaded   --N échecs (fenêtre)--> Quarantined            (auto, par le core)
Quarantined --restart/reload--> Loaded | Failed | Quarantined
```

Un `stop` prend effet à la **prochaine frontière** (le plugin cesse de recevoir
des hooks à partir de là) ; on ne coupe pas un `filter_pool` déjà en cours —
cohérent avec les bornes molles du moteur.

## Échec et dégradation

Best-effort **mais visible**, jamais fail-fast silencieux ni avalage :

- **`on_load` échoue** → `Failed`, inscrit, inactif ; **les autres plugins
  démarrent quand même**, le daemon aussi. La visibilité (`plugin list`) est ce
  qui empêche que « une politique qu'on croyait active » disparaisse en silence.
- **crash en `on_event`** → log + `Failed` + on continue (l'événement est
  best-effort de toute façon).
- **crash en `filter_pool`** → on prend le pool **non filtré** (dégradé) +
  `Failed` visible + compteur. Une décision doit sortir ; mais un filtre
  critique tombé silencieusement serait pire que visible-et-dégradé.
- **crash en `on_scan`** → média indexé **sans enrichissement** (dégradé) +
  visible.

### Retry et quarantaine

**Un plugin en échec ne se retente jamais tout seul en boucle.**

- **`on_load` échoue** → **une seule tentative**, pas de rechargement
  automatique. Réarmement uniquement explicite (`plugin restart`/`reload`). Un
  plugin dont la base est verrouillée ne martèle pas l'ouverture à chaque tick.
- **Échec en cours de route** (`filter_pool`/`on_event`/`on_scan`) → **compteur
  en fenêtre glissante** : au-delà de **N = 3 échecs dans une courte fenêtre**, le
  plugin passe `Quarantined` et **ses hooks/events ne sont plus appelés**
  jusqu'à réarmement explicite. La fenêtre glissante distingue le crash
  *chronique* du *hoquet* occasionnel : un fichier bizarre isolé ne condamne pas
  le plugin, N crashes rapprochés si. N et la durée de fenêtre sont
  configurables ; ce sont les défauts.

Sortir de quarantaine est **toujours** explicite (`plugin restart`) — jamais un
réarmement automatique, qui rejouerait le crash en boucle. C'est la contrepartie
de « visible et sous contrôle ».

## Encore ouvert

- **Plugin « critique » vs best-effort** : faut-il qu'un plugin déclare qu'il
  est *critique* (son échec bloque la décision / le démarrage) plutôt que
  dégrader ? Défaut proposé : tout est best-effort + visible ; la criticité
  déclarée est un raffinement.
- **`reload` = relire aussi la config du plugin ?** Pour l'instant `reload`
  vise l'artefact ; si la config du plugin vit à part, décider si `reload` la
  relit aussi.
- **Timeout de `on_load`/`on_unload`** : valeur par défaut et si elle est
  configurable par plugin.
- **Surface hôte** (`push_override`, `control`, base) : contrat détaillé dans sa
  propre section ; remise au plugin via le `ctx` de `on_load`.
