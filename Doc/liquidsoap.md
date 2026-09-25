# Câblage Liquidsoap

Référence durable du câblage stationd ↔ Liquidsoap. Décision A+C (2026-09-23) :
**A** = pont HTTP loopback Liquidsoap → stationd (Liquidsoap n'a pas de client
gRPC) ; **C** = socket de contrôle stationd → Liquidsoap (pause / resume /
skip).

## Principe : Liquidsoap ne décide rien

Grille, playlists, groupes, overrides, état de diffusion : tout vit dans
stationd. Le script généré ne contient **aucune** logique de programmation
(pas de `.m3u`, pas de `random` pondéré, pas de prédicat horaire — tout ce que
le script AzuraCast portait). Il se réduit à une chaîne de sources et au pont.

```
pull   = request.dynamic(POST /ls/v1/next)        ← stationd résout chaque piste
         on_track(pull) → POST /ls/v1/track        ← avant le crossfade (voir plus bas)
pull   = cross(pull)                               ← optionnel
radio  = fallback(track_sensitive=false, [
           pull            (sauf en pause : piste gelée, non lue),
           bruit de fond   (en pause, ou tant que stationd répond halted),
           blank           (tant que stationd n'a jamais répondu — démarrage),
           fallback sécu   (rien à diffuser / stationd injoignable) ])
radio  = fallback(track_sensitive=false, [interrupt, radio])   ← overrides hard (coupe)
radio  → normalize/compress (option) → %include custom → output.icecast
```

## Contrat du pont (HTTP, 127.0.0.1, en-tête `X-Stationd-Token`)

| Route | Corps | Réponse |
|---|---|---|
| `POST /ls/v1/next` | `{}` | `{kind, uri, state, reason}` — les 4 champs toujours présents |
| `POST /ls/v1/track` | `{rid, kind}` | 200 |

`kind` de `/next` :
- `file` → `uri = annotate:stationd_rid="N":/chemin/absolu` ;
- `halted` → `state = paused|stopped` : **bruit de fond**, jamais le fallback ;
- `relay` → `uri` = l'URL d'une playlist `remote` : relayée (voir « Relais »).
- `none` → `reason = fallback|pool_empty|error` : fallback sécu.

