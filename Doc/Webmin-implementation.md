# Webmin — démarrage de l’implémentation

Date : 7 octobre 2026.
Référence fonctionnelle : [Webmin.md](Webmin.md).

Ce document rapproche le dossier fonctionnel du code existant. Les choix
ci-dessous sont des propositions de départ ; aucune fonction Webmin n’est
encore implémentée. Le nom Webmin désigne ici la supervision StationD décrite
dans le dossier.

## 1. État du dépôt

- Le daemon est un crate Rust, avec Axum 0.7 et Tokio déjà présents.
- `crates/stationd-proto` fournit le contrat gRPC partagé par les clients.
- `crates/stationd-tui` est un client gRPC indépendant du daemon.
- `src/plugin.rs` contient le trait `Plugin`, le registre natif, les plugins
  WASM et leur cycle de vie. Les appels `on_load` / `on_unload` sont synchrones.
- Les plugins WASM ne disposent pas d’un accès direct aux sockets ou au
  système de fichiers. Le serveur HTTP et le PTY ne peuvent donc pas être
  ajoutés comme un simple plugin WASM avec les capacités actuelles.
- Les déclarations réelles utilisent `[[plugin]]` et `[plugin.config]`.
  L’exemple `[plugins.remote_supervision]` du dossier n’est pas directement
  une configuration utilisable avec le parseur actuel.

## 2. Intégration proposée

Commencer par un plugin natif nommé `remote-supervision`, enregistré dans
la fabrique existante. Organiser son code dans un module dédié, hors du
fichier principal du runtime de plugins.

Le plugin ouvre un listener interne et lance une tâche Tokio pour Axum.
`on_load` doit rendre la main rapidement : aucune boucle HTTP ou connexion
gRPC longue ne doit bloquer l’acteur des plugins. Un échec d’ouverture du
listener doit être visible comme un échec de chargement.

L’arrêt ou le rechargement doit arrêter le listener, les flux SSE et les
consoles, puis terminer les processus TUI. Le contrat synchrone actuel de
`on_unload` nécessite de définir comment attendre effectivement cette fin
avant de déclarer le plugin arrêté ou de rouvrir le même port.

Cette intégration est du code natif de confiance : elle ne bénéficie pas du
confinement WASM. Garder HTTP, autorisations et PTY derrière des interfaces
étroites. Une passerelle dans un processus séparé reste une alternative si
l’isolation du daemon devient prioritaire.

Ne pas lire ni modifier la base métier StationD depuis Webmin. Les lectures
et commandes de station passent par les API officielles. Le stockage des
identités, sessions, challenges et de l’audit doit avoir un contrat explicite :
la capacité `db` existante offre une base privée au plugin, mais il reste à
vérifier son usage concurrent depuis les tâches HTTP et la révocation.

## 3. Sources de données disponibles

| Besoin Webmin | Contrat existant | Traitement à prévoir |
|---|---|---|
| Identité et uptime | `Station.Status` | Projection publique ; version et état de configuration absents de cette réponse |
| État de diffusion | `BroadcastService.GetState` | Ne pas confondre daemon joignable et diffusion effective |
| Média en cours | `OnAirService.Watch` | Métadonnées, durée facultative, début réel |
| Auditeurs | `OnAirSnapshot.listeners`, `IcecastService.GetStatus` | Une valeur absente signifie inconnu, jamais zéro |
| Suite | `OnAirSnapshot.prefetched` et `upcoming` | Distinguer le préchargé de la simulation ; afficher les réserves `notes` |
| Origine de sélection | `Track.origin`, `rule_id`, `playlist_ref`, `leaf_ref` | Présentation lisible sans reproduire le scheduler |
| Programmation | `current_playlist`, `next_playlists`, `ScheduleService.Preview` | Preview bornée pour l’agenda du lot suivant |
| Moteur audio | `LiquidsoapService.GetStatus` | État observé du pont ; ne pas en déduire une santé complète du processus |
| Icecast | `IcecastService.GetStatus` | Fraîcheur du relevé et problème éventuel |
| Plugins | `PluginService.List` | Liste de champs explicitement publiables |
| Journal | `EventService.Watch`, `OnAirService.History` | Traduire les événements et filtrer les informations techniques |
| Skip | `BroadcastService.Skip` | Autorisation, confirmation, protection CSRF et audit |
| Recharger la grille | `ScheduleService.ReloadGrid` | Clarifier la grille ciblée et les erreurs |
| Redémarrer StationD | `Station.Quit` | Sortie pour relance par le superviseur ; connexion interrompue attendue |

Ne pas convertir les réponses protobuf complètes en JSON public. Des services
exposent aussi des chemins, du SQL, des configurations ou des scripts :
Webmin doit publier des DTO dédiés et une liste fermée de routes.
Le contrat Liquidsoap inspecté ne propose pas de RPC de redémarrage moteur.
Ne pas remplacer cette absence par une commande `systemctl` ou Docker.

