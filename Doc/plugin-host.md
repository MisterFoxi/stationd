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
| `Stop` | arrête la diffusion | dur (immédiat) |
| `Pause` / `Resume` | suspend / reprend | dur |
| `StopWhenIdle` | **arrêt gracieux armé** : s'arrête à la prochaine occasion propre (fin de piste **et** zéro auditeur), pas un kill | mou (armé) |

`control` est **first-class sur la station** : ces actions existent
indépendamment de tout plugin (`stationctl station stop|pause|resume|
stop-when-idle`), et un plugin peut les invoquer via `host.control(...)`. Un
plugin n'est donc qu'un émetteur parmi d'autres — on doit pouvoir arrêter la
station à la main sans aucun plugin chargé.

`StopWhenIdle` est le cas « MAJ quand plus personne n'écoute » : ce n'est pas
« stop maintenant » mais « stop dès que c'est propre ». Il produit un état de
diffusion `draining` observable (cf. `BroadcastStateChanged`).

Note : l'**état** de diffusion (`running`/`paused`/`stopped`/`draining`) et la
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

### `db_*` — la base du plugin

Chaque plugin a **sa** base, mais **le core l'ouvre pour son compte** : le
plugin n'obtient jamais un chemin ni un handle SQLite, seulement une API
`db_get` / `db_put` / `db_query` scopée à sa base. Conséquences :

- **un fichier par plugin** (isolation : un plugin ne corrompt pas les données
  d'un autre ni du core) ;
- **single-writer préservé** : chaque fichier a un unique writer, le core, pour
  le compte d'un unique plugin ;
- WASM-compatible : pas d'I/O disque dans le plugin, tout passe par la fonction
  hôte.

C'est de la **famille (B) du point de vue du plugin** (état durable qui lui
appartient), mais **hors du périmètre de vérité du core** : un plugin stats
reconstruit ses agrégats au-dessus du journal de diffusion du core, il n'est
jamais la source de vérité de quoi que ce soit d'essentiel à l'antenne.

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
- **Capacités déclarées** : `capabilities = ["control", "push_override"]` dans
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
  L'état de diffusion, lui, est persisté (un `stop` reste un `stop` après
  redémarrage).
- **`StopWhenIdle`** : `draining` devient `stopped` au prochain bord de piste
  si le **dernier** échantillon d'auditeurs vaut 0. Jamais échantillonné →
  aucun arrêt (pas de signal d'audience). `resume` annule un drain.
- **Contrat** : `broadcast_v1.proto` (`GetState` / `Control` /
  `SampleListeners` / `PushOverride` / `ListOverrides` / `ClearOverrides`),
  CLI `stationctl station|override|debug listeners`. `ResolveNext` rend
  `OVERRIDE` (+ `override_source`) ou `HALTED` (+ `halted_state`, aucun média,
  Liquidsoap ne doit pas combler).

## Encore ouvert

- **`db_*`** (tranche suivante) : `db_query` lecture seule + `db_put` atomique
  suffisent-ils, ou faut-il des transactions multi-clés exposées ?
- **Persistance de la file d'override** si un cas réel l'exige (aujourd'hui la
  péremption rend la perte au redémarrage acceptable).
