# Système de plugins — les événements

Document de référence (décisions, pas détail d'implémentation). Volet
**événements** du système de plugins. Les deux autres volets — les **hooks
synchrones** (`on_scan`, `filter_pool`…) et la **surface hôte** (ce que le
plugin peut *appeler* : `push_override`, `control`, base de données) — feront
l'objet de leurs propres sections ; ce document ne traite que du flux
d'événements core → plugin.

## Rôle et frontière

Un **événement** est une notification **unidirectionnelle** que le core émet
vers les plugins pour signaler *ce qui vient de se passer*. Trois lignes de
démarcation, structurantes :

- **Événement ≠ hook synchrone.** Un hook (`filter_pool`, `on_scan`) est
  appelé *dans* un chemin (décision, scan), **retourne une valeur** et
  influence le résultat. Un événement ne retourne rien et n'influence rien du
  chemin qui l'a produit — c'est un fait déjà acté. Observer, pas décider.
- **Événement ≠ moyen d'agir.** Le flux d'événements est en **lecture seule**.
  Pour agir (pousser un override, stopper la diffusion, écrire en base), le
  plugin utilise la **surface hôte**, jamais le flux. Le cas « stop quand
  auditeurs = 0 » est la composition des deux : `on_event(ListenersSampled)`
  observe, `host.control(StopWhenIdle)` agit.
- **Événement ≠ log interne.** Un événement est un **fait métier stable** du
  contrat plugin, pas un détail d'implémentation. Les erreurs internes, le
  tracing bas niveau, l'état transitoire ne sont pas des événements : les y
  mettre coupleraient les plugins aux entrailles du core.

## Invariants

- **Enum non-exhaustif.** `PluginEvent` est `#[non_exhaustive]` ; un plugin
  **ignore ce qu'il ne connaît pas** (`match … _ => {}`). Ajouter un événement
  ne casse aucun plugin existant. C'est ce qui autorise à définir un événement
  aujourd'hui et à en brancher la source plus tard, sans rupture de contrat.
- **Best-effort, jamais bloquant.** L'émission d'un événement ne doit **jamais**
  retarder ni faire échouer le chemin critique qui l'a produit (une décision de
  grille, un scan). Un plugin lent, en erreur ou planté **ne retient pas
  l'antenne** — cohérent avec le sandboxing (« un plugin buggé ne peut pas
  planter `stationd` »). Conséquence de conception : l'émission est découplée du
  producteur (le core ne *rappelle* pas un plugin en tenant `next_media`).
- **Pas de rétro-historique.** Un plugin ne reçoit que les événements produits
  **après son chargement**. Le rattrapage d'un état antérieur passe par sa
  propre base ou par l'historique du core (requêtable), pas par le flux.
- **Ordre.** Les événements d'un **même type** arrivent dans l'ordre de
  production. Aucune garantie d'ordre total cross-type au départ (à durcir si
  un besoin réel apparaît — cf. Encore ouvert).

## Source réelle vs contrat

Un événement peut exister **au contrat** (défini, émettable) sans avoir encore
de **source réelle** dans le daemon. Trois niveaux :

| Niveau | Signification | Test |
|---|---|---|
| **Réel** | Le core produit déjà la donnée aujourd'hui | émis nativement |
| **Différé (LS)** | Dépend du câblage Liquidsoap (ce qui joue *vraiment*) | non émis tant que LS absent |
| **Différé (Icecast)** | Dépend de la capture timeline Icecast (audience) | injectable en test |

Pour les sources différées, un mécanisme d'**injection de test**
(`stationctl debug emit …` ou équivalent) permet d'exercer les plugins sans la
couche réelle — indispensable pour développer p. ex. le plugin « stop quand
auditeurs = 0 » avant qu'Icecast ne soit branché.

## Catalogue proposé

### Source réelle aujourd'hui

| Événement | Quand | Données | Usage typique |
|---|---|---|---|
| `TrackResolved` | une décision vient d'être prise (`next_media`) | `media_path`, `playlist_ref`, `rule_id`, `origin` | log, stats de rotation, « quoi et pourquoi » |
| `TrackSkipped` | un candidat est écarté (fichier disparu, contrainte) | `media_path?`, `reason` | observabilité no-silent-failure |
| `LibraryScanned` | un scan biblio se termine | `found`, `unavailable`, `skipped` | stats catalogue, invalidation de cache plugin |
| `GridApplied` | la grille a été (ré)appliquée | `rule_count` | recalcul/cache plugin |
| `PlaylistsReloaded` | la vue playlists a été resynchronisée | `count` | idem |

### Au contrat, source différée

| Événement | Dépend de | Données | Usage typique |
|---|---|---|---|
| `ListenersSampled` | Icecast (injectable) | `count`, `at` | déclencheur audience (stop-when-idle), courbes d'audience |
| `BroadcastStateChanged` | contrôle diffusion (A2) | `from`, `to` (running/paused/stopped/draining) | réaction à un drain/arrêt |
| `LiveStarted` | DJ live (harbor, `live::LiveHub`) | `dj`, `rule_id`, `at` | annoncer le live, notifier, journaliser |
| `LiveEnded` | DJ live | `dj`, `reason` (disconnected/silence/kicked), `at` | fin d'émission, alerte sur coupure pour silence |
| `TrackStarted` / `TrackFinished` | Liquidsoap | `media_path`, `at` | *vraie* diffusion (vs simple résolution), scrobble, durée d'écoute |

## Ce qui n'est délibérément PAS un événement

- Les **hooks synchrones** `filter_pool` / `on_scan` : ils vivent dans le
  chemin et retournent une valeur.
- Les **requêtes de résolution** (résoudre un pool, choisir une piste) : ce
  n'est pas un fait passé, c'est un calcul.
- Les **erreurs internes / le tracing** : log, pas contrat plugin.

## Encore ouvert

- **`TrackStarted` / `TrackFinished` : définir maintenant ou avec LS ?**
  Les figer tôt risque de mal deviner des champs qu'on ne peut pas encore
  remplir ; le `#[non_exhaustive]` rend l'ajout ultérieur indolore. `TrackResolved`
  (déjà réel) porte le « quoi/pourquoi » dont les stats de rotation ont besoin ;
  la *vraie* diffusion (started/finished) n'a de sens qu'avec LS. **Défaut
  proposé : ne pas les mettre au premier jet, les ajouter avec le câblage LS.**
- **Granularité de `ListenersSampled`** : `count` global suffit au cas « = 0 » ;
  le détail par *mount*/source est un raffinement. **Défaut proposé : `count`
  global d'abord.**
- **Sémantique de livraison** : best-effort (on peut perdre un événement si un
  plugin est lent) vs bufferisé/at-least-once. Best-effort est cohérent avec
  « jamais bloquant » ; l'at-least-once suppose une file par plugin. À trancher
  selon les besoins (les stats de conformité, elles, s'appuient sur le journal
  de diffusion du core — famille B —, pas sur le flux d'événements).
- **Ordre total cross-type** : non garanti au départ ; à durcir seulement si un
  plugin en dépend réellement.
- **Filtrage à l'abonnement** : un plugin déclare-t-il les types qui
  l'intéressent (le core ne lui pousse que ceux-là), ou reçoit-il tout et filtre
  lui-même ? Le second est plus simple ; le premier économise des réveils.
