# Câblage Liquidsoap

Référence durable du câblage stationd ↔ Liquidsoap. Décision A+C (2026-09-23) :
**A** = pont HTTP loopback Liquidsoap → stationd (Liquidsoap n'a pas de client
gRPC) ; **C** = socket de contrôle stationd → Liquidsoap (étape 2, pas encore
livrée).

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
           pull,
           bruit de fond   (tant que stationd répond halted),
           blank           (tant que stationd n'a jamais répondu — démarrage),
           fallback sécu   (rien à diffuser / stationd injoignable) ])
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

## Choix et constats (validés contre un vrai Liquidsoap)

- **Arrêt ≠ fallback.** `halted` → le bruit de fond prend l'antenne **à la fin
  de la piste en cours** (le pull reste prioritaire tant qu'il a une piste ;
  jamais de coupure). Reprise ≤ 2 s (délai de relance du pull).
- **Fin de piste observée avant le crossfade.** Une piste entrée par un
  crossfade ne déclenche pas `on_track` en aval de `cross` ; on écoute donc
  `pull` (et le bruit / le fallback) directement.
- **Pas d'`annotate:` sur `single`** : ça rend la source faillible ; les
  sources propres sont identifiées par leur propre callback `on_track`.
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
- **Override `hard`** : toujours dégradé en soft → étape 2 (socket, file
  interruptrice).
- **`unplayed_only`** : marquage auto pas encore câblé (la ref de la décision
  est celle du groupe, pas de la feuille) → étape 4.
- **Compteur `Every` au compteur** : avancé à chaque démarrage réel de piste ;
  avec le préchargement, la piste `Every` elle-même peut compter pour 1.
- Encodage : mp3 seulement.
