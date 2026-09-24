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
active le serveur socket de Liquidsoap (mode **0660** : l'utilisateur de
stationd doit être dans le groupe de Liquidsoap) et y enregistre :

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
ExecStart=/usr/bin/liquidsoap /data/dev/stationd/data/station.liq
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

Vérifier le script sans le lancer : `liquidsoap --check <script>` ;
l'afficher : `stationctl ls render`.

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
