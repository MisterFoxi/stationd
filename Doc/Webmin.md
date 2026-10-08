# StationD Remote Supervision
## Dossier technique

## 1. Objectif

Mettre à disposition des administrateurs et helpers de StationD une interface de supervision distante accessible depuis un navigateur, sans nécessiter de compétences techniques particulières.

Le système doit permettre :

- de vérifier rapidement l’état de la station ;
- de visualiser le média en cours et la programmation à venir ;
- d’effectuer certaines opérations de contrôle ;
- d’accéder à la TUI StationD depuis un navigateur pour les utilisateurs autorisés ;
- de conserver une séparation stricte entre supervision StationD et accès système ;
- de ne pas exposer directement SSH, Docker ou un shell Linux.

Le système sera implémenté sous forme de plugin StationD.

---

# 2. Principe général

Le plugin fournit une interface HTTPS accessible depuis un navigateur.

L’utilisateur ne se connecte pas directement au système Linux.

Le flux général est :

```text
Utilisateur
    │
    │ HTTPS
    ▼
remote.homestone.rp-radio.live
    │
    ▼
Reverse Proxy
    │
    ▼
StationD Remote Plugin
    │
    ├── Authentification
    ├── Gestion des rôles
    ├── API de supervision
    ├── WebSocket Terminal
    └── Audit
            │
            ▼
         StationD
```

Le plugin communique exclusivement avec StationD et les interfaces officiellement exposées par StationD.

Il ne doit pas nécessiter d’accès root.

---

# 3. Philosophie

StationD conserve son architecture :

```text
                 StationD
                    │
       ┌────────────┼────────────┐
       │            │            │
 stationctl    stationd-tui   remote-plugin
    CLI            TUI            WEB
```

Le plugin distant n’est donc pas un nouveau panneau d’administration indépendant.

Il constitue une nouvelle interface utilisateur au-dessus des fonctions StationD existantes.

L’objectif est d’éviter de reproduire l’approche d’AzuraCast avec une interface Web contenant progressivement toutes les fonctions du système.

---

# 4. Deux niveaux d’interface

Le système comporte deux interfaces complémentaires.

## 4.1 Supervision Web

Interface simple destinée à tous les helpers et administrateurs.

Elle doit fonctionner correctement sur :

- PC ;
- tablette ;
- smartphone.

Exemple :

```text
┌─────────────────────────────────────────────────────┐
│ Home Stone Broadcast                     ● ONLINE   │
├─────────────────────────────────────────────────────┤
│ NOW PLAYING                                         │
│                                                     │
│ The Sellsword — Turia Voyage                        │
│ ████████████████████░░░░░░░░░░  02:51 / 04:32     │
│                                                     │
│ Night Schedule › Relaxed                            │
├─────────────────────────────────────────────────────┤
│ LISTENERS                               5           │
├─────────────────────────────────────────────────────┤
│ NEXT                                                │
│ 23:00  TOPTH                                        │
│ 23:01  Tharns Blade                                 │
│ 23:08  Fresh from Every Realm                       │
├─────────────────────────────────────────────────────┤
│ SYSTEM                                              │
│ StationD   ●                                        │
│ Audio      ●                                        │
│ Icecast    ●                                        │
├─────────────────────────────────────────────────────┤
│ [ Open Console ]                                    │
└─────────────────────────────────────────────────────┘
```

Cette interface doit rester volontairement simple.

---

# 5. Console distante

Le bouton :

```text
Open Console
```

ouvre une session terminal dans le navigateur.

Cette console exécute directement :

```text
stationd-tui
```

et non :

```text
/bin/bash
```

Architecture :

```text
Browser
   │
   │ WebSocket
   ▼
Remote Plugin
   │
   │ PTY
   ▼
stationd-tui
```

La TUI existante devient ainsi utilisable à distance sans installation sur le poste client.

---

# 6. Pourquoi ne pas utiliser directement SSH

