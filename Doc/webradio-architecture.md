# Réécriture webradio — décisions d'architecture

Document de référence pour le projet de réécriture du principe d'AzuraCast.
Récapitule les décisions prises, pas le détail d'implémentation.

## Contexte

Le fork AzuraCast (PHP/Symfony + Vue) fonctionne mais est devenu difficile à
maintenir : code non commenté, code mort/moribond, infrastructure
surdimensionnée pour un usage mono-station personnel (le poids de Symfony —
DI, event bus, Messenger, i18n, système de plugins/thèmes — sert surtout à
absorber la diversité des installs publiques d'AzuraCast : multi-tenant,
hébergement mutualisé, contributeurs variés — rien de tout ça n'est pertinent
pour cet usage).

Décision : abandonner le patch du fork, réécrire le principe (webradio
auto-hébergée) avec une architecture pensée dès le départ pour ce cas d'usage
précis, plutôt que de viser la parité fonctionnelle complète avec AzuraCast.

## Architecture générale

Trois couches, strictement séparées :

```
navigateur / site public (WordPress possible ici)
        │  REST/JSON, HTTPS
        ▼
      api            ← backend-for-frontend
        │  gRPC (interne, jamais exposé publiquement)
        ▼
    stationd          ← daemon cœur
        │
        ├── Liquidsoap (socket/API)
        ├── Icecast 2.5 (API admin)
        └── SQLite (vue matérialisée — cf. Persistance)
```

- **`stationd`** : daemon cœur, seul composant autorisé à piloter Liquidsoap
  et Icecast. Détient l'état de la station, la programmation des playlists,
  le scan de la bibliothèque média. Politique de *single writer* : aucune
  autre couche ne touche directement au streaming. Voir « Périmètre de
  stationd » pour la frontière exacte de ce qui lui appartient.
- **`api`** : couche intermédiaire (backend-for-frontend). Expose du
  REST/JSON public en HTTPS au navigateur/frontend, gère authentification,
  rôles et validation des entrées, et n'est qu'un traducteur HTTP↔gRPC vers
  `stationd` — **règle stricte : `api` ne doit jamais contenir de logique
  métier qui lui soit propre**, sous peine de rendre certaines
  fonctionnalités inaccessibles au CLI (voir plus bas).
- **Frontend / site public** : client de `api` comme un autre. Peut être en
  WordPress pour la partie éditoriale/vitrine (actus, programmation, player
  embarqué) — WordPress ne doit alors parler qu'aux endpoints publics et en
  lecture de `api`, jamais à `stationd`, et ne doit jamais détenir de compte
  avec des droits d'administration sur la station. L'interface
  d'administration (config station, upload média, playlists, rôles) reste
  hors de WordPress : rendu serveur (templates Rust, ex. Askama/Tera) + htmx
  pour les mises à jour partielles + JS ciblé (Alpine.js, Sortable.js pour le
  drag & drop, SSE pour le now-playing en direct).

`stationd` n'est jamais exposé sur le réseau public.

## Périmètre de stationd

Frontière en une phrase : **stationd possède tout ce dont il est la source de
vérité ou le single writer.** Les plugins construisent du dérivé/optionnel
au-dessus de cette vérité. Les outils externes gèrent ce qui se passe *avant*
l'entrée du média dans le monde de stationd.

**Dans le core :**

- Contrôle live de Liquidsoap et Icecast (seul writer).
- État de la bibliothèque média, des playlists et de la grille de
  programmation.
- **Lecture** des tags au scan (titre/artiste/durée) — nécessaire aux
  playlists, au now-playing, aux règles de rotation, et au scheduler (décider
  si un titre finit avant une borne suppose de connaître sa durée). La leçon
  « échec silencieux » s'applique ici côté lecture : tag manquant/malformé →
  erreur remontée et loggée, jamais un champ vide avalé sans bruit.
  **Périmètre fermé aux champs standard** (titre, artiste, album, genre,
  année, durée, + n° de piste natif) : les seuls que la sélection, la rotation
  et le scheduler consomment. Tout attribut non standard relève d'un plugin
  (cf. hors core) — le catalogue de `field` du filtre playlist reste donc fermé
  et standard.
