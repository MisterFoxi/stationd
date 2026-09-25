# État d'avancement — passation entre sessions

Journal de ce qui est **fait** et ce qui **reste**, pour reprendre le travail
sans reconstruire le contexte. À distinguer des docs de `Doc/` (décisions
d'architecture durables) : ce fichier-ci est volatil, à mettre à jour à
chaque session.

Dernière mise à jour : 2026-09-25.


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

**Ça diffuse (2026-09-24).** stationd pilote Liquidsoap 2.4 → Icecast 2.5 sur
devstationd : script `.liq` généré par stationd, pull de chaque piste par un
pont HTTP loopback (`/ls/v1/next`, `/ls/v1/track`), contrôle par socket
(`pause` immédiate avec piste gelée, `resume`, `next`, stop à la fin de la
piste en cours, override soft au prochain bord, override hard coupant), bruit
de fond quand la station est arrêtée/en pause, fallback de sécurité si rien à
diffuser ou stationd injoignable. `stationctl ls status` : `on air`, `next`,
état de l'antenne, santé du socket. Étapes 1 et 2 du câblage terminées ;
reste l'étape 3 (tâche de diffusion) et 4 (fins de piste). Référence :
`Doc/liquidsoap.md`.
**Icecast lu (2026-09-24).** stationd lit `/admin/stats` d'Icecast 2.5
(`[icecast]`) : audience réelle de ses mounts → `ListenersSampled`
(stop-when-idle validé de bout en bout en réel), santé des mounts dans
`stationctl icecast status`. Lecture ratée = audience **inconnue**, jamais 0.
**stationd génère `icecast.xml` (2026-09-25)** avec `[icecast.server]`
(mounts + mots de passe par sortie, UTF-8, proxys de confiance), comme le
`.liq` ; validé sur la 2.5 de devstationd.
**Docker (2026-09-25).** stationd + Icecast + Liquidsoap tournent dans un
conteneur de dev (s6-overlay, dépôt monté dans `/src`, réseau de l'hôte) —
remplace l'installation systemd. README « Run (Docker) »,
`Doc/liquidsoap.md` « Déploiement ».

La **grille est pilotable de bout en bout en CLI**, projection comprise :
`grid.toml` (4 familles) → `stationctl schedule validate|apply|export|list|next|preview|check`
→ gRPC → moteur → SQLite. Testé en vrai : apply de 4 règles, `next` rend
`AT_CLOCK_SOFT`, export round-trip stable, `preview` projette 24h avec rendu
UTC + local nommé (test anti-DST 2026-10-25 : 02:30 deux fois, epochs
distincts). Cœur pur (`resolve_next`, `clock`/DST `jiff`, familles A/B) vert,
grammaire + les 7 RPC réels. `cargo build` + `cargo test -p stationd` étaient verts
au dernier build compilé ; les ajouts récents (shuffle/runtime de groupe,
décomposition preview, fix casse de ref) **attendent un `cargo build`** (Rust
indispo dans l'env de préparation).
Biblio média scannée et pilotable au CLI (`library scan|list|genres`, filtre `--genre`). **Étage
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

Environnement : tout se teste désormais dans le conteneur de dev (README
« Run (Docker) ») ; plus d'unités systemd sur devstationd.

**Câblage Liquidsoap — étape 3 : tâche de diffusion** (stationd agit sur
l'antenne sans attendre un pull) :
- **relais des flux `remote`** : ✅ (2026-09-25, voir Fait). Ancienne note :
  `input.http` piloté par stationd (démarrage /
  arrêt quand la règle gagnante change) — aujourd'hui `/next` répond
  `none/stream_unsupported` → fallback ;
- **`AtClock hard`** : ✅ coupe à l'heure pile (2026-09-25, voir Fait) ;
- **résolution à l'heure réelle de passage** : ✅ (2026-09-25, voir Fait —
  piste suivante demandée ~7 s avant la fin de la courante).

Puis **étape 4 — fins de piste** : `unplayed_only` marqué automatiquement
✅ (2026-09-25, voir Fait) ; restent `TrackStarted`/`TrackFinished` aux
plugins, compteur `Every` exact.
Côté Icecast (échantillonnage livré, voir Fait) :
- **stop-when-idle sans piste de trop** : largement réglé par la demande en
  fin de piste (point 5) — le pull, qui décide `stopped`, n'arrive plus que
  quelques secondes avant la fin. Reste le cas d'un échantillon à 0 reçu
  après cette demande. Le `flush` pendant `draining` n'a plus d'intérêt ;
- `X-Forwarded-For` : **tranché** (2.5 : sockets virtuels, cf.
  `Doc/liquidsoap.md`) ;
- auditeurs **par mount** dans `ListenersSampled` (change le contrat
  `Doc/plugin-events.md`, tranche séparée).

**Preview — statistiques de pool : patch préparé et testé (2026-09-19).**
Voir la section Fait ci-dessous et `Doc/preview-pools.md`.

**Plugins A2 — surface hôte : livré et validé (2026-09-23).** Voir Fait.
Reste de la surface hôte : **`db_*`** (base SQLite par plugin, ouverte par le
core pour son compte : `db_get`/`db_put`/`db_query`).

Autres, indépendants :
- **`on_scan`** : câblé (2026-09-24). Le scan collecte les tags personnalisés
  (`TXXX`, clés Vorbis/APE, MP4 freeform — `media::CustomTag`, non persistés),
  les passe en lot à `on_scan` (natif + export WASM) ; les genres renvoyés sont
  fusionnés dans `media_genre` (dédup `genre_key`). Plugin
  `plugins/custom-tags-wasm` (`[plugin.config] tags = ["Type"]`) :
  `TXXX:Type = talks` → genre `talks`. `.wasm` à compiler (cible wasm32).
- **Crate de types partagé** `Candidate`/`PluginEvent` (host + guests wasm ne
  les dupliquent plus — aujourd'hui recopiés dans les 2 crates guest).
- **Refacto acteur `GridEngine`** (gabarit `library_actor`).
- **Câblage Liquidsoap** : étapes 1 et 2 terminées (2026-09-24, voir Fait +
  `Doc/liquidsoap.md`) ; étape 3 = tâche d'entrée ci-dessus.
- **Redémarrage de stationd = pont amnésique** (constaté en réel le
  2026-09-25) : l'état du pont (pulls, `on air`, pistes préparées) est en
  mémoire ; Liquidsoap continue sa piste et ne rappelle `/next` / `/track`
  qu'au début de la suivante. Conséquences : `ls status` vide jusqu'au
  prochain changement de piste ; la piste préparée par l'ancienne instance
  démarre en `unknown request id` ; ces deux pistes ne sont **jamais
  marquées `unplayed_only`**. Piste proposée (non tranchée, à faire plus
  tard) : (1) pistes auto-décrites — l'annotation porte aussi média et
  feuille, renvoyés sur `/track` ; (2) resynchronisation au démarrage — demander
  à Liquidsoap, par le socket, ce qui est à l'antenne et depuis quand.
- Journaux : stationd en **UTC** (`…Z`), Liquidsoap en heure locale —
  lecture croisée pénible ; journaliser stationd en heure locale (à faire).
- Petits restes Liquidsoap : fondu sur la coupe d'un override hard.
  (Validation au démarrage — jeton ASCII, fichiers de secours / bruit :
  faite le 2026-09-25, voir Fait. Bloc `[[plugin]]` égaré de `Cargo.toml` :
  retiré le 2026-09-25.)

---

## Fait

### — Validation au démarrage : jeton et fichiers de Liquidsoap (2026-09-25) —

Point 7 de la 0.1 (dernier indispensable).

- **`LiquidsoapConfig::validate`** : `api_token` en ASCII imprimable
  (`printable_ascii`, comme les mots de passe Icecast) — refus au chargement.
- **`LiquidsoapConfig::check_air_files()`** (E/S, appelé par `main.rs`
  avant l'écriture du script ; remplace l'avertissement « does not exist ») :
  `fallback_path` / `halted_path` absent, pas un fichier ordinaire, vide ou
  illisible par stationd → **démarrage refusé** ; non lisible par « les
  autres » → avertissement avec mode, uid, gid (Liquidsoap tourne sous un
  autre utilisateur, stationd ne peut pas vérifier ses droits ; cas du
  `error.mp3` en `660 foxi:foxi` vu en Docker).
- En Docker : stationd refusé → jamais « prêt » → Icecast et Liquidsoap ne
  démarrent pas sur une config bancale ; s6 relance stationd (erreur au
  journal à chaque tentative).
- Tests (+2) : jeton `…` et tabulation refusés ; fichiers bon / `660`
  (avertissement) / absent / dossier / vide / illisible (ce dernier sauté
  en root).

**Validation** : `cargo test --locked` vert (358).

**La 0.1 « indispensable » est complète** (points 1 à 8). Restent les
souhaitables : `TrackStarted` / `TrackFinished` aux plugins, compteur
`Every` exact ; et les notes « plus tard » (pont amnésique au redémarrage,
journaux en heure locale, TUI).

### — `day_part` à fin ouverte (2026-09-25) —

Décisions : `end` facultatif ; une tranche sans `end` court jusqu'au
prochain début d'une **autre** tranche (ouverte ou non) ; une fenêtre
explicite qui démarre dedans la termine **pour de bon** (option a, pas de
reprise) ; ses `days` / dates s'évaluent sur le **jour de son début** ;
seule dans la grille, elle tourne en permanence (pas d'avertissement à la
validation — `schedule validate` n'a pas de canal d'avertissement).

- **Résolveur** (pur) : `DayPart.end: Option<WallClock>` ;
  `open_part_covers` (dernier début ≤ now, 7 jours en arrière, validité du
  jour de début ; couverte si aucune autre tranche n'a démarré depuis) ;
  `Validity::applies_on`, `Date::prev` (calendrier grégorien),
  `Weekday::prev`. Classement : une tranche ouverte compte comme la plus
  large (une fenêtre explicite déjà commencée l'emporte).
- **Grammaire** : `end` facultatif, export sans `end` ; **schéma JSON**
  (`schemas/grid.schema.json`) aligné. **Stockage** : migration **0016**
  (`grid_day_part` reconstruite, `end_*` NULL ensemble).
- **Dimensionnement** (`schedule check`) : durée nominale = jusqu'au
  prochain début d'une autre tranche sur l'horloge (jours ignorés), 24 h
  si seule.
- gRPC : `DayPart.end` absent. TUI : affiche `-` (compile, sans autre
  changement — la TUI est traitée plus tard).
- Tests (+10) : chaîne de tranches ouvertes (dont minuit), fenêtre
  explicite qui termine une ouverte, fenêtre déjà commencée prioritaire,
  jours du début (samedi → lundi), tranche seule, dates, `Date::prev` ;
  grammaire aller-retour ; stockage NULL ; dimensionnement.

**Validation** : `cargo test --locked` vert (357). Migration 0016 neuve :
au premier démarrage elle reconstruit `grid_day_part` (la grille reste en
place).

### — Relais des playlists `remote` (2026-09-25) —

Point 2 de la 0.1. Décisions : `input.http` dans le script généré, piloté
par les réponses du pull (pas de tâche stationd en plus) ; entrée **soft**
(fin de la piste en cours) ; le relais tient tant que la grille répond
`relay` ; sortie dans les ~2 s.

- **Pont** : `NextReply::relay(url)` (au lieu de `none/stream_unsupported`),
  `OnAirKind::Relay` (`media_path` = URL), URL loguée au changement
  seulement. Moteur inchangé.
- **Script** : `input.http(start=false, {stationd.relay_url()})`, premier de
  la chaîne mais disponible seulement quand le pull croisé n'a plus rien ;
  `relay_on` / `relay_off` (refs de fonctions) ; `pull_started` arrête le
  relais ; `thread.run` toutes les 2 s → `pull_raw.fetch()` pendant le
  relais (constaté : `request.dynamic` ne redemande plus quand il n'est pas
  lu) ; transition `relay` signalée sur `/track`.
- Limites documentées (`Doc/playlists.md`) : `remote` en `at_clock` /
  `every` = une interrogation (~2 s) ; en groupe, `runtime` requis ; skip
  sans effet pendant un relais.
- Tests (+4) : pont (relay + URL, rien en file, `on air` relay, retour à une
  piste) ; piste remplacée par un relais jugée normalement ; script (source,
  place, garde de disponibilité, branches, veille).
- **Essai réel Liquidsoap 2.2.4** : entrée à la fin exacte de la piste
  (traîne du crossfade comprise — la première version la coupait puis la
  rejouait), veille toutes les 2 s, sortie vers piste / bruit, insert hard
  par-dessus.

**Validation** : `cargo test --locked` vert (347). À valider sur la 2.4 :
une règle `day_part` sur une playlist `remote` (URL d'un flux réel) —
entrée à la fin de la piste, `ls status` `on air: relay …`, sortie à la fin
de la tranche. Relancer Liquidsoap (nouveau script).

### — Piste suivante choisie à l'heure réelle de passage (2026-09-25) —

Point 5 de la 0.1. Décision : Liquidsoap ne demande la suivante qu'en **fin
de piste** — seul le script généré change, aucune logique stationd. Écarté :
re-résoudre côté stationd par `flush` (la résolution a des effets de bord —
curseur, quota de groupe, jeton `AtClock`, historique — qu'une piste jetée
fausserait).

- **`ls_script`** : `stationd.next` répond `null` sans appel HTTP tant que
  `pull_raw.remaining() > stationd.lead` et pas `urgent` ;
  `pull_lead_s()` = crossfade + retry (2 s) + `PULL_LEAD_MARGIN_S` (2 s) =
  7 s par défaut ; `stationd.remaining` (ref de fonction, branchée après la
  définition du pull) ; `cmd_skip` : si rien n'est préparé, `urgent` +
  `pull_raw.fetch()` avant `source.skip`.
- Tests : +1 (valeur de `lead`, garde avant l'appel, branchement de
  `remaining`, ordre `fetch` → `skip`).
- **Essai réel sur Liquidsoap 2.2.4** (ici ; 2.4 absente) : script généré,
  syntaxe `null` → `null()` et `source.methods(x).on_track(…)` →
  `x.on_track(…)` adaptées pour la 2.2 seulement, faux stationd Python,
  pistes de 20 s, crossfade 3 s : `/next` à 7 s de chaque début de piste
  (au lieu de 17 s), skip → suivante immédiate sans fallback, interrupt →
  pull redemandé à la coupe, joué après l'insert.

**Validation** : `cargo test --locked` vert (344). À valider sur la 2.4 de
devstationd : relancer Liquidsoap (nouveau script) ; `ls status` → `next`
vide la plupart du temps, rempli dans les dernières secondes ; journal
stationd : les pulls arrivent ~7 s avant chaque `on air` ; un skip enchaîne
sans fichier de secours.

### — `AtClock hard` : coupe à l'heure pile (2026-09-25) —

Point 4 de la 0.1. Décisions : une **tâche horloge** dans stationd (pas dans
Liquidsoap) ; la coupe reprend celle de l'override hard (`flush` +
`interrupt`) ; coupe seulement à **≤ 10 s** du repère, station à l'antenne,
repère non consommé — sinon **dégradé en soft** (jeton laissé libre).

- **`GridEngine::next_hard_mark(now)`** : frontières de minute, résolveur
  réduit aux `AtClock` hard + état réel ; un repère « tombe » sur la minute
  si son jeton porte ce HH:MM (un repère plus ancien encore dans son
  `expiry` n'est pas un nouveau repère) ; fenêtre 60 min ; repère passé de
  ≤ 10 s encore rendu.
- **`GridEngine::air_at_clock_hard(mark, now)`** : garde (retard, gate,
  jeton de CE repère), résolution partagée avec le pull (`produce`, extrait
  de `next_media` : plugins, contraintes, repioche d'un fichier disparu),
  `persist_effects` (jeton), `log_start`, événement plugin.
- **`AirEvent::HardMark { at }`** ; `ls_control::spawn_at_clock_ticker`
  (réveil sur l'horloge système, correction sub-seconde, ≤ 60 s, un envoi par
  repère) ; `cut_in_at_clock` + `cut` (commun avec l'override hard) ;
  `LsBridge::at_clock_uri`. Branché dans `main.rs` avec `[liquidsoap]`.
- Constantes : `HARD_CUT_LATE_S = 10`, `HARD_MARK_LOOKAHEAD_MIN = 60`,
  `TICK_MAX = 60 s`.
- Tests (+7) : prochain repère (soft ignoré, repère exact et tout juste
  passé, jeton consommé sauté, aucune règle hard) ; coupe à l'heure (jeton
  consommé, pull suivant sans doublon) ; en retard / en avance (pas de
  coupe, soft ensuite) ; station en pause (pas de coupe, soft après
  reprise) ; tâche horloge → `flush` + `interrupt` une seule fois ; pause →
  aucune commande.

**Validation** : `cargo test --locked` vert (343), aucun nouvel
avertissement clippy dans le code touché. À valider en réel : une règle
`at_clock` hard (ex. `every_minutes = 5`) coupe la piste en cours à la
seconde du repère (`AtClock hard cut in` dans le journal).

### — `unplayed_only` : marquage automatique en fin de piste (2026-09-25) —

Point 3 de la 0.1. Décisions : la fin de piste est **déduite côté stationd**
(Liquidsoap ne signale que les débuts ; script `.liq` inchangé) ; « jouée en
entier » = **temps réellement à l'antenne** (pauses exclues) `+ 15 s ≥`
durée indexée ; la marque porte la **playlist feuille**.

- **`selection::Resolved::File { path, leaf }`** : la feuille (statique /
  dynamique / queue) qui a produit le fichier, clé canonique ; `None` pour
  un override média. → `ResolvedDecision::leaf_ref` → `Pending` / `OnAir`
  du pont.
- **Pont** (`ls_bridge`) : `OnAir` compte `aired_s` + `counting_since` ;
  une piste quitte l'antenne quand autre chose démarre (piste, `rid`
  inconnu, fallback, bruit de fond hors pause) ; bruit de fond **en état
  `paused`** = gel (`before_halt`), `resume` relance le compteur ; gelée
  puis abandonnée = sortie avec le temps gelé. Appels moteur hors verrou.
- **`GridEngine::on_track_left(media, leaf, aired_s, now)`** +
  `played_to_end` (pur, `PLAYED_TO_END_TOLERANCE_S = 15`) →
  `on_episode_finished` (existant) ; `media_index::duration_ms_of`.
- `ls_control` : le « retour de pause » est aussi reconnu depuis `stopped`
  (stop poussé pendant une pause : la piste gelée reprend au `resume`).
- Comportement changé : le bruit de fond ne gèle la piste que si l'état est
  `paused` (avant : toujours) ; après un `stop`, la piste est terminée.
  Test `resume_from_pause_…` ajusté (pause appliquée d'abord).
- Tests (+9) : feuille portée à travers un groupe (sélection) ;
  `played_to_end` ; `on_track_left` (coupée / override / inconnue /
  entière) ; pont : jouée en entier via un groupe → marque sur la feuille,
  skip, pause puis abandon, pause puis reprise jusqu'au bout, stop, fallback.

**Validation** : `cargo test --locked` vert (336 dont 317 dans la lib), aucun nouvel
avertissement clippy dans le code touché. À valider en réel : un épisode
d'une playlist `unplayed_only` joué en entier n'est plus resélectionné ;
sauté, il l'est.

### — Environnement Docker de dev : stationd + Icecast + Liquidsoap sous s6 (2026-09-25) —

Décisions : Docker **remplace** l'install systemd (point 6 de la 0.1) ; un
seul conteneur, trois processus supervisés par **s6-overlay** (choisi
contre « stationd lance ses enfants » : zéro code, ordre de démarrage
géré) ; base `ubuntu:26.04` = paquets de l'archive validés sur devstationd
(`icecast2=2.5.0-1`, `liquidsoap` 2.4.0+dev), pas de dépôt tiers ; mode
azuradev : dépôt monté dans `/src`, `cargo build` dans le conteneur,
`target/` et registre cargo en volumes nommés ; `network_mode: host`
(IP réelle du proxy vue par Icecast, ports inchangés). Image de prod
multi-stage : plus tard.

- **Fichiers** : `compose.yaml`, `docker/Dockerfile.dev`,
  `docker/rootfs/etc/s6-overlay/` (services `init-perms` oneshot,
  `stationd`, `icecast`, `liquidsoap` ; script `init-perms`), `.env`
  (non versionné : `DEV_UID`, `DEV_GID`, `STATIOND_GID`, `MEDIA_GID`).
- **Ordre** : `init-perms` → `stationd` → `icecast` → `liquidsoap`.
  stationd est « prêt » quand `stationctl status` répond
  (`s6-notifyoncheck`) — `main.rs` écrit `.liq` et `icecast.xml` avant
  d'ouvrir le gRPC, aucun code ajouté. Redémarrer stationd ne relance pas
  les deux autres (antenne maintenue).
- **Utilisateurs** : `dev` (UID/GID de l'hôte, + `stationd`), `icecast2`
  (+ `stationd`), `liquidsoap` (principal `stationd`, + groupe des médias).
- **Socket de contrôle déplacé** : `control_socket =
  "/run/stationd/liquidsoap.sock"` (`/run/stationd` créé par `init-perms`,
  `root:stationd 2770`) — dans `stationd.toml` et `stationd.example.toml` ;
  le défaut du code reste `./data/liquidsoap.sock`.
- **Validation Icecast 2.5 (point 1)** : chaîne générée testée en natif
  sur devstationd avant la bascule (« Icecast fonctionne ») ; le détail des
  vérifications (mount non déclaré refusé, IP réelle via le proxy, titre
  accentué) n'a pas été consigné.

**Validation** : sur devstationd, build de l'image, `cargo build` dans le
conteneur, les trois services démarrent dans l'ordre, ça diffuse. Pièges
rencontrés en route : voir « Pièges » → Docker.

### — stationd génère `icecast.xml` (`[icecast.server]`) (2026-09-25) —

Décisions : stationd **écrit** la config Icecast comme le `.liq` (au
démarrage, si changée ; Icecast sous sa propre unité, jamais lancé par
stationd) ; `[icecast.server]` présent = génération, absent = lecture seule
comme avant ; **mot de passe source par mount** (celui de chaque
`[[liquidsoap.output]]`) ; `admin_url` reste explicite (vérifiée contre
`port`). Validé sur azuradev (config AzuraCast) : proxys de confiance =
un socket virtuel par adresse.

- **Config** `[icecast.server]` (`deny_unknown_fields`) : `config_path`
  (`./data/icecast.xml`), `port` (8000), `bind_address`, `hostname`,
  `location` (défaut : nom de station), `admin_email`, `max_clients`,
  `trusted_proxies` (IP exactes, pas de CIDR, doublon refusé), `share_dir`
  (`/usr/share/icecast2`), `log_dir` (`/var/log/icecast2`). Refusé au
  chargement : `admin_url` ou une sortie hors de `port`, mot de passe admin
  = un mot de passe source, mount en double.
- **`src/icecast_xml.rs`** (pur) : un `<mount>` par sortie (son mot de
  passe, `<charset>UTF-8</charset>`), `<sources>` = nombre de sorties,
  **pas de `<source-password>` global**, admin de `[icecast]`, socket
  public + un socket virtuel par proxy (`<trusted-proxy>#proxy-N`), CORS,
  chemins, logs. Échappement XML. `write_if_changed` atomique en **0640**.
- Contrat `IcecastService.RenderConfig` ; **CLI** `stationctl icecast
  render`. `main.rs` : écrit la config, avertit si `share_dir/web` absent.
- **Débit reçu corrigé** : Icecast ne rafraîchit `total_bytes_read` que
  toutes les ~5 s (mesuré) → l'écart entre deux lectures était faux (756
  kbit/s affichés pour 320 réels). Désormais moyenne glissante sur 60 s
  (affichée après 30 s ; `read_window_s` dans le contrat). Réel : ~314
  affichés pour ~320 envoyés.
- **Groupe du fichier** : `[icecast.server] file_group` (groupe partagé
  créé à l'install, ex. `stationd`) → chgrp à chaque démarrage, erreur
  bruyante si groupe inexistant ou stationd non membre ; droits réels
  journalisés et affichés par `icecast render` (`RenderConfigResponse.access`).
  Validé ici en non-root (membre → OK et Icecast lit via le groupe ;
  non-membre / groupe absent → refus explicite).
- Tests : +8 (config serveur, XML relu par roxmltree, sockets virtuels,
  écriture 0640, groupe, fenêtre de débit, dilution d'un palier).

**Validation** : `cargo test --locked` vert (308), clippy propre. **Contre
un vrai Icecast 2.4.4** lancé sur la config générée (utilisateur non-root
dans le groupe du fichier) : démarre ; `/radio.mp3` et `/lo.mp3` acceptent
chacun leur mot de passe, refusent celui de l'autre et un faux (401) ; un
mount non déclaré est refusé quel que soit le mot de passe (avec un global,
il était accepté → global retiré) ; admin refuse le mot de passe source ;
stationd lit ensuite cet Icecast (`icecast status` : 2 sources, 1
auditeur). Non testable ici : sockets virtuels (2.5 uniquement).

### — Icecast 2.5 : audience réelle + santé des mounts (2026-09-24) —

Décisions : stationd **lit** Icecast (`/admin/stats`, XML, compte admin) ;
somme des mounts de `[[liquidsoap.output]]` (pas le total serveur) ;
**lecture ratée = audience inconnue, jamais 0** (Icecast injoignable, 401,
XML illisible, un de nos mounts absent ou sans source) ; client
`hyper-util` (déjà dans l'arbre) ; commande dédiée `icecast status` (Icecast
tombe indépendamment de Liquidsoap) ; global d'abord, par mount plus tard.

- **Config** `[icecast]` (optionnelle, `deny_unknown_fields`) : `admin_url`
  (`http://host[:port]` seulement — https refusé : Icecast lu en direct,
  jamais via le reverse proxy), `admin_user` (défaut `admin`),
  `admin_password` (ASCII), `poll_interval` (15 s, 2..=600). Exige
  `[liquidsoap]` (ses mounts sont ceux surveillés). Avertissement au
  démarrage si une sortie vise un autre `host:port` que `admin_url`.
- **`src/icecast.rs`** : `parse_stats` (roxmltree, noms comparés sans
  espace de noms ; enveloppe `<report>` 2.5 → erreur avec le texte de
  l'incident), `audience` (somme, ou raison de l'inconnu), `IcecastClient`
  (GET + Basic auth, 3 s, corps ≤ 2 Mio), `IcecastMonitor` (dernière lecture,
  problème, audience, **débit réel** = croissance de `total_bytes_read` entre
  deux lectures, remis à zéro si `stream_start` change), `spawn_sampler`
  (journalise l'apparition / le changement / la fin d'un problème, pas
  chaque échec).
- `StationControl::clear_listeners()` : l'audience redevient inconnue
  (pas d'événement) → un `draining` ne s'achève jamais sur une panne.
- **Contrat** `proto/icecast_v1.proto` (`IcecastService.GetStatus`) →
  `src/icecast_grpc.rs` (projection pure du moniteur sur nos mounts).
  **CLI** `stationctl icecast status` ; `station state` affiche `unknown` au
  lieu de `(never sampled)`. `debug listeners` reste (écrasé à la lecture
  suivante).
- Constats Icecast **2.5.0** (captures réelles devstationd, en fixtures) :
  succès = `<icestats>` classique ; **erreur = `<report>`** (reportxml,
  `<incident>…<text>You need to authenticate`) avec `<icestats>` en espace
  de noms ; **plus de `<bitrate>`** : débit lu dans `<audio_info>…bitrate=`.
  Source connectée ⇔ `stream_start_iso8601` présent (vérifié). Le titre
  arrive en une seule chaîne ICY (`title` = `display-title` =
  `x_icy_title`), pas d'artiste séparé (mp3).
- Tests : +20 (config, parseur sur captures réelles, audience,
  inconnu ≠ 0, débit, client HTTP contre un faux Icecast, projection gRPC).

**Validation** : `cargo test --locked` vert (300), clippy propre, build
`--features tui` OK. E2E local contre un faux Icecast servant la capture
réelle (audience, mount absent, Icecast coupé → données marquées périmées).
**E2E réel devstationd** : 3 auditeurs `curl` → `listeners sampled count=3`,
`stop-when-idle` → `draining`, auditeurs coupés → 0 → `stopped` + bruit de
fond (une piste plus tard, cf. tâche d'entrée).

### — Fix : `ls status` bloqué sur `stopping` au 2ᵉ stop (2026-09-24) —

Le bruit (et le fallback) étaient signalés par `on_track` ; après un resume,
la boucle de bruit reste gelée en cours et est **reprise** au stop suivant →
aucun nouveau morceau, rien de signalé. Désormais signalés à chaque bascule
via `fallback(transitions=[…])` (`stationd.switched_to`, HTTP dans
`thread.run`). Reproduit puis validé en réel (stop → resume → stop → pause →
resume).

### — Liquidsoap : overrides soft réactif + hard coupant — fin de l'étape 2 (2026-09-24) —

- **Soft** : le push déclenche `flush` → l'override passe à la fin de la piste
  en cours (plus une piste plus tard).
- **Hard** : résolu au push (`GridEngine::air_override_now(id)` — l'entrée
  par id, hors ordre de file ; `StationControl::override_by_id`), puis
  `flush` + `stationd.interrupt <uri>` → file `interrupt` (request.queue) en
  tête de chaîne, `track_sensitive=false` : coupe immédiate, piste coupée
  abandonnée, la suite reprend après l'insert. `degraded` seulement sans
  Liquidsoap ou station en pause/arrêtée.
- **Règle** : une piste préparée issue d'un override n'est jamais vidée (elle
  a été consommée de la file) — trouvé en E2E : un hard vidait le soft
  préparé, perdu. `NextUp.from_override`, `LsBridge::prepared_is_override`.
- `StationControl` : canal `AirEvent { Transition | Override{id, mode} }`
  (remplace le canal de transitions). `next_override` refactoré
  (`air_override_entry` commun au pull et au hard).
- Tests (280) : hard résolu par id hors ordre, dégradé si halted, soft → flush,
  hard → flush + interrupt, override préparé jamais vidé (soft/hard/stop).
- E2E réel : jingle soft poussé → `next: jingle` ; flash hard → coupe
  immédiate ; puis jingle, puis la grille.

### — Liquidsoap : pause / resume / next par le socket de contrôle (2026-09-24) —

Sémantique : **pause immédiate** (piste gelée, bruit de fond à l'antenne),
**resume** = la piste gelée reprend où elle s'était arrêtée, **next** = skip
immédiat (la piste préparée démarre) ; **stop reste gracieux** (fin de piste,
non poussé).

- Config `[liquidsoap] control_socket` (défaut `./data/liquidsoap.sock`,
  longueur < 108 octets vérifiée). Script : serveur socket Liquidsoap (0660 —
  l'utilisateur stationd doit être dans le groupe de Liquidsoap), drapeau
  `stationd.paused` (retire le pull du `fallback`, garde le bruit), commandes
  `stationd.pause|resume|skip|state` ; skip sur la source de pistes **avant**
  le crossfade.
- **`src/ls_control.rs`** : client socket (commande → lignes → `END`, 3 s,
  réponse ≠ `OK` = refus), santé (dernier OK / dernière erreur) ; tâche
  **air sync** : `StationControl::attach_air` reçoit chaque transition →
  `paused` ⇒ `pause`, `running` ⇒ `resume` (CLI **et** plugins), échec retenté
  toutes les 5 s, état restauré réaffirmé au démarrage.
- `ls_bridge` : au resume depuis pause, la piste gelée repasse `on air` (même
  `since` : ce n'est pas un nouveau démarrage).
- Contrat : `BroadcastService.Skip` (7ᵉ RPC) ; `LiquidsoapStatus` +
  `control_*`. CLI : `stationctl station next` (alias `skip`) ; `ls status`
  ligne `control:`.
- **Stop sans piste en trop** : une transition vers `stopped` pousse
  `stationd.flush` (`pull_raw.set_queue([])` côté script) → la piste préparée
  est abandonnée, le pull redemande et reçoit `halted` : le bruit arrive à la
  fin de la piste **en cours**. Validé en réel (1 piste démarrée, pas 2).
- `ls status` : `LiquidsoapStatus.air_state` (`playing | paused | stop armed |
  stopping | stopped`) ; la ligne `tracks:` affiche `stopping — at the end of
  the current track` tant que la piste en cours (voire celle déjà préparée)
  va au bout après un `stop`.
- Tests : client socket (aller-retour, injoignable, refus), air sync (suit la
  machine d'états, stop non poussé, retry jusqu'à l'arrivée de Liquidsoap),
  bridge (piste gelée remise à l'antenne).

**Validation** : `cargo test --locked` vert (274). E2E réel Liquidsoap :
pause → bruit immédiat, piste gelée ; resume → même piste, même position ;
next → piste suivante immédiate ; pause + next + resume → la suivante démarre
au resume.

### — Câblage Liquidsoap, étape 1 : « ça diffuse » (2026-09-24) —

Décisions : A+C (pont HTTP loopback + socket, le socket = étape 2) ; stationd
**écrit** le `.liq`, Liquidsoap tourne sous **sa propre unité** ; `axum` en
direct ; pas de cue/fade/loudness en v1 (crossfade fixe) ; DJ live plus tard ;
**station arrêtée/en pause = bruit de fond en boucle** (le stream reste
occupé), jamais le fallback de sécurité. Détail : `Doc/liquidsoap.md`.

- **Config** `[liquidsoap]` (optionnelle, `deny_unknown_fields`) :
  `script_path`, `http_bind` (**loopback exigé**), `api_token`,
  `fallback_path`, `halted_path`, `[liquidsoap.crossfade]` (`simple|none`,
  `fade`, `duration`), `normalize`, `custom_include`, `log_level`,
  `[[liquidsoap.output]]` (Icecast, mp3, bitrate LAME validé). Validation
  bruyante au chargement (`ConfigError::Liquidsoap`).
- **`src/ls_script.rs`** (pur) : génère le script — pull `request.dynamic`
  → crossfade → `fallback([pull, bruit si halted, blank au démarrage,
  fallback sécu])` → normalize (option) → `%include` → outputs. Chaînes
  échappées + `#{` neutralisé. Chemins rendus absolus. `write_if_changed`
  (atomique).
- **`src/ls_bridge.rs`** : `POST /ls/v1/next` (→ `next_media` : gate,
  overrides, grille) répond `file` (uri annotée `stationd_rid`, chemin absolu)
  / `halted` / `none` ; `POST /ls/v1/track` (démarrage réel) → `on_air`,
  `on_track_completed` (compteur `Every`). Token `X-Stationd-Token`.
- **Contrat** `proto/liquidsoap_v1.proto` (`RenderScript`, `GetStatus`) →
  `src/ls_grpc.rs` ; **CLI** `stationctl ls render|status`. `ls status` montre
  `on air` (démarrage réel) et `next` (piste préchargée par Liquidsoap, pas
  encore démarrée ; vidée par une réponse halted/none) ; `last reply` n'est
  affiché que pour halted/none.
- `main.rs` : écrit le script, avertit si fallback/bruit absents, sert le pont
  (échec de bind = fatal). `GridEngine` dérive `Clone`.
- Tests : config (défauts, loopback, sorties, typo), script (échappement,
  ordre des sources, options), pont (uri, halted ≠ fallback, pas de règle,
  on-air + compteur, rid consommé une fois, boucle bruit, token HTTP).

**Validation** : `cargo build` + `cargo test` verts (267 unitaires +
intégration). **E2E réel** stationd + Liquidsoap (2.2.4 de la distro, script
adapté à la marge : `null()` et `on_track` sans `synchronous`) sortie dummy :
pistes tirées et annoncées, stop → bruit à la fin de la piste en cours, resume
→ musique en ~2 s, stationd tué → fallback, stationd relancé → musique.
Constats intégrés au générateur : `on_track` écouté **avant** `cross` (après
un crossfade il ne se déclenche pas), pas d'`annotate:` sur `single` (rend la
source faillible), rafale de `/next` autour d'une fin de piste bornée côté
script. Syntaxe 2.4 (`null`, `source.methods`, `synchronous=`) calquée sur le
script AzuraCast 2.4.5 ; **validé en diffusion réelle sur devstationd**
(Liquidsoap 2.4 → Icecast 2.5).

### — Biblio : filtre et inventaire par genre (2026-09-23) —

Base : aucune migration (les genres étaient déjà lus au scan et stockés dans
`media_genre`). Comparaison **insensible à la casse, repli Unicode** (`trim` +
`to_lowercase` côté Rust) : SQLite `NOCASE`/`lower()` ne replient que l'ASCII,
« Électro » ≠ « électro » sinon.

- **Contrat** `library_v1.proto` (additif) : `ListMediaRequest.genres`
  (repeated, **any**, vide = pas de filtre) ; nouveau RPC `ListGenres` →
  `GenreCount { genre, count, spellings }` + `untagged`.
- **`media_index`** : `genre_key` (clé de repli), `list(pool, only_available,
  genres)` filtre, `genres(pool, only_available) -> GenreInventory` (un seau par
  clé repliée, libellé = graphie la plus fréquente, `spellings` > 1 = tags
  incohérents à corriger à la source ; un média compté une fois par seau).
- **`library_actor`** : `list(only_available, genres)` ; genre vide → erreur
  bruyante `BadFilter` (→ `invalid_argument`) ; commande `Genres`.
- **CLI** : `library list` affiche `[genre, …]` (`—` si aucun) ;
  `--genre X` (répétable, any) ; `--by-genre` (regroupement d'affichage, un
  média multi-genre sous chacun, `(no genre)` à la fin) ; `library genres
  [--all]` (compte par genre + graphies divergentes + sans genre).
- Tests : `media_index` (filtre casse/Unicode/any, inventaire replié, portée
  available), `library_actor` (genre vide rejeté).

**Validation** : `cargo build --locked --bins` + `cargo test --locked -p
stationd` verts (246 tests unitaires + intégration) dans l'env de préparation.

**Filtre `genre` des playlists aligné** (même jour) : `has`/`has_any`/
`has_all`/`has_none` sont désormais **insensibles à la casse (Unicode)**.
- **Migration 0015** `media_genre.genre_key` (+ index) : clé repliée écrite au
  scan par `media_index::genre_key` (source unique du repli). Backfill SQL
  `lower(trim(genre))` = ASCII seulement → **relancer `library scan`** après
  migration pour les genres accentués.
- `selection.rs` : `SetField` gagne `fold` ; `genre` compare `genre_key`, les
  valeurs de filtre sont repliées avant bind. `genre` garde la graphie
  d'origine (affichage, `Candidate.genres`). Test
  `genre_filter_is_case_insensitive_unicode` (has JAZZ, has électro,
  has_none [ROCK, jazz]).
- ⚠ Changement de sens : une playlist `has "Jazz"` qui excluait `jazz`
  l'inclut désormais.

**Limite** : le scan lit **une seule chaîne** genre par fichier (pas de
découpe `Rock; Pop` — raffinement prévu dans `media.rs`).

### — Plugins A2 : surface hôte (control + override + host functions) (2026-09-23) —

Décisions tranchées (détail dans `Doc/plugin-host.md` § Décisions) : `hard`
sans LS **dégradé en soft + warn** ; **capacités déclarées** par plugin ;
`media_path` **arbitraire** accepté (chemin sûr sous `media/`, vérifié sur
disque au passage) ; override PL = `tracks` pistes (défaut 1) ; file bornée (64)
et **volatile** (perte annoncée à l'arrêt). `db_*` = tranche suivante.

- **`src/station_control.rs`** (neuf) : `StationControl` clonable, API
  synchrone (appelable depuis un hook). État `running|paused|stopped|draining`
  (`apply(action, by)`, transitions refusées si absurdes), `gate()` au bord de
  piste (`draining` + dernier échantillon = 0 → `stopped`), `sample_listeners`,
  file d'override (`push`/`next`/`consume`/`drop`/`list`/`clear`, péremption,
  plafond), **horloge manuelle déplacée ici** (partagée avec `GridEngine` :
  péremption et résolution sur la même horloge). Émission `BroadcastStateChanged`
  / `ListenersSampled` via le handle plugins (`attach_plugins`, OnceLock).
- **Migration 0014** `broadcast_state` (ligne unique, famille B) : état
  persisté par un writer ordonné (mpsc), restauré au démarrage (warn si ≠
  running). Un `stop` opérateur survit au redémarrage.
- **`GridEngine::next_media`** : 1) gate → `halted` (aucune résolution, aucun
  log) ; 2) **couche override** avant la grille (résolveur pur intouché) —
  média vérifié sur disque, PL résolue avec plugins/contraintes ; échec →
  override abandonné (loggué) et la grille reprend ; effets grille (AtClock /
  Every) non touchés ; 3) grille inchangée. `ResolvedDecision` += `halted`,
  `override_source`. `log_start` factorisé. `with_control(...)`.
- **`plugin.rs`** : `Host` (nom + capacités + control) remis dans
  **`on_load(host)`** (signature du trait changée) ; `Capability {Control,
  PushOverride}`, `PluginDecl.capabilities`, `HostError::Denied` loggué.
  `spawn_with(decls, control)` (`spawn` = sans control). Events
  `ListenersSampled{count,at}`, `BroadcastStateChanged{from,to,by}`. **Host
  functions extism** `station_control` / `push_override` (JSON in/out, refus =
  `{"ok":false}` jamais un trap), liées au `Host` du plugin. Plugin natif
  **`stop-when-idle`** (exige `control`, `min_zero_samples`).
- **Guest** `plugins/stop-when-idle-wasm/` (crate séparé) : `on_event` →
  `station_control({"action":"stop_when_idle"})` à 0 auditeur.
- **Contrat** : `proto/broadcast_v1.proto` (6 RPC, `BroadcastService`, 6ᵉ
  service du serveur) ; `schedule_v1` `Decision` += `OVERRIDE`/`HALTED`,
  `override_source`, `halted_state` ; `plugin_v1` `PluginInfo.capabilities`.
- **CLI** : `stationctl station state|stop|pause|resume|stop-when-idle`,
  `override push (--media|--playlist) [--hard] [--expiry 5m] [--tracks N] |
  list | clear [--id]`, `debug listeners <n>` ; `schedule next` affiche
  override/halted ; `plugin list` affiche `caps=[…]`.
- `stationd.example.toml` : exemples `[[plugin]]` commentés.
- Tests : `station_control` (machine d'états, drain, normalisation chemin,
  push/ordre/validation, hard dégradé, péremption, consume/drop, plafond,
  persistance restaurée), `plugin` (refus capacité, signature du plugin,
  JSON host fns, stop-when-idle via l'acteur + min_zero_samples),
  `grid_engine` (stopped → rien, drain au bord, override média une fois,
  override PL `tracks=2`, override injouable abandonné, péremption sur
  l'horloge moteur).

**Validation** : `cargo build` + `cargo test -p stationd` verts (2026-09-23).
Guest wasm compilé (`--target wasm32-unknown-unknown`) et validé en réel
(`debug listeners 0` → draining → `schedule next` → HALTED).
**Effet audio réel = LS-gated** : sans Liquidsoap, `stopped`/`HALTED` ne coupe
rien (seul le CLI consomme `ResolveNext`). Au câblage LS : sur `HALTED`, LS
doit couper sa sortie, **pas** basculer sur son fallback de sécurité.
Le TUI (`--features tui`) n'est pas touché (aucun littéral proto modifié).

### — Contraintes portées par un groupe + refs de membres relatives (2026-09-23) —

**Contraintes de groupe** : les `[broadcast.constraints]` déclarées sur un
groupe s'appliquent désormais à **chaque piste émise par le groupe**.
- Sémantique : **cumulatives** — portée d'une feuille = contraintes de tous les
  groupes englobants + les siennes ; chaque jeu est appliqué (la fenêtre la
  plus stricte gagne). Héritées à travers les groupes imbriqués, jamais
  comptées deux fois, jamais relâchées (pool vidé → `PoolEmpty` → fallthrough).
- `selection.rs` : `resolve_media`/`resolve_member`/`resolve_group_{rotation,
  weighted}` threadent `inherited: &[&Constraints]` ; `resolve_leaf` et
  `apply_constraints` prennent une slice (boucle `apply_one_constraint_set`).
- Membres `remote`/`queue` : non filtrés (comme leurs propres contraintes —
  un flux n'a pas d'identité de piste, une entrée de queue a été poussée
  explicitement).
- CheckCoverage inchangé (l'axe A lisait déjà les contraintes du groupe).

**Refs de membres relatives** — tranché : **syntaxe explicite**.
- `ref` commençant par `./` ou `../` (ou `.`/`..`) → relatif au **dossier du
  groupe** (`./intro` depuis `shows/main` → `shows/intro`) ; `..` peut remonter
  jusqu'à la racine, jamais au-delà (erreur bruyante). Un ref relatif doit
  **nommer une playlist** : `./`, `.`, `..`, `./x/..` (qui désignent un
  dossier) sont rejetés.
- Tout autre `ref` → relatif à la racine, **inchangé** (rétro-compatible).
- **Aucun fallback implicite** entre les deux : `./intro` introuvable ne
  retombe jamais sur la racine `intro` (→ `PlaylistNotFound` / erreur `sync`).
- `playlist::resolve_member_ref(group_key, raw)` = point unique, utilisé par
  `validate_set` (sync : refs + cycles), `selection` (rotation + weighted) et
  `pool_inspection` (preview / CheckCoverage). Les refs de grille
  (`grid.toml`) restent racine-relatives (`normalize_ref`, inchangé).
- ⚠ Changement de sens limité : un `ref` de membre déjà écrit `./x` dans un
  groupe en sous-dossier visait la racine ; il vise désormais le dossier du
  groupe. Sans effet pour un groupe à la racine.
- Tests : `playlist` (résolution, remontée, racine dépassée, vide, set :
  relatif OK / inconnu sans fallback / cycle via relatif / au-delà racine) ;
  `selection` (contraintes de groupe : appliquées, héritées en imbriqué,
  cumulatives avec celles du membre, pool vidé → PoolEmpty ; refs relatives
  sequence + weighted, pas de fallback racine) ; `tests/pool_preview.rs`
  (inspection avec refs relatives).

**Validation** : `cargo test -p stationd` vert (2026-09-23).

### — queue : tampon runtime + enqueue (CLI-complete) (2026-09-22) —

Dernier mode de sélection câblé. Une `queue` est un tampon volatil rempli au
runtime (demandes auditeurs / injection DJ), consommé FIFO/LIFO.

- **Migration 0013** `queue_entry(id, playlist_ref, rel_path, enqueued_at)` +
  index (playlist_ref, id). Famille B, aucune FK, survit au restart.
- **`src/queue_state.rs`** : `push` (refuse au-delà de `max_len` — 0/absent =
  illimité, pas de drop silencieux), `pop(fifo/lifo)` (renvoie + supprime),
  `count`.
- **Résolution** : arms `Mode::Queue` de `resolve_media` et `resolve_member` →
  pop selon `order`, consommation à la résolution ; vide → `PoolEmpty` →
  fallthrough. Fichier mort → `mark_unavailable` + re-pop suivant.
- **Enqueue (full CLI-complete)** : `grid_engine::enqueue(ref, media)` (erreur
  si ref inconnue / pas une queue ; refusé au max_len) → RPC `Enqueue` →
  `stationctl queue push <ref> <media>` (exit non-zéro si refusé).
- **LIFO** : implémenté tel que déclaré, avec la note doc « LIFO public = piège »
  (mieux modélisé comme push prioritaire) ; pertinent pour l'injection DJ.
- Tests : `queue_state` (fifo/lifo/max_len), `selection` (pop fifo/lifo,
  drain → PoolEmpty), `grid_engine` (enqueue → next_media pop → consommé ;
  erreurs ref inconnue / non-queue).

**Validation** : édité ; `cargo test -p stationd` à confirmer. Migration 0013
neuve.

### — remote : résolution en flux (2026-09-22) —

Un `remote` (relais d'un flux externe) est désormais RÉSOLU au playout — avant,
`Unsupported`. Le relais réel (`input.http` côté Liquidsoap) reste **LS-gated**.

- **Contrat** : enum `selection::Resolved { File(rel_path) | Stream(url) }`.
  `resolve_ref`/`resolve_ref_at` aplatissent en `String` (tests fichier
  intacts) ; `resolve_ref_with_plugins` rend le `Resolved` typé.
- **Résolution** : l'arm `Mode::Remote` de `resolve_media` ET de `resolve_member`
  (un remote peut être membre de groupe — relais de nuit) renvoie l'`url` en
  `Stream`. `Mode::Queue` reste `UnsupportedMode`.
- **`grid_engine::next_media`** : un `Stream` court-circuite la boucle de re-pick
  disque (`media_exists`/`mark_unavailable`) et n'écrit NI dans `broadcast_log`
  NI dans `episode_play` (une URL n'a ni identité fichier ni artiste).
  `ResolvedDecision` gagne `stream: bool` pour le futur câblage LS.
- Tests : `selection` (remote seul → Stream, remote membre de groupe → Stream) ;
  `grid_engine` (next_media relaie un remote sans toucher au disque ni logger).

**LS-gated** : le moteur produit désormais l'URL + le flag `stream` ; le relais
`input.http` et le point de bascule de source seront branchés avec Liquidsoap.

**Validation** : édité ; `cargo test -p stationd` à confirmer.

### — unplayed_only (slice 2) (2026-09-22) —

Play-once par épisode : un épisode déjà diffusé EN ENTIER pour une playlist n'est
plus resélectionné (cooldown à expiration infinie, distinct de `broadcast_log`).

- **Migration 0012** `episode_play(playlist_ref, rel_path, size_bytes,
  mtime_ns, played_at, PK(playlist_ref, rel_path))`. Famille B, aucune FK.
- **`src/episode_play.rs`** : `mark` (upsert), `played_matching` = JOIN
  `episode_play ⋈ media` sur `size_bytes` ET `mtime_ns` → le **garde-fou**
  d'identité (chemin + taille/mtime) vit dans le SQL ; un fichier changé rend
  l'épisode ré-éligible. Pas besoin de toucher `Candidate`.
- **Filtrage** : `selection::resolve_leaf`, après materialize/plugin/constraints,
  avant le choix. Pool vidé → `PoolEmpty` → fallthrough (no-silent-failure).
- **Validation** : `unplayed_only` exige `order = newest|oldest` — erreur
  bruyante à `validate()` ET en résolution (avant, un `unplayed_only` avec
  shuffle était silencieusement ignoré → trou fermé). `reject_unplayed_only`
  supprimé.
- **Marquage** : `grid_engine::on_episode_finished(playlist_ref, media, now)`, à
  la FIN d'une diffusion (garde-fou taille/mtime capté depuis l'index). No-op
  hors playlist `unplayed_only`. `media_index::size_mtime_of` ajouté.
- Tests : `selection` (oldest rattrapage + garde-fou ré-éligibilise, newest
  backlog, shuffle rejeté), `playlist` (validate), `grid_engine`
  (on_episode_finished scopé), `episode_play` (mark/JOIN/garde-fou).

**Limite (décision confirmée)** : le marquage n'est **pas auto-câblé** dans la
boucle live — il attend le signal de fin de piste de Liquidsoap. Le filtre
fonctionne dès maintenant ; le marquage auto arrive avec le câblage Liquidsoap.
En attendant, `on_episode_finished` est pilotable au CLI/tests.

**Validation** : édité ; `cargo test -p stationd` à confirmer. Migration 0012
neuve → aucun conflit checksum.

### — Anti-répétition temporelle (slice 1) (2026-09-22) —

Première tranche de « famille B → anti-répétition ». `no_same_track_within` et
`no_same_artist_within` sont désormais **appliqués au playout** (avant, ils
n'étaient qu'un signal de dimensionnement dans `CheckCoverage`).

- **Migration 0011** `broadcast_log(id, rel_path, artist NULL, played_at)` +
  index `played_at`. Famille B : append-only, aucune FK, jamais remis à zéro
  par un scan/apply. Historique **station-wide**.
- **`src/broadcast_log.rs`** : `record` (au démarrage d'une piste),
  `tracks_since` / `artists_since` (fenêtres). `media_index::artist_of` fournit
  l'artiste au moment du log.
- **Écriture** : `grid_engine::next_media`, une fois la source produite (même
  point que `persist_effects`) — « titre X démarré à T ».
- **Filtrage** : `selection::apply_constraints`, après `materialize` + filtre
  plugin, avant le choix. Contrainte **dure** : jamais relâchée ; pool vidé →
  `PoolEmpty` → fallthrough grille (no-silent-failure : pas de trou muet, pas de
  rejeu forcé). Un candidat non taggé (artist None) n'est jamais exclu par la
  fenêtre artiste. `now`/`constraints` threadés dans `resolve_leaf` via
  `resolve_media`/`resolve_member` — contraintes **propres à chaque feuille**.
- Tests : `selection` (piste dans/hors fenêtre, artiste exclu, non-taggé jamais
  exclu) + `grid_engine` (next_media logue puis exclut) + `broadcast_log`
  (record + fenêtres).

**Limite documentée** : les contraintes déclarées sur un GROUPE lui-même (et non
sur ses membres) ne sont PAS encore threadées jusqu'à la résolution des membres
— les contraintes propres d'un membre feuille, elles, s'appliquent. À faire si
le besoin apparaît.

**Slice 2 (à suivre)** : `unplayed_only` — mécanisme distinct (cooldown à
expiration infinie, keyé par (playlist, identité d'épisode) avec garde-fou
chemin+taille/mtime). Table/logique à part.

**Validation** : édité ; `cargo test -p stationd` à confirmer.

### — Groupes imbriqués : déjà faits, verrouillés par des tests (2026-09-22) —

Constat : les groupes imbriqués (un groupe membre d'un groupe) étaient **déjà
implémentés des deux côtés** — résolution (`selection.rs` : `resolve_member`
recurse via `Box::pin(resolve_media)`, `now`/`depth` threadés, cap
`MAX_GROUP_DEPTH`) ET inspection (`pool_inspection.rs` : `inspect_ref_at_depth`
recurse, même cap). `ETAT` était en retard sur le code.

La résolution avait ses tests ; l'inspection nested n'en avait **aucun** (chemin
récursif non exercé, bras `inspect_leaf` « nested group pools » devenu mort
défensif). Et `CheckCoverage` n'avait aucun test. Ajouté dans
`tests/pool_preview.rs` :
- `inspect_ref_sums_a_nested_group_recursively` — outer=sequence[inner,jazz],
  inner=weighted[jazz,static] → total 6 / 1_022_000 ms, le membre nested portant
  son agrégat 4 / 601_250 ms.
- `coverage_flags_empty_pool_and_missing_ref` — ✗ pool vide, ✗ ref cassée, worst=✗.
- `coverage_flags_undersized_group_members` — `runtime = 1h` et `take = 5` sur un
  pool de 2 pistes → ⚠, membre nommé.
- `coverage_ok_for_a_sufficient_rotation` — dynamic sans contrainte → OK.

Rien à construire côté nested. `cargo test -p stationd` à relancer.

### — Décision : pas de `limit` standalone au playout (2026-09-22) —

`broadcast.limit` sur une playlist **autonome** (base/day_part référencée
directement par la grille) n'est **pas appliqué au playout, par design**.

Vérification faite sur la famille B : `every_state` (rule_id/last_played/
tracks_since), `at_clock_taken`, `group_state` (member_idx/take_count),
`playlist_cursor`. Aucune « source de grille active + run count », et rien pour
la dériver. L'appliquer imposerait une nouvelle table `grid_activation` + une
sémantique de « cède la main » sans destinataire clair dans une grille *pull*
(le résolveur re-choisit par les règles à chaque bord ; rien vers quoi tourner
pour une base unique).

Or « N pistes puis on passe » **existe déjà** : `take` par membre de groupe
(`group_state.take_count`) et `Every` au compteur (`every_state.tracks_since`).
Aucun cas d'usage pour un `limit` standalone en plus → on ne construit rien.

`limit` **reste dans le contrat** : `CheckCoverage` s'en sert comme signal de
dimensionnement (pool ≥ N pistes distinctes). Sur une base non-groupe il est
**advisory** (parse, non appliqué au playout — pas une erreur de validation) ; le
tour-de-rôle « N puis suivant » passe par un groupe `rotate`/`sequence` + `take`.

### — DayPart cross-minuit (2026-09-22) —

Débloque une base de nuit qui enjambe minuit (22:00→06:00). C'était un manque
fonctionnel : `window_covers` renvoyait `None` dès `end ≤ start`, et `grid_toml`
rejetait la règle à l'`apply`.

- `resolver.rs` (`window_covers`, pur) : `end < start` = fenêtre cross-minuit
  `[start, 24:00) ∪ [00:00, end)`, largeur qui enjambe minuit (le
  « narrowest wins » compare donc des durées justes) ; `start == end` = fenêtre
  nulle, ne couvre jamais.
- `grid_toml.rs` : la validation `day_part` n'interdit plus que `start == end`
  (message adapté) ; `end < start` passe. La table `grid_day_part` (0006) n'a
  aucun `CHECK end > start` → aucune migration.
- Tests : résolveur `daypart_cross_midnight_covers_both_sides_of_midnight` +
  `narrowest_daypart_wins_across_midnight` ; grammaire
  `rejects_cross_midnight_day_part` → `accepts_cross_midnight_day_part`, +
  `rejects_zero_length_day_part`.

**Caveat documenté** (pas un bug silencieux) : la couverture est sur l'horloge
murale ; `days` reste évalué sur le **jour calendaire de `now`**. Une base de
nuit **tous les jours** est exacte ; une émission restreinte à un jour (« nuit
du vendredi ») matcherait par le jour de `now`, pas par le jour de *début* de la
fenêtre — l'ancrage sur le jour de départ n'est pas fait. À trancher en suivi si
le besoin apparaît.

**Validation** : édité ; `cargo build` + `cargo test -p stationd` à confirmer.

### — CheckCoverage : preview de dimensionnement « assez de média ? » (2026-09-22) —

Nouveau RPC pour valider une grille *avant* commit : pour chaque règle, le pool
de la playlist référencée a-t-il assez de média pour ce qu'on lui demande ?
**Read-only, dimensionnement pur — PAS un playout.** `stationctl schedule check
[--rule <id>]`.

**⚠ Abandon d'un concept.** Ceci **remplace** l'idée de « preview autoritatif =
playout baké sur horloge virtuelle + commit des médias sélectionnés » (dérouler
le playout puis le figer était le mauvais modèle ; ce qu'on veut, c'est compter
et signaler les manques — en particulier les boucles). La grosse section
« Preview / validation de la grille avant diffusion » qui décrivait ce concept a
été **retirée** de ce fichier. Les messages proto de l'arbre BLOCK/TRACK
(`PreviewLog`/`PreviewSlice`/`PreviewBlock`/`Coverage`/`Diagnostic*`,
`PreviewRequest.depth`, `PreviewResponse.log`) ont été **retirés du proto et des
sites d'appel** ; seule la timeline plate `occurrences`/`indicative` subsiste au
contrat `Preview`.

**Verdict = pire des deux axes** (`OK` / `THIN ⚠` / `INSUFFICIENT ✗`) :
- **Axe A — exigences propres de la playlist** : pool vide → ✗ ;
  `no_same_track_within=D` → durée pool ≥ D sinon ⚠ (rejeu de piste forcé) ;
  `no_same_artist_within` → ≥ 2 artistes distincts sinon ⚠ (borne basse honnête,
  affinable) ; `limit=N` → count ≥ N sinon ⚠.
- **Axe B — demande temporelle de la grille** : source **finie** (static/queue
  sans repeat, groupe `sequence`) dont la durée < créneau → ⚠. Créneau =
  `DayPart` end−start (passage minuit géré), `BaseRotation` 24h, `AtClock`/`Every`
  ponctuel (count ≥ 1). Source **bouclante** (dynamic/remote/rotation,
  `repeat=true`) → axe B auto-satisfait (c'est l'axe A qui mord sur le rejeu).
- **Membres de groupe** : chaque membre jugé sur **son quota** — vide → ✗ ;
  `runtime=D` > durée du pool membre → ⚠ « boucle dans le slot » ; `take=N` > pistes
  distinctes → ⚠ « répétition » ; pool inconnu (remote/queue) → pas de fausse
  alerte. Le pire membre remonte au groupe ; un vide fait ✗ si `abort`, ⚠ si
  `skip`. **C'est ce qui rend les boucles visibles — le but du check.**
- Erreur de config **par entrée** (ref cassée, playlist illisible, pool non
  résolvable) → entrée ✗ avec détail, **jamais un abandon du rapport** ; seule
  l'infra (SQLite) remonte en erreur gRPC. Exit CLI non-zéro **uniquement sur ✗**
  (⚠ passe encore à l'antenne → ne casse pas un gate CI).

Câblage (tout additif, rien de l'existant modifié) :
- `proto/schedule_v1.proto` : `rpc CheckCoverage`, enum fermé
  `Verdict {OK, THIN, INSUFFICIENT}`, messages `CheckCoverage{Request,Response}`
  / `CoverageEntry` / `CoverageMember`. → **7e RPC** de `ScheduleService`.
- `src/pool_inspection.rs` : `PoolStats` += `distinct_artists` (calculé au grain
  feuille sur le pool matérialisé ; `None` pour un agrégat de groupe / remote /
  queue — non dédoublonnable).
- `src/grid_engine.rs` : types `Verdict`/`CoverageEntry`/`CoverageMember`/
  `CoverageReport` + `check_coverage(rule_ids)` : itère les règles **activées**,
  `inspect_ref` (aucun playout, aucune mutation famille B), verdict via
  `verdict_for` / `member_verdict`.
- `src/schedule_grpc.rs` : handler `check_coverage` + mappings (traducteur pur).
- `src/bin/stationctl.rs` : `schedule check [--rule …]`, tableau
  `verdict | kind | ref | #médias | durée | détail`, membres indentés (`├`/`└`),
  résumé `verdict grille:`.

Réparé au passage : deux champs proto orphelins de l'extension arbre abandonnée
(`PreviewResponse.log`, `PreviewRequest.depth`) qui bloquaient la compilation ;
tests d'intégration recalés (`PoolStats.distinct_artists`, `PreviewRequest.depth`
dans `tests/pool_preview.rs` + `tests/agenda_preview.rs`).

**Validation** : édité ; **`cargo build` + `cargo test -p stationd` à reconfirmer**
(Rust indispo dans l'env d'édition ; le dernier changement = quota membre
`runtime`/`take`). Regen proto par `build.rs` au prochain build.

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
- `stationctl library scan|list [--all]` (+ `genres`, `--genre`, `--by-genre` depuis 2026-09-23).

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
- **Sélection** : complète (static / dynamic / remote / queue / group +
  anti-répétition, y compris contraintes de groupe héritées + unplayed_only).
  `limit` standalone : écarté ; marquage `unplayed_only` : auto-câblé
  (fin de piste déduite par le pont) ; relais remote : câblé
  (`input.http`). Contraintes sur une `queue` : non appliquées
  (préexistant ; la proposition v1 veut « entrée exclue reste, on cherche la
  suivante éligible » — à faire si besoin).

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
- Liquidsoap : étapes 3 (tâche de diffusion) et 4 (fins de piste) — voir
  tâche d'entrée. Reportés : DJ live (harbor), cue/fade/loudness par piste,
  formats autres que mp3.
- Rôles/permissions (différés). mTLS gRPC (reporté). Couche `api` (Axum BFF)
  + UI web.
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
- **Refs de membres** : racine-relatives par défaut
  (`homestone-chronicles/homestone-chronicles-intro`) ; relatives au dossier du
  groupe **seulement** si écrites `./x` / `../x`. Un ref nu court (`intro`)
  vise la racine, pas le dossier du groupe.
- **Casse des chemins** : `media.rel_path` CONSERVE la casse (fichiers réels,
  FS potentiellement sensible à la casse) ; les refs playlists sont, elles,
  normalisées en minuscules. Ne pas traiter les deux pareil.
- **`resolver.rs` doit rester pur / std-only** : c'est ce qui le rend testable
  et simulable sans horloge. Toute conversion de fuseau passe par `clock`.
- **`AtClock` soft sans `expiry`** peut se déclencher tard (au prochain bord de
  piste), repères intermédiaires sautés (fusion). Pour taper l'heure pile :
  `Mode::Hard` (coupe au repère, à 10 s près ; sinon dégradé en soft) +
  `expiry` court pour ne pas diffuser trop tard un repère non coupé.
- **`ref_effective` pas encore unique en base** (index différé, cf. 0004).
- **`signal::unix` dans `main.rs`** = Linux only. Dev Windows (`X:`) / build+run
  Linux (`/data/dev/stationd`). `X:\stationd` = `\\devradio.lan\dev\stationd`
  (même dépôt, lecteur mappé).
- **Chemins relatifs de config** résolus depuis le CWD (en Docker : `/src`,
  la racine du dépôt monté).
- **Liquidsoap** :
  - stationd **réécrit** le `.liq` au démarrage (s'il a changé) mais ne lance
    pas Liquidsoap : après une modif de `[liquidsoap]` ou du générateur,
    **relancer Liquidsoap** (log stationd : « script (re)written »).
  - Changement de stationd seul (pont, CLI) : relancer stationd suffit ;
    Liquidsoap retombe sur le fallback le temps du redémarrage.
  - `api_token` / mots de passe : **ASCII imprimable**, refusé au chargement
    sinon (un `…` copié d'un exemple donnait des 401 muets sur le pont).
    `fallback_path` / `halted_path` : absents, vides, pas des fichiers ou
    illisibles par stationd → **démarrage refusé** ; non lisibles par « les
    autres » → avertissement (Liquidsoap doit les lire par un de ses
    groupes, sinon il s'arrête sur « Infallible source.dynamic … »).
  - Socket de contrôle en **0660** : l'utilisateur de stationd doit être dans
    le groupe de Liquidsoap ; chemin < 108 octets. En Docker :
    `/run/stationd/liquidsoap.sock` (hors du dépôt monté).
  - La piste suivante est résolue **~7 s avant la fin** de la courante
    (`stationd.lead` dans le `.liq`) ; `schedule next` consomme toujours une
    vraie piste — ne pas l'utiliser quand Liquidsoap tourne.
  - Nouveau script `.liq` (point 5) : **relancer Liquidsoap** après la mise
    à jour de stationd (`s6-svc -r /run/service/liquidsoap`).
  - Écriture via le pont fichiers : une écriture a déjà été signalée réussie
    avec l'ancien contenu → **relire après écriture** (cmp) avant de compiler.
    Reproduit le 2026-09-24 (dépôt `device_commit_files` réutilisant un
    chemin déjà déposé) ; parade : **un chemin de dépôt neuf par écriture**.
    **Reproduit deux fois le 2026-09-25 malgré des dossiers de dépôt neufs**
    (`stationd.example.toml`, puis `compose.yaml` ; nouvelle date de
    modification mais ancien contenu, l'autre fichier du même envoi
    correct) : la relecture `cmp` après chaque écriture reste la seule
    garde fiable ; réécrire depuis un autre dossier neuf a suffi.
    Ce jour-là les outils Filesystem MCP étaient en panne (erreur de schéma) :
    lecture/écriture par stage/commit.
- **Icecast** :
  - `[icecast] admin_password` = mot de passe **admin** d'`icecast.xml`,
    pas le mot de passe source. Sur devstationd ils sont **identiques**
    (à séparer : changer `<admin-password>` ne touche pas Liquidsoap). Avec
    `[icecast.server]`, stationd refuse de démarrer s'ils sont égaux.
  - Avec `[icecast.server]` : Icecast est lancé par s6 dans le conteneur
    (`icecast2 -c /src/data/icecast.xml`). Après une modif : relancer
    stationd (réécrit) puis `s6-svc -r /run/service/icecast`. (Avant
    Docker : unité `icecast-stationd`, unité du paquet désactivée.)
  - **Groupe partagé `stationd`** (créé à l'install, aucun nom d'utilisateur
    supposé) : `file_group = "stationd"` → `icecast.xml` (0640) donné à ce
    groupe à chaque démarrage (erreur si groupe absent / stationd non
    membre) ; Icecast `SupplementaryGroups=stationd` ; Liquidsoap
    `Group=stationd` (socket 0660). `status=216/GROUP` de systemd = groupe
    ou utilisateur de l'unité inexistant. Un fichier créé prend le groupe
    **principal** du processus, d'où `file_group` (chgrp explicite).
  - **Icecast 2.5 : config illisible → SEGV au démarrage** (`code=dumped,
    signal=SEGV`), pas d'erreur (la 2.4.4 disait `FATAL: error parsing
    config file`). Constaté sur devstationd : `icecast.xml` encore en groupe
    `foxi` (stationd pas relancé avec `file_group`). Vérifier dans les
    conditions de l'unité : `sudo systemd-run --uid=<user Icecast> -p
    SupplementaryGroups=stationd --pipe --wait head -c 40 <config>` →
    `Permission denied` = cause. Le contenu de la config était hors de cause
    (toutes les variantes démarrent lancées à la main sur une copie lisible).
  - `usermod -aG stationd <user>` ne vaut que pour les **nouvelles**
    sessions : vérifier `id` avant de relancer stationd, sinon « not a
    member ».
  - Débit reçu = moyenne sur 60 s (compteur Icecast rafraîchi toutes les
    ~5 s) : rien avant 30 s de mesure, c'est normal.
  - Icecast lu en `http://` direct (loopback/LAN), jamais via Traefik.
  - Audience inconnue ≠ 0 : si `icecast status` montre un problème, un
    `stop-when-idle` armé attend (voulu).
  - `curl -s -u 'admin:…' http://127.0.0.1:8000/admin/stats` pour voir ce que
    stationd lit ; un auditeur de test : `curl -s …/radio.mp3 -o /dev/null &`.
  - `icecast.xml` copié dans le dépôt pour lecture : mots de passe en clair,
    **ne pas committer**.
- **Docker (dev)** :
  - `COPY rootfs/ /` reporte les droits des dossiers du contexte (créés via
    le partage Samba, restrictifs) **jusque sur `/etc`** : plus aucun
    utilisateur non-root ne lisait `/etc/resolv.conf`, `/etc/hosts`,
    `/etc/passwd` (symptôme : `cargo` en `dev` « Could not resolve host »,
    alors que `getent` en root résout). Le Dockerfile normalise les droits
    après le `COPY` ; tout futur `COPY` depuis le partage doit en faire
    autant.
  - `protobuf-compiler` avec `--no-install-recommends` n'amène pas les
    `.proto` standard (`google/protobuf/timestamp.proto`…) :
    `libprotobuf-dev` explicite.
  - `tzdata` obligatoire : jiff résout `[station] timezone` dans
    `/usr/share/zoneinfo` (absent de l'image de base).
  - Liquidsoap lit lui-même fallback, bruit et médias : tout ce qu'il lit
    doit être lisible par `liquidsoap` (groupe `MEDIA_GID` ou lecture pour
    tous). Partage NFS de devstationd en `070 foxi:foxi`. Non expliqué :
    le Liquidsoap natif (106:110, groupes `liquidsoap`/`audio`) n'était pas
    non plus dans ce groupe — remappage côté TrueNAS ou jamais lu depuis le
    NFS ? À éclaircir avant la prod.
  - Le conteneur est en réseau de l'hôte : arrêter tout Icecast / Liquidsoap
    natif avant `docker compose up` (ports 8000, 8081, 50051).
  - Après une modification de `docker/` ou de `.env` : `docker compose build`
    puis `docker compose up -d --force-recreate` (sinon l'ancienne image
    tourne).

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
| `src/schedule_grpc.rs` | Service gRPC scheduling (7 RPC réels, dont `CheckCoverage`) |
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
| `src/station_control.rs` | État de diffusion + file d'override + horloge manuelle (mécanisme de la surface hôte A2) |
| `src/broadcast_grpc.rs` | Transport gRPC `BroadcastService` (état, control, overrides, injection auditeurs) |
| `proto/broadcast_v1.proto` | Contrat contrôle de diffusion (compilé/servi ; 7 RPC, dont `Skip`) |
| `src/ls_script.rs` | Générateur du script Liquidsoap (pur : config → `.liq`) |
| `src/ls_bridge.rs` | Pont HTTP loopback Liquidsoap → stationd (`/next`, `/track`), `on air` / `next` |
| `src/ls_control.rs` | Socket de contrôle stationd → Liquidsoap + tâche air sync (état, overrides) |
| `src/ls_grpc.rs` | Transport gRPC `LiquidsoapService` (`RenderScript`, `GetStatus`) |
| `src/icecast.rs` | Lecture `/admin/stats` Icecast : parseur, audience, client HTTP, moniteur (débit réel), échantillonneur |
| `src/icecast_grpc.rs` | Transport gRPC `IcecastService` (`GetStatus`, `RenderConfig`) |
| `src/icecast_xml.rs` | Générateur `icecast.xml` (pur : `[icecast.server]` + sorties → XML), écriture 0640 |
| `proto/icecast_v1.proto` | Contrat `stationctl icecast status` / `render` |
| `proto/liquidsoap_v1.proto` | Contrat `stationctl ls render` / `ls status` |
| `Doc/liquidsoap.md` | Câblage Liquidsoap : chaîne, contrat du pont, socket, déploiement Docker, limites (référence durable) |
| `compose.yaml` / `docker/Dockerfile.dev` | Conteneur de dev : dépôt monté, stationd + Icecast + Liquidsoap sous s6-overlay |
| `docker/rootfs/etc/s6-overlay/` | Services s6 (`init-perms`, `stationd`, `icecast`, `liquidsoap`) et leur ordre |
| `plugins/stop-when-idle-wasm/` | Guest WASM démo A2 : host function `station_control` |
| `plugins/{require-title,blacklist}-wasm/` | Crates guest WASM de démo (séparés, cible wasm32) : `filter_pool` |
| `src/grpc.rs` | Service `Station` (status/quit/playlist*) |
| `src/db.rs` | Init pool SQLite + migrations |
| `src/main.rs` | Daemon : démarrage, 7 services gRPC, pont Liquidsoap (écrit le `.liq`, sert `/ls/v1`, air sync), échantillonneur Icecast, shutdown |
| `src/bin/stationctl.rs` | CLI (station + `schedule …` + `library …` + `plugin …` + `ls render` / `ls status` + `icecast status` / `render`) |
| `proto/station.proto` | Contrat `Station` |
| `proto/schedule_v1.proto` | Contrat `ScheduleService` (compilé/servi ; 7 RPC réels) |
| `proto/library_v1.proto` | Contrat `LibraryService` (compilé/servi ; Scan + ListMedia) |
| `proto/plugin_v1.proto` | Contrat `PluginService` (compilé/servi ; List + Control) |
| `Doc/plugin-{events,hooks,host}.md` | Contrats du système de plugins (référence durable) |
| `Doc/proposition-grammaire-grille-v1.md` | Contrat grammaire `grid.toml` (référence durable) |
| `proto/playlist_v1.proto` | Contrat playlist v1 (⚠ pas encore compilé/servi) |
| `migrations/0001→0010` | Schéma (0005 état grille, 0006 règles, 0007 biblio, 0008 curseur, 0009 groupe, 0010 runtime/shuffle groupe ; ⚠ 0004 absent) |
| `tests/sync.rs` | Intégration `sync` |
| `Doc/*.md` | Décisions d'architecture (référence durable) |