## 4. Console et permissions

La TUI actuelle est un client d’administration gRPC. Aucun mode helper avec
contrôle des permissions côté serveur n’a été identifié dans les fichiers
inspectés. Masquer des boutons du dashboard ne limite pas les commandes
accessibles depuis une TUI complète.

Proposition pour le MVP :

- viewer : supervision en lecture seule ;
- helper : supervision, puis actions Web explicitement autorisées lors du
  lot de contrôle ;
- admin : supervision et console TUI complète.

La console helper reste désactivée jusqu’à l’existence d’un contrôle fiable
sur les RPC de sa session. Un mode visuel restreint dans la TUI ne suffirait
pas, à lui seul, à garantir cette limite.

Le programme TUI et ses arguments sont fixés côté serveur. Le client fournit
uniquement les entrées terminal et une taille bornée ; il ne choisit ni
exécutable, ni arguments, ni endpoint gRPC, ni variables d’environnement.
La révocation et l’expiration doivent aussi fermer SSE, WebSocket et PTY.

## 5. Lots de réalisation

### Lot A — socle du plugin

- Définir la configuration dans le format `[[plugin]]` existant.
- Valider bind interne, URL publique HTTPS et limites de ressources.
- Enregistrer le plugin natif et implémenter son cycle de vie asynchrone.
- Fournir uniquement une sonde minimale interne, sans données de station.
- Garder le plugin désactivé par défaut et les routes métier fermées.

Validation : configuration invalide refusée, port occupé visible, démarrage
et arrêt reproductibles, port libéré au rechargement, absence de blocage de
l’acteur des plugins.

### Lot B — identité et sessions

- Définir le stockage propre au plugin et son accès CLI officiel.
- Enrôlement temporaire à usage unique ; premier admin créé localement.
- WebAuthn avec challenges expirables, RP ID et origin issus de la
  configuration serveur ; validation par une bibliothèque adaptée.
- Cookies de session sécurisés, expiration, révocation, rôles et permissions.
- Protection des mutations contre CSRF, contrôle d’origin et limites de débit.
- Audit sans secrets, tokens d’enrôlement ni contenu brut de terminal.

Validation : rejeu et enrôlement expiré refusés, compte révoqué refusé,
permissions vérifiées côté serveur, origin incorrect rejeté, aucune route
métier accessible sans session valide.

### Lot C — dashboard authentifié

- Adapter les RPC de lecture aux DTO Webmin.
- Mutualiser le flux d’antenne et le distribuer aux abonnés SSE avec des
  buffers bornés ; ne pas lancer une simulation indépendante par navigateur.
- Construire l’interface responsive : état, média, audience, suite, services.
- Signaler les données inconnues, périmées ou la perte de connexion gRPC.

Validation : données réelles comparées à la TUI, absence de champs secrets,
déconnexion et reconnexion visibles, audience inconnue correctement affichée,
suite simulée identifiée, clients lents sans blocage du serveur.

### Lot D — console admin

- Intégrer PTY, WebSocket et terminal navigateur.
- Lier chaque console à une session Web valide et à l’utilisateur propriétaire.
- Appliquer quotas, bornes de taille, timeout d’inactivité et durée maximale.
- Définir l’activité comme les entrées utilisateur ; la sortie périodique de
  la TUI ne doit pas prolonger indéfiniment une session inactive.
- Nettoyer le processus et ses descendants à chaque fermeture ou révocation.

Validation sous Linux : ANSI/Unicode, redimensionnement, reconnexion interdite
avec session révoquée, accès d’un autre utilisateur refusé, fermeture du
navigateur, expiration et arrêt du plugin sans processus orphelin.

### Lot E — actions et vues complémentaires

Ajouter skip, rechargement de grille, agenda et journal métier. Chaque
mutation suit le même chemin session → origin/CSRF → permission → audit →
RPC. Les redémarrages et la console helper attendent leurs contrats dédiés.

## 6. Premier incrément à coder

Le lot A est le premier incrément proposé. Il ne publie pas encore le
dashboard et ne crée aucun accès distant non authentifié à StationD.

Avant d’élargir son périmètre, arrêter le contrat de fin des tâches au
rechargement et celui du stockage des identités. Ne pas activer le plugin
dans la configuration de production pendant ce travail.

## 7. Vérification locale de cette analyse

Analyse réalisée par lecture des sources Rust et des contrats protobuf.
Aucune modification du runtime, de la configuration active ou du déploiement.
Ni `cargo` ni `rustc` ne sont disponibles dans le PATH Windows de cette
session ; la compilation devra utiliser l’environnement de développement
Rust prévu pour StationD. Les tests PTY devront s’exécuter sous Linux.