- **Capture brute de la timeline de diffusion** : flux d'événements (titre X
  démarré à T / terminé, échantillon de listeners relevé sur l'API admin
  Icecast à T). Persisté en SQLite quoi qu'il arrive. En core pour trois
  raisons :
  1. l'historique sert déjà au cœur (now-playing, « récemment joués », règles
     de rotation type « pas de rejeu avant N heures ») — ce n'est pas de
     l'analytics optionnel ;
  2. cohérence avec le sandboxing plugin : un plugin stats crashé/désactivé ne
     doit pas faire perdre silencieusement le journal de diffusion ;
  3. le journal est probablement un artefact de conformité (déclaration aux
     sociétés de gestion, type SACEM/SCPP côté FR — **à vérifier pour la
     situation exacte**), pas juste de l'analytics : il ne doit pas dépendre
     d'un plugin installé.

  Conséquence CLI : historique et export du journal sont dans le contrat gRPC,
  donc atteignables (`stationctl history`, `stationctl export-log …`).

**Hors du core (outils externes, pré-ingest) :**

- **Tagging / édition de tags** : fait mieux ailleurs (Picard, beets —
  fingerprint + lookup MusicBrainz). Le curateur tague avec son outil, dépose
  dans le répertoire média, stationd scanne. Fait disparaître le bug
  historique « écriture de tag mp3 qui échoue en silence » (plus du ressort de
  stationd). Tradeoff assumé : plus de correction de tag in-place depuis
  l'admin — titre faux → on corrige dans l'outil et on re-scanne. Acceptable
  en mono-station perso.
- **Podcasts** : gestion (feeds RSS, téléchargement, rétention) hors scope,
  faite mieux par un podcatcher dédié. Voir « Playlists et programmation »
  pour la frontière fichier vs flux.

**Hors du core (plugins, dérivé) :**

- Stats dérivées : agrégats, dashboards, pic/moyenne d'auditeurs, durée de
  session, export vers service externe, scrobble ListenBrainz… construits
  au-dessus de la capture brute.
- **Attributs média non standard** : tags custom arbitraires (mood, campagne
  publicitaire datée, n° de séquence propre, flags fonctionnels…) *et* le
  critère de sélection qui les exploite. Enrichir un média avec des clés hors
  du catalogue standard est du dérivé → plugin ; le mettre dans le cœur
  rouvrirait une surface de contrat générique (`field: tag:*`, table
  clé/valeur) contraire au principe « path-first, un seul concept ». Les
  besoins courants se couvrent **sans plugin** par le rangement en dossiers
  (`ads/`, `jingles/id/` → filtre `path prefix`) et l'ordre daté
  (`order_by = "filename"`, ou n° de piste natif) ; le plugin n'est requis que
  pour un attribut qui n'est ni une catégorie de rangement ni un ordre.

**À trancher (tag-adjacent) :** ReplayGain/R128 (normalisation loudness) et
cue points. Soit pré-ingest (l'outil externe calcule et écrit, stationd lit),
soit à la volée côté Liquidsoap. À décider explicitement, sinon ça retombe
insidieusement dans stationd.

## CLI = interface complète, et CLI-first

Exigence forte : l'intégralité des fonctionnalités du backend doit être
pilotable en CLI, sans aucune dépendance graphique (référence : `git`, qui
n'a besoin d'aucune interface graphique pour être complet), notamment pour un
déploiement bare-metal sans environnement de bureau.

Le CLI (construit avec `clap`) est un **client gRPC direct de `stationd`**,
au même titre que `api` — il ne passe pas par la couche HTTP publique,
puisqu'il tourne en local avec un accès admin de confiance (comme la CLI
`docker` qui parle directement au daemon plutôt que de repasser par une API
web).

**CLI-first** : la complétude CLI n'est pas seulement une garantie *a
posteriori*, c'est l'**ordre de conception**. On définit d'abord la capacité
au niveau du contrat gRPC (que le CLI consomme directement), l'UI web vient
après.

Conséquence directe de la règle "`api` = pur traducteur" : toute
fonctionnalité doit d'abord exister dans le contrat gRPC de `stationd`. Si
elle n'existe que dans le code de `api` (ou du frontend), le CLI ne peut
jamais l'atteindre — la garantie de complétude du CLI ne tient que si cette
discipline est respectée à chaque nouvelle fonctionnalité.