L’objectif n’est pas de remplacer SSH.

SSH reste disponible pour les administrateurs techniques.

Le plugin vise des personnes qui ne sont pas nécessairement familières avec :

```text
ssh
ssh-keygen
authorized_keys
terminal
shell
```

Le plugin fournit donc une expérience :

```text
ouvrir le site
↓
s’authentifier
↓
cliquer sur Open Console
↓
StationD TUI
```

Toute la complexité technique reste côté serveur.

---

# 7. Authentification

L’authentification doit éviter autant que possible les mots de passe traditionnels.

La solution privilégiée est :

```text
WebAuthn / Passkeys
```

Exemples de méthodes utilisables :

- Windows Hello ;
- Touch ID ;
- Face ID ;
- clé FIDO2 ;
- téléphone compatible passkey.

Pour l’utilisateur :

```text
Login
  │
  ▼
Use Passkey
  │
  ▼
Windows Hello / téléphone
  │
  ▼
Authenticated
```

Aucune clé privée n’est manipulée manuellement.

---

# 8. Principe cryptographique

Lors de l’enregistrement d’un utilisateur :

```text
Client
  │
  ├── génère paire de clés
  │
  ├── conserve clé privée
  │
  └── transmet clé publique
            │
            ▼
      Remote Plugin
```

Le serveur ne conserve jamais la clé privée.

Lors d’une connexion :

```text
Server
   │
   │ random challenge
   ▼
Client
   │
   │ signed challenge
   ▼
Server
   │
   │ verify public key
   ▼
Authenticated
```

Le système repose donc sur une authentification asymétrique.

---

# 9. Enrôlement des utilisateurs

La création d’un utilisateur est effectuée par un administrateur.

Exemple :

```text
stationctl remote-user add Alice --role helper
```

StationD génère alors un token d’enrôlement temporaire.

Exemple :

```text
User created: Alice
Role: helper

Enrollment URL:

https://remote.homestone.rp-radio.live/enroll/7xK9A...
```

L’administrateur transmet simplement ce lien à Alice.

---

# 10. Premier accès

Alice ouvre le lien.

Elle obtient :

```text
Home Stone Remote Access

Account:
Alice

Role:
Helper

[ Register this device ]
```

Le navigateur propose alors :

```text
Use Windows Hello
Use phone
Use security key
```

Une fois enregistré, le token d’enrôlement est invalidé.

---

# 11. Connexions suivantes

L’interface devient simplement :

```text
Home Stone Remote

Alice

[ Sign in with Passkey ]
```

Puis :

```text
Windows Hello
```

et l’utilisateur arrive sur le dashboard.

---

# 12. Gestion des rôles

Trois rôles sont recommandés.

## Viewer

Droits :

```text
status
now playing
queue
agenda
listeners
events
```

Pas d’action sur la station.

---

## Helper

Droits :

```text
viewer permissions
+
skip media
reload schedule
acknowledge alert
```

Éventuellement accès à une TUI limitée.

---

## Admin

Droits :

```text
helper permissions
+
restart engine
restart StationD
full TUI
user management
```

---

# 13. Permissions fines

L’architecture doit permettre d’aller plus loin que les rôles fixes.

Exemple :

```toml
[[remote.users]]
name = "Alice"
role = "helper"

permissions = [
    "station.read",
    "queue.read",
    "agenda.read",
    "player.skip",
]
```

Les rôles deviennent alors simplement des ensembles prédéfinis de permissions.

---

# 14. Principe de moindre privilège

Même un utilisateur `admin` du plugin ne devient pas administrateur Linux.

La relation doit être :

```text
Remote Admin
      ≠
Linux root
```

Le plugin peut uniquement invoquer les fonctions StationD explicitement autorisées.

---

# 15. Pas de shell système par défaut

Le plugin ne doit jamais exécuter :

```text
/bin/bash
/bin/zsh
sudo
docker
systemctl
```