`/track` : `rid` connu = une de nos pistes démarre réellement → compteur de
pistes station (`Every` au compteur) ; `kind = halted|fallback` = une source
propre à Liquidsoap. Visible dans `stationctl ls status` : `on air` (en cours)
et `next` (la piste préparée, demandée quelques secondes avant la fin de la
courante — vide le reste du temps, c'est normal).

### Piste suivante demandée en fin de piste (2026-09-25)

`request.dynamic` garde une requête d'avance : laissé tel quel, il
demanderait la suivante **au démarrage** de la courante, et stationd la
choisirait une piste entière avant sa diffusion (`DayPart`, `AtClock` soft,
`Every`, historique : tout décalé d'une piste). La fonction du pull
(`stationd.next`) répond donc « rien pour l'instant » **sans appeler
stationd** tant que la piste en cours a plus de `stationd.lead` secondes à
jouer (`pull_raw.remaining()`) ; `request.dynamic` réessaie à son délai
(2 s). `lead` = chevauchement du crossfade + délai de réessai + 2 s de marge
(**7 s** par défaut, 4 s sans crossfade) : la suivante est choisie ~5–7 s
avant la fin, ~8–10 s avant d'être entendue. Au démarrage, après une fin de
piste, ou durée restante inconnue (-1) : demande immédiate.

- **skip** : rien n'est préparé d'avance la plupart du temps → `urgent`
  levé et `pull_raw.fetch()` **avant** `source.skip` (sinon le fichier de
  secours comblerait le trou).
- **interrupt** (override / `AtClock` hard) : la piste coupée est sautée, le
  pull (sans piste) redemande tout de suite, pendant l'insert.
- **stop / override soft** : le `flush` n'a en général plus rien à vider ;
  la demande de fin de piste reçoit `halted` / l'override.
- Validé contre Liquidsoap **2.2.4** (script généré, adapté à la syntaxe
  `null()` de la 2.2 pour l'essai, face à un faux stationd ; pistes de 20 s,
  crossfade 3 s) : `/next` 7 s avant chaque début de piste (au lieu de 17 s) ;
  skip → piste suivante immédiate, pas de fallback ; interrupt → pull
  redemandé à la coupe, piste jouée après l'insert. À confirmer sur la 2.4.

### Relais d'une playlist `remote` (2026-09-25)

`/next` répond `relay` + URL : le script mémorise l'URL, (re)démarre
`relay = input.http(id="stationd_relay", start=false, {stationd.relay_url()})`
et ne met aucune piste en file. Dans la chaîne, le relais est **premier**
mais n'est disponible que si `relaying`, pas en pause, et **le pull
(crossfade compris) n'a plus rien** : entrée **soft**, la piste en cours va
au bout — traîne du crossfade comprise (sinon coupée à l'entrée puis
rejouée à la sortie, vu en réel).

- **Veille de la grille** : pendant le relais le pull n'est pas lu et
  `request.dynamic` cesse de redemander (vu en réel) ; un `thread.run`
  toutes les 2 s appelle `pull_raw.fetch()` tant que `relaying` et file
  vide → stationd est interrogé à ce rythme.
- **Sortie** : `file` → la piste se prépare, le relais cède dès qu'elle est
  prête et s'arrête à son début (`stationd.pull_started`) ; `halted` /
  `none` → arrêt immédiat (bruit / secours toujours prêts). Pause → bruit,
  puis relais repris en direct. Insert hard → par-dessus, puis relais repris.
- **Signalement** : bascule vers le relais → `/track` `kind = relay` →
  `ls status` : `on air: relay <url>`. La piste qu'il remplace est jugée
  « jouée en entier » normalement.
- Flux injoignable : `input.http` réessaie seul ; en attendant, fichier de
  secours.
- Validé contre Liquidsoap **2.2.4** (serveur local d'un flux mp3 sans fin,
  faux stationd) : entrée à la fin exacte de la piste (20 s pleines), veille
  toutes les 2 s, sortie vers une piste en ~0,1 s après la réponse `file`,
  sortie vers le bruit sur `halted` dans la seconde, insert hard par-dessus
  puis piste suivante. À confirmer sur la 2.4 (`input.http` : `start`,
  `stop`, `is_started`, URL en fonction).

### `AtClock` hard : coupe à l'heure pile (2026-09-25)

Une tâche horloge de stationd (`ls_control::spawn_at_clock_ticker`) dort
jusqu'au prochain repère **hard** (`GridEngine::next_hard_mark` : le
résolveur lui-même, réduit aux `AtClock` hard, parcouru de minute en minute —
fuseau, heure d'été et jetons consommés compris), en se replanifiant au
moins toutes les 60 s (nouvelle grille, `clock set`). Au repère, elle envoie
`AirEvent::HardMark` à la tâche du socket, qui coupe **comme un override
hard** : `flush` (sauf piste préparée d'override) puis `interrupt <uri>` ;
après l'insert, le pull redemande et la grille reprend.

`GridEngine::air_at_clock_hard` ne coupe que si c'est vraiment l'heure :
au plus **10 s** après le repère, station **à l'antenne** (ni pause ni
arrêt), `AtClockHard` de CE repère dû et non consommé. Alors la source est
résolue comme au pull et le **jeton consommé** (le pull suivant ne la rejoue
pas ; si le pull tombe pile au repère et la prend avant, pas de double).
Sinon, pas de coupe et le jeton reste libre : la règle passe **en soft** au
prochain bord de piste, dans la limite de son `expiry` (station en pause,
redémarrage ou saut d'horloge après le repère, pool vide). Précision : la
seconde (réveil sur l'horloge système) ; horloge de station figée sur un
repère (`clock set`) → coupe dans la minute.

### Fin de piste (déduite, 2026-09-25)

Liquidsoap ne signale que les **débuts**. Une de nos pistes **quitte
l'antenne** quand autre chose démarre : piste suivante (ou `rid` inconnu),
bruit de fond après un `stop`, fallback. Le pont compte le **temps réellement
passé à l'antenne** : une pause le gèle (bruit de fond pendant que l'état est
`paused`), le `resume` le relance ; une piste gelée puis abandonnée (skip en
pause) quitte l'antenne avec son temps gelé. À la sortie,
`GridEngine::on_track_left` compare ce temps à la durée indexée :
**jouée en entier** si `temps + 15 s ≥ durée` (la marge couvre le crossfade
et la seconde du pont) → marque `unplayed_only` (`episode_play`) de la
**playlist feuille** qui a produit le fichier (membre du groupe, jamais le
groupe : `Resolved::File` porte la feuille jusqu'au pont). Skip, override
hard, fallback, redémarrage de Liquidsoap : temps trop court, pas de marque.
Override média (fichier direct) ou durée inconnue : jamais de marque.
L'horloge est celle de la station (`clock set` fige le décompte).

## Socket de contrôle (stationd → Liquidsoap)

`[liquidsoap] control_socket` (défaut `./data/liquidsoap.sock` ; en Docker
`/run/stationd/liquidsoap.sock`, cf. Déploiement). Le script active le
serveur socket de Liquidsoap (mode **0660** : lisible par le groupe de
Liquidsoap, qui doit être le groupe partagé `stationd`) et y enregistre :

| Commande | Effet |
|---|---|
| `stationd.pause` | **immédiat** : la piste en cours est gelée (plus lue), le bruit de fond passe à l'antenne |
| `stationd.resume` | la piste gelée reprend **là où elle s'était arrêtée** |
| `stationd.skip` | la piste en cours est abandonnée, la piste préparée démarre (crossfade) ; en pause, elle démarrera au resume |
| `stationd.flush` | vide la piste déjà préparée (préchargée) : le pull redemande à stationd |
| `stationd.interrupt <uri>` | override hard : la file `interrupt` coupe l'antenne maintenant ; la piste coupée est abandonnée (skip) ; à la fin de l'insert, la piste préparée démarre |
| `stationd.state` | diagnostic : `paused= halted= loading=` |

Qui envoie quoi :
- pause / resume suivent **la machine d'états de diffusion** (`StationControl`) :
  toute transition vers `paused` pousse `pause`, toute transition vers
  `running` pousse `resume` — qu'elle vienne du CLI ou d'un plugin. Un échec
  est journalisé, visible dans `stationctl ls status` (`control:`) et
  **retenté** toutes les 5 s jusqu'à ce qu'il passe ou soit remplacé. Au
  démarrage de stationd, l'état restauré est réaffirmé une fois ;
- `stop` reste gracieux (la piste en cours va au bout) mais pousse `flush` :
  la piste préparée est abandonnée, le pull redemande et reçoit `halted`, le
  bruit de fond arrive **à la fin de la piste en cours** (pas une piste plus
  tard). Exception : si le crossfade a déjà commencé à mixer la suivante, elle
  passe. La piste abandonnée reste comptée dans l'historique anti-répétition
  (écrit à la résolution). `stop` pendant une pause laisse la piste gelée ; un
  `resume` ultérieur la reprend ;
- **overrides** (même tâche, `AirEvent::Override`) :
  - `soft` → `flush` : l'override est servi au prochain pull, donc **à la fin
    de la piste en cours** (plus une piste plus tard) ;
  - `hard` → résolu tout de suite (une piste consommée), `flush` puis
    `interrupt <uri>` : coupe immédiate, puis la suite (reste d'un override
    playlist multi-pistes, ou la grille). Station en pause/arrêtée ou
    Liquidsoap non câblé → dégradé en soft (`degraded` au push). Un cut qui
    échoue (socket injoignable) n'est pas rejoué plus tard : erreur bruyante ;
  - **une piste préparée qui vient d'un override n'est jamais vidée** (elle a
    été consommée de la file en étant servie) : elle passe d'abord — avant un
    stop, avant un override soft plus récent, et juste après l'insert d'un
    hard ;
- skip = RPC `BroadcastService.Skip` (`stationctl station next`, alias `skip`) :
  réponse synchrone, erreur `unavailable` si le socket ne répond pas.

## Choix et constats (validés contre un vrai Liquidsoap)

- **Arrêt ≠ fallback.** `halted` → le bruit de fond prend l'antenne **à la fin
  de la piste en cours** (le pull reste prioritaire tant qu'il a une piste ;
  jamais de coupure). Reprise ≤ 2 s (délai de relance du pull).
- **Fin de piste observée avant le crossfade.** Une piste entrée par un
  crossfade ne déclenche pas `on_track` en aval de `cross` ; on écoute donc
  `pull` (et le bruit / le fallback) directement.
- **Pas d'`annotate:` sur `single`** : ça rend la source faillible.
- **Sources propres (bruit, fallback) signalées à la bascule**, via les
  `transitions` du `fallback` (appelées à chaque bascule, reprise comprise),
  pas par `on_track` : une boucle de bruit laissée en cours est **reprise** au
  stop suivant (aucun nouveau morceau) → le stop n'était pas signalé et
  `ls status` restait sur `stopping`. Appel HTTP hors thread audio
  (`thread.run`).
- **Chemins absolus** : Liquidsoap ne partage pas le répertoire courant de
  stationd.
- **Rafale de `/next`** autour d'une fin de piste quand la réponse est vide :
  bornée côté script (pas de nouvel appel avant `retry_delay` après une réponse
  vide).

## Icecast (lecture seule, `[icecast]`)

stationd **lit** Icecast par l'API admin (`GET /admin/stats`, Basic auth
admin — pas le mot de passe source), en `http://` direct, jamais via le
reverse proxy, toutes les `poll_interval` s (15 par défaut). Liquidsoap
reste le seul client source ; stationd n'écrit rien dans Icecast.

- **Audience** = somme des `<listeners>` des mounts de
  `[[liquidsoap.output]]` (pas le `<listeners>` global du serveur) →
  `StationControl::sample_listeners` → `ListenersSampled` → stop-when-idle.
- **Inconnue, jamais 0** si : Icecast injoignable / pas de réponse en 3 s,
  401, réponse non XML ou `<report>` d'erreur, un de nos mounts absent,
  sans source (`stream_start_iso8601` absent) ou sans `<listeners>`.
  L'audience est alors oubliée (`clear_listeners`) : un `draining` ne
  s'achève pas.
- **Santé** (`stationctl icecast status`) : par mount, source connectée
  depuis quand (IP, user-agent), débit annoncé (`<audio_info>` en 2.5) et
  débit **reçu** mesuré, auditeurs / pic, titre ICY vu par les auditeurs.
  Débit reçu = croissance de `total_bytes_read` en **moyenne glissante sur
  60 s** (affiché après 30 s) : Icecast ne rafraîchit ce compteur que toutes
  les ~5 s, un écart entre deux lectures serait faux jusqu'à ±33 %. 0 sur
  toute la fenêtre = source bloquée.

Constats Icecast 2.5.0 : erreurs en enveloppe `<report>` (reportxml) avec
`<incident><state><text>` ; plus de `<bitrate>` par source ; titre mp3 en
une seule chaîne (`title` = `display-title` = `x_icy_title`).

Le passage `draining → stopped` a lieu au pull suivant, désormais demandé
en fin de piste : le bruit arrive à la fin de la piste en cours (plus de
piste de trop, sauf échantillon à 0 reçu dans les dernières secondes, après
la demande).

### `icecast.xml` généré (`[icecast.server]`)

Même modèle que le `.liq` : stationd **écrit** `config_path` au démarrage
(seulement s'il a changé, atomique, mode **0640** — il contient les mots de
passe), ne lance pas Icecast ; `stationctl icecast render` l'affiche.
Contenu, tiré de `stationd.toml` :

- un `<mount>` par `[[liquidsoap.output]]`, avec **son** mot de passe source
  (Liquidsoap et Icecast ne peuvent pas diverger) et
  `<charset>UTF-8</charset>` (sans lui, Icecast peut lire les titres mp3 en
  ISO-8859-1) ;
- **pas de `<source-password>` global** : vérifié sur un vrai Icecast
  (2.4.4) — avec un global, qui le connaît peut créer n'importe quel autre
  mount ; sans, un mount non déclaré est refusé (401), les nôtres acceptent
  leur propre mot de passe. `<sources>` = nombre de sorties ;
- admin = `[icecast]` (celui que lit l'échantillonneur) ; refusé au
  chargement s'il est égal à un mot de passe source, si `admin_url` ou une
  sortie ne vise pas `port` ;
- chemins d'install (`share_dir`, défaut paquet Debian/Ubuntu
  `/usr/share/icecast2` : `web/`, `admin/`, `report-db.xml`), `log_dir`,
  en-têtes CORS (lecteurs web), comme la config livrée avec la 2.5.

### Derrière un reverse proxy (`X-Forwarded-For`, Icecast 2.5)

La 2.4 officielle ignore `X-Forwarded-For` : derrière Traefik, tous les
auditeurs avaient l'IP de Traefik (le *nombre* restait juste ; les stats par
IP — uniques, géo — étaient perdues). La 2.5 lit l'en-tête pour les
connexions venant d'un proxy déclaré : un socket virtuel **par adresse**,
référencé par le socket public (forme validée sur azuradev, qui en
déclarait deux) :

```xml
<listen-socket id="public">
    <port>8000</port>
    <trusted-proxy>#proxy-1</trusted-proxy>
</listen-socket>
<listen-socket id="proxy-1" type="virtual">
    <client-address>192.168.1.94</client-address>   <!-- Traefik -->
</listen-socket>
```

Généré depuis `[icecast.server] trusted_proxies` (adresses IP exactes ; pas
de CIDR, non documenté). N'y mettre **que** le proxy : toute connexion venant
d'une adresse de confiance peut fournir un `X-Forwarded-For` arbitraire.
stationd et Liquidsoap parlent à Icecast en direct, sans cet en-tête : non
concernés. Le comptage d'audience de stationd n'en dépend pas.

## Déploiement (Docker, 2026-09-25)

stationd, Icecast et Liquidsoap tournent dans **un conteneur**, supervisés
par s6-overlay (`docker/Dockerfile.dev`, `compose.yaml`, services dans
`docker/rootfs/etc/s6-overlay/s6-rc.d`). Procédure complète : README,
« Run (Docker) ». Remplace les unités systemd de l'installation native.

stationd **écrit** le `.liq` et `icecast.xml` (au démarrage, seulement s'ils
ont changé) mais ne lance ni Liquidsoap ni Icecast : c'est s6 qui les lance,
chacun sous son propre service.

```
init-perms (oneshot, root) → stationd ──(prêt : répond en gRPC)──→ icecast → liquidsoap
```

- **Prêt = répond en gRPC** (`s6-notifyoncheck` + `stationctl status`) :
  `main.rs` écrit les deux fichiers **avant** d'ouvrir le port gRPC, donc
  Icecast et Liquidsoap ne démarrent jamais sur une version périmée.
- **Redémarrer stationd ne coupe pas l'antenne** : s6 ne relance pas les
  services qui en dépendent ; Liquidsoap passe sur le fallback le temps du
  redémarrage (`s6-svc -r /run/service/stationd`).
- Après une modification de `[liquidsoap]`, `[icecast]`, `[icecast.server]`
  ou des sorties : relancer stationd (réécrit les fichiers), puis
  `s6-svc -r /run/service/icecast` et/ou `/run/service/liquidsoap`.
- Réseau de l'hôte (`network_mode: host`) : Icecast voit la vraie IP du
  reverse proxy (`trusted_proxies`), rien ne change pour l'écoute
  (`http://<hôte>:8000/<mount>`).

Vérifier le script sans le lancer : `liquidsoap --check <script>` ;
l'afficher : `stationctl ls render`.

### Utilisateurs et groupe partagé `stationd`

Même modèle que l'installation native — ce que les trois processus
partagent passe par le groupe `stationd` — mais figé dans l'image. Le GID
de `stationd` est celui de l'hôte (argument de build `STATIOND_GID`, `.env`).

| Utilisateur | Groupes | Pourquoi |
|---|---|---|
| `dev` (UID/GID de l'hôte) | `stationd` | lance stationd (et cargo) ; les fichiers écrits sur le dépôt monté restent à l'utilisateur de l'hôte |
| `icecast2` (paquet) | + `stationd` | lit `icecast.xml` (0640, `file_group = "stationd"`) |
| `liquidsoap` | principal `stationd`, + groupe des médias (`MEDIA_GID`) | crée le socket de contrôle (0660) ; lit fallback, bruit de fond et médias |

- **Socket de contrôle** : `/run/stationd/liquidsoap.sock`. `init-perms` crée
  `/run/stationd` (tmpfs du conteneur, `root:stationd`, `2770`) : le socket
  n'est plus sur le dépôt monté (partage Samba), qui n'a pas à être
  inscriptible par `liquidsoap`.
- **Médias** : Liquidsoap lit chaque piste lui-même. Le partage NFS de
  devstationd est en `070 foxi:foxi` (accès par le groupe seulement) →
  `liquidsoap` rejoint le groupe propriétaire (`MEDIA_GID =
  $(stat -c %g /mnt/nfs/radio)`). Même chose pour `fallback_path` /
  `halted_path` : lisibles par ce groupe ou par tous, sinon « Infallible
  source.dynamic … was not able to prepare source » et Liquidsoap s'arrête.
  stationd vérifie au démarrage (`LiquidsoapConfig::check_air_files`) :
  absent, vide, pas un fichier ou illisible par stationd → **démarrage
  refusé** ; non lisible par « les autres » → avertissement (mode, uid,
  gid), stationd ne pouvant pas vérifier les droits de Liquidsoap.
  `api_token` : ASCII imprimable, refusé au chargement sinon.
- `icecast.xml` : stationd le donne au groupe `file_group` à chaque
  démarrage (erreur bruyante si le groupe n'existe pas ou si stationd n'en
  est pas membre) ; droits réels journalisés
  (`Icecast config access … access=0640, group stationd`) et rappelés par
  `stationctl icecast render`.

### Installation native (abandonnée)

Avant Docker, chaque processus avait son unité systemd (`icecast-stationd`,
`liquidsoap-stationd` : `icecast2 -c data/icecast.xml` sous
`User=icecast2` + `SupplementaryGroups=stationd`, `liquidsoap
data/station.liq` sous `Group=stationd`) et le groupe `stationd` était
créé à la main (`groupadd --system stationd`). Détails dans l'historique git
de ce fichier. Constats de cette époque toujours valables : **Icecast 2.5
plante (SEGV) sans message quand sa config est illisible** ; `status=216/GROUP`
de systemd = groupe ou utilisateur de l'unité inexistant.

## Limites connues (étapes suivantes)

- **Résolution quelques secondes avant la fin** (plus une piste en avance) :
  un `AtClock` soft dont le repère tombe dans ces dernières secondes passe
  une piste plus tard (pour l'heure pile : `hard`). Un `flush` qui tombe
  après la demande de fin de piste jette encore une piste déjà résolue
  (effets de bord gardés) — rare désormais.
- **Relais** : un `remote` en `at_clock` / `every` ne dure qu'une
  interrogation (~2 s) ; dans un groupe, il lui faut un `runtime`
  (cf. `Doc/playlists.md`). Un `skip` pendant un relais ne fait rien (il n'y
  a pas de piste à sauter ; la grille reprend quand elle ne désigne plus le
  flux).
- **Coupe sèche** d'un override hard ou d'un `AtClock` hard (pas de fondu
  sur la piste coupée).
- **Compteur `Every` au compteur** : avancé à chaque démarrage réel de piste ;
  la piste suivante étant résolue avant le démarrage de la courante (de
  quelques secondes), la piste `Every` elle-même peut compter pour 1.
- Encodage : mp3 seulement.