Corollaire concret pour tout ce qui touche au temps : le scheduler doit être
**simulable sans attendre l'horloge** (`stationctl schedule preview --at … /
--next 24h`). Une grille qu'on ne peut observer qu'en attendant le wall-clock
est une grille qu'on ne peut pas tester — cf. « Gestion du temps ».

## Playlists et programmation

### Playlists

- **Source = fichiers TOML, un par playlist** (diffable, relisible,
  reproductible, versionnable — cohérent avec l'argument `git` et serde
  natif). Persistance file-first : le TOML est canonique, la base est une vue
  matérialisée. Voir « Persistance ».
- **Toujours dynamiques** : une playlist est *résolue à la lecture* depuis
  l'état de la bibliothèque (une requête + une politique de sélection). Une
  tracklist ordonnée explicite n'est qu'un cas dégénéré : « ces IDs, dans cet
  ordre, mode séquentiel ». Un seul modèle, le statique en sous-cas. Mappe
  bien sur les sources/rotations dynamiques natives de Liquidsoap.
  - Comportement sur **ensemble vide** : une requête qui ne matche rien à
    l'instant T doit avoir un fallback défini côté Liquidsoap, sinon trou
    d'antenne. No-silent-failure appliqué au live.
  - Tension avec le drag & drop : on ne réordonne pas un résultat de requête.
    Le drag & drop s'applique à l'ordre *dans* une sélection explicite ou à la
    composition des groupes, pas à une PL purement requêtée.
- **Playlists groupées = composition** : une playlist faite d'autres
  playlists, avec une politique de rotation/poids/planning au niveau du groupe
  (ex. bloc « journée » alternant jazz / ambient / jingles). Blocs
  réutilisables → grille entière construite par assemblage. Idée reprise
  d'AzuraCast, jugée très puissante.
  - Contrainte de validation : **détection de cycles** (un groupe qui se
    référence directement ou transitivement = boucle infinie) + résolution des
    références cross-fichiers TOML.

**Une entrée de playlist est toujours un média concret (fichier ou URL
résolue), jamais une intention à résoudre au runtime.** Règle structurante,
cf. podcasts ci-dessous.

### Podcasts

Hors scope en tant que gestion (feeds, téléchargement, rétention), mais un
épisode peut faire partie d'une playlist — via **podcast-as-fichiers**
uniquement :

- Un podcatcher externe télécharge les épisodes dans un répertoire (NFS
  partagé). stationd monte ce répertoire en **lecture seule** et le scanne
  comme n'importe quel média. Podcatcher = single writer, stationd = reader.
  Zéro scope podcast dans le core — juste un répertoire média de plus.
- **Écarté : podcast-as-flux** (« joue le dernier épisode du feed RSS X »), qui
  ferait rentrer fetch réseau + dépendance externe + mode d'échec au moment
  critique de l'antenne. La résolution RSS → fichier vit entièrement côté
  podcatcher, en amont du répertoire NFS. stationd voit un `.mp3`, point.
- Réconciliation (le podcatcher fait de la rétention) : épisode disparu au
  scan → marqué indisponible (même mécanisme que tout fichier supprimé de la
  biblio) ; épisode supprimé/en réécriture au moment de la diffusion → même
  filet `on-empty`/fallback que les PL dynamiques, skip loggé, jamais un trou
  muet.
- Note déploiement (pas archi) : côté podcatcher, privilégier une écriture
  atomique (fichier temporaire + `rename`) pour que stationd ne scanne/ouvre
  jamais un fichier à moitié téléchargé.

### Scheduler

Lance les playlists selon la grille. Deux régimes distincts — les confondre
est le piège classique de l'automation radio.

- **Régime pull (≈ 90 % de la grille)** : Liquidsoap, quand son titre courant
  se termine, redemande le suivant à stationd. stationd regarde *à ce
  moment-là* l'horloge → playlist active → résolution dynamique → renvoie un
  média concret. Gracieux par construction (on ne coupe jamais un titre, on
  change juste ce qu'on tend ensuite), single-writer propre, et c'est
  littéralement « playlists toujours dynamiques, résolues à la lecture ». Les
  bornes de bloc musical sont *molles* : le changement se fait au titre
  suivant, personne n'entend la différence.
- **Régime interrupt (≈ 10 %, seul vrai « timestamp imposé »)** : flash info à
  08:00 pile, top horaire légal, jingle daté. Le pull-à-la-fin-du-titre ne
  peut pas le garantir à la seconde → **source interruptrice côté Liquidsoap**
  (prédicat temporel, avec duck/fade plutôt que coupe sèche en général).

Ne pas modéliser *toute* la grille en timestamps durs : la plupart des bornes
sont molles, seuls quelques événements sont réellement à l'horloge.

**La grille doit être totale** : tout instant non couvert = trou d'antenne.
D'où une **playlist par défaut/fallback obligatoire** qui remplit tout ce que
la grille ne couvre pas. Même filet que le `on-empty` et l'épisode manquant,
appliqué à l'axe temps.

**Modèle de priorité** (résolution de conflit, priorité croissante) :

1. fallback (défaut, remplit les trous)
2. grille récurrente (jazz 8–10h, lun–ven)
3. one-shot planifié (cet événement, cette date, une fois)
4. interrupt/override (flash info horaire, live DJ, annonce d'urgence)

Le plus prioritaire actif gagne. Sans cet ordre, deux entrées qui se
chevauchent ont un comportement indéfini.

**Scheduler ≠ playlist groupée** : une groupée porte une politique de
*rotation* (poids/séquence, sans horloge) ; le scheduler porte une politique
*temporelle avec priorité et override*. La grille est un concept top-level
distinct qui *référence* des playlists (éventuellement groupées) — sémantiques
propres (priorité, one-shot, interrupt) qu'une playlist n'a pas. Structures
séparées.

## Gestion du temps

Principe : **epoch UTC en interne, conversion uniquement à la présentation.**
Les maths de temps (comparaisons, ordonnancement des interrupts, « ce titre
finit-il avant la borne ») se font sur une droite monotone, sans heure qui
existe deux fois ou zéro fois. L'ambiguïté DST ne peut pas vivre dans le
core ; elle est repoussée à la seule frontière où elle a un sens : la
saisie/affichage.

Deux natures de temps distinctes, portées par deux types distincts :

- **Instants** (one-shot daté, top horaire, log de diffusion, échantillon
  listeners) → epoch UTC. Aucune ambiguïté. Majorité des données.
- **Règles récurrentes** de la grille (« jazz 08:00, lun–ven ») →
  `(heure civile, fuseau IANA, récurrence)`. Une règle en wall-clock ne se
  stocke PAS en offset gelé (`08:00 Europe/Paris` = 07:00 UTC l'hiver, 06:00
  l'été) : on conserve le nom de fuseau IANA, et l'expansion règle → instants
  epoch se fait à la volée en appliquant le DST en vigueur pour chaque
  occurrence.

Représentation Rust :

- Jamais d'`i64` epoch nu (temporel « stringly-typed » → réintroduit l'échec
  silencieux : addition de deux timestamps, s/ms mélangés). Type wrappé
  obligatoire.
- **`std::time::Instant` (horloge monotone) réservé aux mesures de durée**
  (échantillonnage, timeouts, « dans N s ») — jamais pour pointer un instant
  du calendrier (non sérialisable, pas de date). Wall-clock/epoch = « quand » ;
  monotone = « depuis combien de temps ».
- Crate temps à choisir (statut = décision de stack, cf. tableau) : la
  séparation instant/règle mappe sur `jiff` (`Timestamp` vs `Zoned`/`civil`)
  ou `chrono` + `chrono-tz` (`DateTime<Utc>` vs civil + `Tz`). À trancher.

Frontières :

- Fil gRPC : `google.protobuf.Timestamp` pour les instants ; message
  `{ heure civile, fuseau IANA, récurrence }` pour les règles (les deux
  doivent exister au contrat, cf. CLI complet).
- SQLite : epoch en `INTEGER` ; règle récurrente = heure civile + chaîne de
  fuseau.

Fuseau de la station = **config de station** (un seul fuseau de référence par
nœud, cohérent avec « un nœud = une station autonome »). « Local » n'a de sens
que défini une fois pour la station — une webradio est écoutée depuis
plusieurs fuseaux.

HMI :

- Double horloge si local ≠ UTC, avec **fuseau nommé** et non offset
  (« 08:00 CET / 07:00 UTC », pas « 08:00 +01:00 »).
- La double horloge est aussi le **point de validation de saisie** : à la
  création d'un événement sur la borne de bascule, l'UI lève l'ambiguïté au
  moment de la saisie (« 02:30 n'existe pas / existe deux fois cette nuit-là »)
  — seul instant où un humain peut trancher l'intention. En aval, en epoch,
  l'information est perdue par construction.

CLI : `stationctl schedule preview --at … / --next 24h` simule la grille sans
attendre le wall-clock, affiche chaque occurrence en UTC **et** en local, et
sert de test anti-DST (une fenêtre sur la nuit de bascule révèle trou ou
doublon).

## Persistance : file-first

**Les fichiers TOML sont la source de vérité. SQLite est une vue matérialisée
reconstructible.** Playlists et grille sont canoniques sous forme de fichiers
(diffables, versionnables git, reproductibles) ; la base sert à servir les
lectures rapides (requêter une grille sur un tas de TOML est pénible — la vue
rend le drag & drop et la prog rapides).

**stationd est le seul writer du TOML.** Le frontend n'écrit jamais de
fichier ni n'ouvre son propre handle SQLite — il *envoie les modifs* à
stationd et *demande* les lectures, toujours via `api`/gRPC.

```
frontend (drag&drop) → REST → api → gRPC UpdatePlaylist → stationd
                                                            ├── régénère le TOML (toml_edit, lossless)
                                                            └── met à jour la vue DB