à partir d’une requête Web utilisateur.

La console standard exécute exclusivement :

```text
stationd-tui
```

---

# 16. Mode terminal

Le navigateur utilise un émulateur de terminal JavaScript.

Technologie recommandée :

```text
xterm.js
```

Communication :

```text
Browser
   │
   │ WebSocket
   ▼
Rust backend
   │
   ▼
Pseudo Terminal
   │
   ▼
stationd-tui
```

---

# 17. PTY

Le backend doit fournir un pseudo-terminal.

Bibliothèque Rust envisageable :

```text
portable-pty
```

Le PTY doit supporter :

- redimensionnement dynamique ;
- séquences ANSI ;
- clavier ;
- couleurs ;
- Unicode ;
- ratatui.

Lorsque le navigateur change de taille :

```text
Browser
    │
    │ resize(cols, rows)
    ▼
WebSocket
    │
    ▼
PTY resize
```

---

# 18. Cycle de vie d’une session

Ouverture :

```text
Authenticated user
       │
       ▼
POST /console/session
       │
       ▼
Permission check
       │
       ▼
PTY create
       │
       ▼
stationd-tui
       │
       ▼
WebSocket upgrade
```

Fermeture :

```text
browser closes
      │
      ▼
WebSocket closed
      │
      ▼
PTY terminated
      │
      ▼
stationd-tui killed
```

Aucun processus orphelin ne doit rester actif.

---

# 19. Sessions multiples

Configuration possible :

```toml
[remote.console]

max_sessions = 5

max_sessions_per_user = 1

idle_timeout = "15m"

max_session_duration = "4h"
```

---

# 20. Timeout

Une session inactive doit automatiquement être fermée.

Exemple :

```text
No activity for 15 minutes

Session expired.
```

Le timeout s’applique aussi aux WebSockets.

---

# 21. Dashboard

Le dashboard doit fournir au minimum :

## Station

```text
ONLINE / OFFLINE

StationD version

uptime

configuration status
```

## Player

```text
artist
title
elapsed
duration
playlist
rule
```

## Scheduler

```text
active DayPart
active rule
next events
```

## Listeners

```text
current listeners
```

## Services

```text
StationD
audio engine
Icecast
plugins
```

---

# 22. Explication de la sélection musicale

StationD doit exposer la cause ayant conduit à la sélection du média.

Exemple :

```text
Tuxis — The Boot Kissing Confession

Selected by

DayPart
  Nights
    ↓
Night Schedule
    ↓
Playlist
  Energique Mood
```

Cela doit permettre de comprendre rapidement les décisions du scheduler.

---

# 23. Queue

Affichage recommandé :

```text
NOW

Tuxis — The Boot Kissing Confession
04:22

NEXT

23:00 TOPTH 3
23:01 Tharns Blade
23:08 Fresh from Every Realm
23:12 ...

```

Les informations de règle doivent être visibles :

```text
TOPTH
[AtClockHard]

Fresh from Every Realm
[Every novelties]
```

---

# 24. Agenda

Le dashboard doit pouvoir ouvrir un agenda.

Vues :

```text
Today

24h

Week
```

Exemple :

```text
22:00 ━━━━━━━━━━━━━━━━━ Night Schedule

23:00     TOPTH

23:08     Fresh Songs

00:00     TOPTH

00:30     Podcast
```

Le moteur de preview existant dans StationD doit servir de source.

---

# 25. Journal métier

Les logs bruts ne constituent pas la vue principale.

Le plugin doit afficher un journal compréhensible :

```text
22:44 PLAY
Tuxis — The Boot Kissing Confession

22:48 PLAY
A Slave's Lament

23:00 CLOCK
TOPTH triggered

23:00 PLAY
Johannesnagy — TOPTH 3

23:00 SCHEDULE
Night Schedule resumed

23:08 EVERY
Fresh Songs triggered
```

---

