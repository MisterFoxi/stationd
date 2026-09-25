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
- `none` → `reason = fallback|pool_empty|stream_unsupported|error` : fallback sécu.

`/track` : `rid` connu = une de nos pistes démarre réellement → compteur de
pistes station (`Every` au compteur) ; `kind = halted|fallback` = une source
propre à Liquidsoap. Visible dans `stationctl ls status` : `on air` (en cours)
et `next` (la piste préchargée — Liquidsoap en demande une d'avance, au
démarrage de la courante).

## Socket de contrôle (stationd → Liquidsoap)

`[liquidsoap] control_socket` (défaut `./data/liquidsoap.sock`). Le script
active le serveur socket de Liquidsoap (mode **0660** : lisible par le
groupe de Liquidsoap, qui doit être le groupe partagé `stationd`, cf.
Déploiement) et y enregistre :

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

Limite connue : le passage `draining → stopped` a lieu au pull suivant ; la
piste déjà préparée par Liquidsoap passe d'abord (une piste de plus qu'un
`stop`).

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

## Déploiement

stationd **écrit** le script (au démarrage, seulement s'il a changé) ; il ne
lance pas Liquidsoap. Unité séparée → redémarrer stationd ne coupe pas
l'antenne (Liquidsoap passe sur le fallback le temps du redémarrage).

```ini
# /etc/systemd/system/liquidsoap-stationd.service
[Unit]
Description=Liquidsoap (script généré par stationd)
After=network.target

[Service]
User=liquidsoap
Group=stationd
ExecStart=/usr/bin/liquidsoap /data/dev/stationd/data/station.liq
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

Vérifier le script sans le lancer : `liquidsoap --check <script>` ;
l'afficher : `stationctl ls render`.

### Le groupe partagé `stationd`

stationd, Liquidsoap et Icecast tournent sous des utilisateurs que
l'installation choisit : aucun nom d'utilisateur n'est supposé. Ce qu'ils
partagent passe par **un groupe système créé à l'installation**, `stationd` :

- le socket de contrôle, créé par Liquidsoap en 0660 → Liquidsoap tourne
  avec `Group=stationd` ;
- `icecast.xml`, écrit par stationd en 0640 (il contient les mots de passe)
  → `[icecast.server] file_group = "stationd"` ; stationd donne le fichier à
  ce groupe à chaque démarrage (erreur bruyante si le groupe n'existe pas ou
  si stationd n'en est pas membre) ; Icecast tourne avec
  `SupplementaryGroups=stationd`.

```bash
sudo groupadd --system stationd
sudo usermod -aG stationd <utilisateur qui lance stationd>   # se reconnecter ensuite
```

Au démarrage, stationd journalise les droits réels du fichier
(`Icecast config access … access=0640, group stationd`) ; `stationctl icecast
render` les rappelle en en-tête.

### Lancer Icecast sur la config générée

stationd écrit `data/icecast.xml` mais ne lance pas Icecast. Il faut donc :

1. **Arrêter l'Icecast du paquet**, qui lit `/etc/icecast2/icecast.xml` :
   `sudo systemctl disable --now icecast2`
2. **Créer une unité qui lance Icecast sur le fichier de stationd** :

   ```ini
   # /etc/systemd/system/icecast-stationd.service
   [Unit]
   Description=Icecast (config générée par stationd)
   After=network.target

   [Service]
   User=icecast2                 # l'utilisateur d'Icecast sur cette machine
   SupplementaryGroups=stationd
   ExecStart=/usr/bin/icecast2 -c /data/dev/stationd/data/icecast.xml
   Restart=always
   RestartSec=2

   [Install]
   WantedBy=multi-user.target
   ```

   `User=` : le nom dépend de l'installation (`getent passwd | grep -i
   icecast` ; `icecast2` pour le paquet Debian/Ubuntu). `SupplementaryGroups`
   donne à Icecast le groupe du fichier (0640) : sans lui, il ne peut pas lire
   sa config (`status=216/GROUP` = groupe inexistant).
3. `sudo systemctl daemon-reload && sudo systemctl enable --now icecast-stationd`

Après une modification de `[icecast]`, `[icecast.server]` ou des sorties :
relancer stationd (il réécrit le fichier), puis
`sudo systemctl restart icecast-stationd`.

Pour voir le fichier que stationd a généré : `stationctl icecast render`.

## Limites connues (étapes suivantes)

- **Résolution une piste en avance** : `request.dynamic` demande la suivante
  quand la courante démarre. Journal de diffusion, jeton `AtClock`, reset
  `Every` sont écrits à la résolution. Un override soft / un stop agit après
  la piste déjà préparée. → étape 3 (`remaining`) + `flush` (étape 2).
- **Remote** : non relayé (`none/stream_unsupported`, warn) → étape 3
  (`input.http` piloté par une tâche stationd).
- **Coupe sèche** d'un override hard (pas de fondu sur la piste coupée).
- **`unplayed_only`** : marquage auto pas encore câblé (la ref de la décision
  est celle du groupe, pas de la feuille) → étape 4.
- **Compteur `Every` au compteur** : avancé à chaque démarrage réel de piste ;
  avec le préchargement, la piste `Every` elle-même peut compter pour 1.
- Encodage : mp3 seulement.