```

Même méthode gRPC ⇒ `stationctl playlist edit …` fait exactement le même
write. Single-writer préservé, CLI complet préservé, file-first respecté. Un
frontend qui écrirait lui-même le TOML mettrait la capacité d'édition hors du
contrat gRPC (CLI aveugle), casserait le single-writer et rouvrirait la
divergence silencieuse — écarté.

Ce que file-first rend obligatoire :

- **Reload / réconciliation sur édition externe.** Un humain qui édite un
  `.toml` dans son éditeur ou fait un `git pull` est un chemin d'écriture
  *légitime et de premier rang*. stationd surveille le répertoire (watch) ou
  recharge sur commande (`stationctl reload`) et reconvertit vers la vue DB.
  Les deux writers légitimes du TOML : **un humain** et **stationd** — jamais
  le frontend.
- **Round-trip lossless obligatoire** (`toml_edit`) : la régénération ne doit
  pas écraser commentaires/formatage, sinon les diffs git deviennent du bruit
  et on perd la moitié de la raison d'avoir choisi file-first.
- **Validation au chargement, no-silent-failure** : fichiers éditables à la
  main → peuvent arriver invalides. TOML malformé ou référence média cassée au
  load → erreur bruyante, on garde le dernier état valide ou on tombe sur le
  fallback, **jamais** une playlist silencieusement vide. Validation =
  schéma serde **+ intégrité référentielle** (médias référencés existent) +
  détection de cycles (groupées). CRUD/validate/import/export dans le contrat
  gRPC → atteignables au CLI.
- **Concurrence régénération-UI vs édition-humaine** (détail d'implémentation,
  pas archi) : last-writer-wins avec détection de `mtime` — refuser/recharger
  si le fichier a bougé sous stationd depuis la lecture.

## Stack technique

| Composant | Choix | Raison |
|---|---|---|
| Backend | Rust | typage fort / `Result` explicite — évite la classe de bugs à échec silencieux rencontrée sur le fork (ex. écriture de tag mp3 qui échouait sans erreur loggée) |
| Framework web (`api`) | Axum | même écosystème (`hyper`/`tower`) que `tonic`, cohérence technique avec le canal gRPC interne |
| IPC interne `api` ↔ `stationd` | gRPC (`tonic`) | contrat typé (protobuf), réduit la surface de parsing malformé. Sans mTLS pour commencer (réseau Docker interne non exposé), mTLS activable plus tard si le threat model change |
| Frontend public | REST/JSON classique | gRPC non utilisable nativement depuis un navigateur sans grpc-web + proxy — complexité jugée disproportionnée |
| Crate temps | à trancher : `jiff` **ou** `chrono` + `chrono-tz` | doit matérialiser en deux types distincts la séparation instant (epoch UTC) / règle récurrente (civil + fuseau IANA) — cf. « Gestion du temps ». Maturité/adoption relative à revérifier au moment de committer |
| Persistance | SQLite embarqué dans `stationd`, en **vue matérialisée** (source = TOML, cf. Persistance) | chaque nœud est autonome (voir déploiement) — pas de coordination cross-station, pas de service DB séparé |
| Config & playlists/grille | TOML | écosystème Rust natif (Cargo l'utilise), bon support serde, moins de pièges que YAML (coercions de types, indentation significative) ; canonique en file-first, diffable git |
| Conteneurisation | Docker, build multi-stage | sécurise un environnement d'exécution connu ; image finale = binaire Rust statique + Liquidsoap + Icecast, sans la mécanique composer/npm qui posait problème sur le fork PHP |
| Plugins | WebAssembly (`wasmtime` / `extism`), cible `wasm32-unknown-unknown` | ajoutent des fonctionnalités (pas de traitement audio temps réel) ; un seul binaire cross-OS, sandboxé — un plugin buggé ne peut pas planter `stationd` |

## Rôles et permissions

Conservés (utile même en mono-station : co-DJ, modérateur), mais simplifiés
par rapport à AzuraCast : un enum de rôles fixe par station (ex. `Owner`,
`Operator`, `Viewer`), pas de matrice utilisateur × station comme dans un
système multi-tenant.

**Différé** : liste précise des rôles et de leurs capacités à définir plus
tard, une fois qu'on aura un proto qui tourne — le besoin réel se verra à
l'usage plutôt qu'en spéculant maintenant.

## Modèle de déploiement

Un nœud = une station autonome : VM (Proxmox) ou machine bare-metal, le
choix ne change rien à l'application elle-même (Docker ou du natif via
systemd fonctionnent identiquement dans les deux cas). Médias partagés entre
nœuds via NFS (truenas.lan), **monté en lecture seule côté stationd**.
Configuration et état propres à chaque nœud — pas de base partagée entre
stations, d'où le choix SQLite. Fuseau horaire de référence = config de
station (cf. « Gestion du temps »).

Accès NFS unidirectionnel : stationd ne fait que lire les médias (biblio,
épisodes de podcast déposés par un podcatcher externe qui, lui, est le single
writer de son répertoire). Deux writers sur le même fichier = à proscrire.

## Points de vigilance généraux

- Le risque NFS historique (verrouillage/latence/corruption sur **écriture**
  de métadonnées) tombe largement : stationd ne fait que lire, le tagging est
  pré-ingest. Reste la vigilance sur le fichier qui bouge sous les pieds de
  stationd (rétention podcatcher, remplacement de média) → réconciliation au
  scan + `on-empty`/fallback à la diffusion (cf. Playlists).
- Rust itère plus lentement que PHP en dev/débogage exploratoire.
- Écosystème ORM/migrations Rust (sqlx, sea-orm) correct mais moins
  clé-en-main que Doctrine.
- Un hébergement de plugins audio natifs (VST3/CLAP tiers) serait un projet
  à part entière (formats natifs, binaire par OS, cassage du sandboxing) —
  écarté pour l'instant, les plugins WASM couvrent le besoin réel exprimé
  (ajout de fonctionnalités, pas de traitement audio temps réel).

## Encore ouvert

- Liste précise des rôles et de leurs capacités (différé au proto — cf.
  Rôles).
- Points d'accroche exacts des plugins (hooks côté `stationd`) et fonctions
  hôte à leur exposer. Deux accroches déjà identifiées par le besoin
  « attributs média non standard » (cf. Périmètre) : (a) enrichir un média au
  scan — lire des clés hors catalogue standard et les persister en dérivé ;
  (b) apporter un critère / proposer / filtrer une sélection. Où vit le
  dérivé (store possédé par le plugin vs table clé/valeur du core peuplée via
  fonction hôte) reste à trancher.
- mTLS sur le canal gRPC interne (reporté).
- Fonctionnalités précises d'AzuraCast à porter (probablement sous forme de
  plugins) plutôt qu'à réinventer.
- Choix de la crate temps : `jiff` vs `chrono` + `chrono-tz` (à trancher au
  moment de committer).
- ReplayGain/R128 et cue points : pré-ingest (outil externe) vs à la volée
  (Liquidsoap) — à décider explicitement pour éviter que ça retombe dans
  stationd.