# 26. Logs techniques

Une vue complémentaire peut fournir :

```text
Events

StationD

Audio Engine

Icecast

Plugins
```

Les logs devront être filtrés côté serveur.

Le navigateur ne doit pas avoir accès directement au filesystem.

---

# 27. Actions rapides

Selon les permissions :

```text
Skip

Reload schedule

Restart audio engine

Restart StationD
```

Chaque action sensible doit demander confirmation.

Exemple :

```text
Restart audio engine?

Listeners may experience a short interruption.

[ Cancel ] [ Restart ]
```

---

# 28. API

Le plugin peut fournir une petite API HTTP.

Exemples :

```text
GET /api/status

GET /api/now-playing

GET /api/queue

GET /api/agenda

GET /api/listeners

GET /api/events

GET /api/plugins
```

Contrôle :

```text
POST /api/control/skip

POST /api/control/reload

POST /api/control/restart-engine
```

La logique métier doit toutefois rester dans StationD.

---

# 29. Communication avec StationD

Solution privilégiée :

```text
Remote Plugin
      │
      │ gRPC
      ▼
StationD
```

Il faut éviter que le plugin aille directement lire ou modifier SQLite.

Flux recommandé :

```text
Browser
   │
   ▼
Remote Plugin
   │
   ▼
StationD gRPC
   │
   ├── Scheduler
   ├── Player
   ├── Queue
   ├── Plugins
   └── Stats
```

---

# 30. Streaming des événements

Pour les données temps réel :

```text
StationD
   │
   │ gRPC Stream
   ▼
Remote Plugin
   │
   │ SSE
   ▼
Browser
```

SSE suffit pour :

```text
now playing
listeners
queue
status
alerts
```

WebSocket est réservé au terminal.

---

# 31. Stack Web recommandée

Backend :

```text
Rust

Axum
Tokio
Tonic
portable-pty
```

Frontend :

```text
HTML

CSS

HTMX

SSE

xterm.js
```

Il n’est pas nécessaire d’introduire React ou Vue dans la première version.

---

# 32. Reverse proxy

Exemple :

```text
Internet
    │
    ▼
Cloudflare
    │
    ▼
Traefik
    │
    ▼
stationd remote plugin
```

Nom proposé :

```text
remote.homestone.rp-radio.live
```

Le plugin lui-même peut écouter uniquement :

```text
127.0.0.1:8090
```

ou sur le réseau Docker interne.

---

# 33. TLS

Le plugin ne doit jamais être exposé directement sans TLS.

Flux obligatoire :

```text
HTTPS
WSS
```

et non :

```text
HTTP
WS
```

---

# 34. Sécurité WebSocket

Chaque WebSocket terminal doit être lié à :

```text
authenticated session

user ID

session ID

permissions
```

Un identifiant de terminal ne doit jamais constituer à lui seul une autorisation.

---

# 35. Protection CSRF

Toutes les opérations de contrôle doivent être protégées contre les requêtes inter-sites.

Exemple :

```text
POST /api/control/skip
```

doit vérifier :

```text
session
CSRF token
permission
origin
```

---

# 36. Protection brute-force

Même avec les passkeys :

- limiter les tentatives ;
- limiter les créations de session ;
- journaliser les échecs ;
- pouvoir bannir temporairement une IP.

---

# 37. Audit

Toutes les actions de contrôle doivent être enregistrées.

Exemple :

```text
2026-10-07 20:14
user=Alice
action=player.skip
result=success
```

Autre exemple :

```text
2026-10-07 20:17
user=Bob
action=engine.restart
result=success
```

---

# 38. Sessions

Les sessions Web doivent pouvoir être révoquées.

Commandes prévues :

```text
stationctl remote-session list

stationctl remote-session revoke <id>

stationctl remote-session revoke-user Alice
```

---

# 39. Gestion utilisateur

Exemples CLI :

```text
stationctl remote-user add Alice --role helper

stationctl remote-user list

stationctl remote-user show Alice

stationctl remote-user role Alice viewer

stationctl remote-user revoke Alice
```

---

# 40. Révocation immédiate

Lorsqu’un utilisateur est supprimé :

```text
stationctl remote-user revoke Alice
```

le plugin doit immédiatement :

```text
revoke Web sessions

close active WebSockets

terminate active PTY

invalidate authentication credentials
```

---

# 41. Appareils multiples

Un utilisateur peut éventuellement enregistrer plusieurs passkeys.

Exemple :

```text
Alice

Devices

Windows PC
registered 2026-10-07

iPhone
registered 2026-10-08
```

Chaque appareil peut être révoqué individuellement.

---

# 42. Administration des utilisateurs

L’administration initiale peut rester CLI/TUI.

À terme :

```text
StationD TUI
  │
  └── Remote Access
         │
         ├── Users
         ├── Sessions
         ├── Devices
         └── Audit
```

Cela permet de ne pas gérer les utilisateurs du plugin depuis le plugin lui-même.

---

# 43. Mode Helper

Le helper doit disposer d’une interface particulièrement simple.

Page d’accueil :

```text
Home Stone

● Broadcasting normally

Listeners: 6

Now Playing
The Sellsword — Turia Voyage

Next
TOPTH at 23:00


[ Open Station Console ]
```

Le helper n’a pas à comprendre l’architecture interne.

---

# 44. Mode Admin

L’administrateur dispose d’informations supplémentaires :

```text
CPU

memory

StationD uptime

audio engine status

Icecast status

plugin status

scheduler state

last errors
```

Ces données restent orientées StationD et non administration Linux.

---

# 45. Mobile

La partie supervision doit être responsive.

Sur téléphone :

```text
Home Stone

● ONLINE

NOW PLAYING
The Sellsword
Turia Voyage

3 listeners

NEXT
23:00 TOPTH

[ Skip ]

[ Console ]
```

Le terminal TUI pourra être utilisable mais ne constituera pas nécessairement l’usage principal sur smartphone.

---

# 46. Dépendance TUI

Le plugin lance :

```text
stationd-tui
```

Le plugin ne doit pas reproduire sa logique.

Ainsi :

```text
stationd-tui v1
        │
remote console v1

stationd-tui v2
        │
remote console automatiquement v2
```

Toute évolution de la TUI bénéficie immédiatement à la console distante.

---

# 47. Séparation des responsabilités

```text
stationctl
administration CLI / scripting

stationd-tui
administration interactive

remote-plugin
accès distant sécurisé

public website
interface auditeurs
```

Ces composants doivent rester indépendants.

---

# 48. Plugin StationD

Nom de travail possible :

```text
remote-console
```

ou :

```text
remote-supervision
```

Exemple de configuration :

```toml
[plugins.remote_supervision]

enabled = true

bind = "127.0.0.1:8090"

public_url = "https://remote.homestone.rp-radio.live"


[plugins.remote_supervision.console]

enabled = true

command = "/usr/local/bin/stationd-tui"

idle_timeout = "15m"

max_sessions = 5

max_sessions_per_user = 1


[plugins.remote_supervision.security]

webauthn = true

password_login = false


[plugins.remote_supervision.audit]

enabled = true

retention_days = 90
```

---

# 49. Limites de sécurité

Le plugin ne doit jamais :

```text
ouvrir un shell Linux arbitraire

transmettre des commandes shell utilisateur

accéder à Docker directement

exécuter sudo

éditer /etc

modifier les clés SSH système

exposer SQLite

exposer des variables d’environnement secrètes
```

---

# 50. Évolution possible

Une version ultérieure pourrait fournir un vrai accès système distant.

Mais cela devrait être un composant distinct :

```text
stationd remote supervision
             ≠
system remote administration
```

Le premier reste dans le périmètre StationD.

---

# 51. MVP

La première version pourrait se limiter à :

```text
Authentication WebAuthn

Users + roles

Status page

Now Playing

Listeners

Next queue

Open Console

stationd-tui over WebSocket

Session timeout

Audit
```

Cela apporte déjà l’essentiel de la supervision distante.

---

# 52. Phase suivante

Deuxième étape :

```text
Agenda

Event log

Skip

Reload schedule

Engine restart

Alerts

User/device administration
```

---

# 53. Architecture finale

```text
                         Internet

                            │
                            │ HTTPS/WSS
                            ▼

                    ┌─────────────────┐
                    │     Traefik     │
                    └────────┬────────┘
                             │
                             ▼
                 ┌───────────────────────┐
                 │ Remote Supervision    │
                 │ StationD Plugin       │
                 │                       │
                 │ WebAuthn              │
                 │ Sessions              │
                 │ Roles                 │
                 │ HTTP API              │
                 │ SSE                   │
                 │ WebSocket             │
                 │ Audit                 │
                 └───────┬────────┬──────┘
                         │        │
                      gRPC       PTY
                         │        │
                         ▼        ▼
                    StationD   stationd-tui
                         │
               ┌─────────┼──────────┐
               │         │          │
            Player    Scheduler   Plugins
               │
               ▼
          Audio Engine
               │
               ▼
            Icecast
```

---

# 54. Résultat recherché

Pour un helper :

```text
open browser

authenticate with Windows Hello

check station

optionally open console

perform authorized action

close browser
```

Pour StationD :

```text
no SSH exposure

no Linux credentials

no duplicated administration UI

existing TUI reused

strong asymmetric authentication

role-based access

complete audit trail
```

Le plugin devient ainsi une **passerelle de supervision StationD sécurisée**, plutôt qu’un serveur SSH accessible depuis le Web.
---

# 55. Supervision multi-stations — exigence du 7 octobre 2026

Une installation Webmin peut gérer plusieurs stations StationD.
Le premier écran après authentification est une **synthèse du réseau**,
et non le dashboard d’une station choisie implicitement.

La synthèse présente les stations accessibles à l’utilisateur : identité,
état de diffusion, média en cours, audience et alertes principales. Les
informations inconnues ou périmées doivent être identifiées ; une station
injoignable ne doit pas rendre les autres indisponibles.

Choisir une station ouvre son dashboard détaillé puis, selon les droits,
ses actions et sa console. La station cible doit toujours être visible,
notamment avant une confirmation d’action et lors de l’ouverture d’une TUI.
Même avec une seule station, conserver cette entrée par la synthèse réseau.

Chaque station dispose d’un identifiant stable et unique, d’un libellé et
d’un endpoint gRPC configuré côté serveur. Le navigateur ne peut pas fournir
un endpoint arbitraire. Les permissions doivent pouvoir être limitées par
station ; être admin sur une station ne donne pas implicitement accès aux
autres. L’audit et les sessions terminal doivent identifier la station cible.

La tranche A prépare le catalogue validé. La synthèse visuelle sera réalisée
avec le dashboard authentifié (tranche C), après les sessions et permissions
(tranche B). La sécurité du transport vers les stations distantes devra être
précisée avant leur connexion : ne pas exposer leur gRPC directement sur
Internet.

# 56. Connexion par mot de passe — décision du 7 octobre 2026

À la demande de l'opérateur, le parcours utilisateur est remplacé par une
connexion nom de compte + mot de passe. Aucun enrôlement d'appareil ni
passkey n'est demandé. Le lien temporaire créé par `remote-user add` ou
`remote-user enroll` sert à définir ou réinitialiser le mot de passe.
La validation d'un nouveau mot de passe révoque les sessions existantes.
Les exigences passkeys des sections précédentes décrivent la proposition
initiale ; cette décision prévaut pour le parcours utilisateur.
Les rôles par station et la synthèse réseau après connexion sont conservés.
