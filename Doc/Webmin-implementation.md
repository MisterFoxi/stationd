# Webmin — démarrage de l’implémentation

Date : 7 octobre 2026.
Référence fonctionnelle : [Webmin.md](Webmin.md).

Ce document rapproche le dossier fonctionnel du code existant. Les choix
ci-dessous sont des propositions de départ ; les tranches A et B sont maintenant
implémentées (voir §8 et §10). Le nom Webmin désigne ici la supervision StationD décrite
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
- Construire l’accueil responsive sous forme de synthèse du réseau des
  stations autorisées : état, média, audience et alertes par station.
- Ouvrir le détail de la station sélectionnée : média, suite, services ;
  maintenir sa cible visible pour les actions et la console.
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

Le lot A est le premier incrément implémenté. Il ne publie pas encore le
dashboard et ne crée aucun accès distant non authentifié à StationD.

Avant d’élargir son périmètre, arrêter le contrat de fin des tâches au
rechargement et celui du stockage des identités. Ne pas activer le plugin
dans la configuration de production pendant ce travail.

## 7. Vérification locale de cette analyse

Analyse réalisée par lecture des sources Rust et des contrats protobuf.
Cette analyse initiale précédait la tranche A. La configuration active et le déploiement ne sont pas modifiés.
Ni `cargo` ni `rustc` ne sont disponibles dans le PATH Windows de cette
session ; la compilation devra utiliser l’environnement de développement
Rust prévu pour StationD. Les tests PTY devront s’exécuter sous Linux.

## 8. Tranche A — réalisation

Implémentation : `src/plugin/remote_supervision.rs`, enregistrée dans la
fabrique native de `src/plugin.rs`. Aucun ajout de dépendance Cargo.

- La configuration est validée dès le chargement du fichier de station,
  même si la déclaration native est désactivée, puis à la construction du plugin.
- `bind` est une adresse IP loopback avec un port non nul.
- `public_url` est une origine HTTPS sans identifiants, chemin autre que `/`,
  query ni fragment. Elle prépare le contrat d’authentification ; le plugin
  ne termine pas TLS lui-même.
- Le catalogue contient 1 à 64 stations : `id` stable et unique (1 à 64
  caractères ASCII alphanumériques, tiret ou underscore), `label` lisible,
  `grpc_endpoint` HTTP/HTTPS sans identifiants ni chemin applicatif.
  Aucune connexion gRPC n’est encore ouverte.
- `max_requests` : 1..1024, défaut 128 ; limite des traitements simultanés.
- `request_timeout_seconds` : 1..60, défaut 5 ; délai d’exécution du handler.
- `shutdown_timeout_seconds` : 1..10, défaut 2 ; délai de fermeture gracieuse.

Le listener est ouvert synchroniquement pour rendre un port occupé visible
comme un échec de chargement du plugin. Axum tourne ensuite dans un thread
possédant son propre runtime Tokio. Ce choix permet à `on_unload`, synchrone,
d’attendre la terminaison même si l’acteur des plugins fonctionne sur un
runtime mono-thread. L’attente peut durer jusqu’au délai de fermeture gracieuse ;
les connexions restantes sont ensuite annulées par la destruction du runtime.
Le port est libéré avant le retour de l’arrêt et donc avant un rechargement.
`Drop` assure aussi le nettoyage si l’instance est abandonnée.

Seul `GET /healthz` (et HEAD correspondant) répond sans authentification :
204, sans données de station. Les autres chemins retournent 401 sans corps.
Une méthode non supportée sur la sonde retourne 405. La sonde constate
uniquement que le serveur HTTP répond ; elle ne garantit pas la diffusion.
La limite de traitements n’est pas un quota de connexions TCP ni un délai
de lecture des en-têtes ; le proxy devra aussi borner ces ressources avant
exposition. SSE, WebSocket et PTY ne sont pas encore implémentés.

Exemple désactivé et commenté ajouté à `stationd.example.toml`, avec deux
stations. L’édition du catalogue imbriqué reste dans le TOML ; le formulaire
TUI générique actuel ne sait pas éditer ce type de liste.

L’exigence fonctionnelle multi-stations est consignée dans `Webmin.md` §55 :
**après login, synthèse du réseau**, puis détail de la station sélectionnée.
Le lot B devra rendre les autorisations dépendantes de la station ; le lot C
réalisera cette synthèse, y compris pour un réseau d’une seule station.

Les validations Rust utilisent le conteneur de développement existant sur
`devstationd` (`docker exec -u dev stationd-station-1`, dépôt `/src`), sans
redémarrage ni activation de Webmin sur la station en cours de diffusion.
Validation de la tranche A :

- `cargo test --locked --lib webmin_` : 9 tests réussis.
- `cargo test --locked --lib` : 627 tests réussis, 11 ignorés, aucun échec.
- Module formaté avec `rustfmt --edition 2021` ; diff Webmin vérifié avec
  `git diff --check`.

Les tests couvrent les origines interdites, les IDs dupliqués, les limites,
le refus des routes métier, la saturation, l’expiration des handlers, les
ports occupés, les rechargements mono-thread et la fermeture des connexions
incomplètes. Les tests ignorés sont ceux déjà conditionnés par des fixtures
ou des composants externes du dépôt.


## 9. Compilation via le Makefile

Sur `devstationd`, depuis `/data/dev/stationd` :

```sh
make webmin
make webmin-test
```

`webmin` est un alias de `build` : il compile StationD, stationctl et les
plugins natifs intégrés, avec le profil habituel (debug sur la branche
`webmin`). Il met aussi `target/active` à jour comme `make build`, sans
redémarrer les services. `make webmin PROFILE=release` sélectionne release.
`webmin-test` exécute les tests ciblés du socle dans le conteneur de dev.
Les deux cibles apparaissent dans `make help`.

La cible `plugins` reste réservée aux modules WASM ; Webmin n’a pas de
fichier `.wasm` et ne se compile pas avec `make plugins P=…`.

## 10. Tranche B — identité et sessions

Réalisée : passkeys via `webauthn-rs` 0.5.5, enrôlement à usage unique,
sessions persistantes révocables, droits par station, protection d’origin
et CSRF, limitation des tentatives, stockage privé et audit. Le catalogue
réseau constitue l’accueil après connexion ; ses états en direct relèvent
toujours de la tranche C.

Les nouvelles commandes `stationctl remote-user`, `remote-session` et
`remote-audit` passent par un RPC d’administration de plugin borné et réservé
au gRPC opérateur. Le navigateur n’accède pas à cet endpoint.

Voir [Webmin-auth.md](Webmin-auth.md) pour la configuration (`capabilities =
["db"]`), l’enrôlement du premier administrateur et la procédure de test HTTPS.
Sans cette capacité, le comportement de sonde seule de la tranche A subsiste.
Le plugin reste désactivé dans l’exemple fourni ; aucune configuration active
n’est modifiée automatiquement.

Le nouveau hook natif `Plugin::filters_pool()` (vrai par défaut pour conserver
le comportement des plugins existants) permet à Webmin de sortir du chemin
musical. Ses écritures d’identité ne sont ainsi pas mises en lecture seule
pendant une simulation de programmation.

Validation de la tranche B :

- Suite daemon : `cargo test --locked --lib --quiet`, 637 réussis et 11 ignorés.
- Après les derniers ajustements de rétention et CSRF : 19 tests Webmin
  réussis, dont le parcours HTTP complet jusqu’à l’accueil réseau.
- `cargo build --locked` : stationd et stationctl compilés sans avertissement.
- Aides `stationctl remote-user --help` et `remote-session --help` vérifiées.
- Syntaxe de `remote.js` vérifiée avec `node --check` ; modules nouveaux
  formatés avec rustfmt et diff contrôlé avec `git diff --check`.

L’essai humain avec une passkey réelle reste à faire derrière le proxy HTTPS
retenu, suivant [Webmin-auth.md](Webmin-auth.md). Aucune identité réelle
n’a été créée automatiquement et la station en cours n’a pas été redémarrée.

## 11. Connexion par mot de passe (7 octobre 2026)

À la demande de l'opérateur, les pages d'inscription et de connexion
utilisent un mot de passe, sans appel aux API WebAuthn du navigateur.
Le lien temporaire existant sert à choisir le mot de passe ; la commande
`remote-user enroll` sert aussi à sa réinitialisation.

Le champ `password_hash`, optionnel à la désérialisation, conserve la
compatibilité des bases déjà créées. L'empreinte Argon2id à sel aléatoire
utilise la dépendance RustCrypto existante. Une vérification factice couvre
les comptes inconnus/révoqués. Le mot de passe n'entre ni dans l'audit ni
dans les réponses d'administration. Une réinitialisation réussie invalide
le lien, retire les anciennes passkeys et révoque toutes les sessions.
Le protocole passkey antérieur reste dans le backend pour compatibilité,
mais les pages utilisateur ne le sollicitent plus.

Validation : 22 tests Webmin passent (dont inscription/connexion HTTP,
rejeu, expiration, mauvais mot de passe, limitation, réinitialisation,
persistance, rotation/révocation des sessions et droits par station).
JavaScript vérifié avec Node --check. Compilation et relance de StationD
réussies ; le plugin est chargé sans échec. Le formulaire HTTPS public
présente les champs nom du compte et mot de passe ; /api/session sans
connexion répond 401. Aucun compte ni mot de passe supplémentaire n'a été
créé sur le service public : la création du compte de contrôle temporaire
a été refusée par le contrôle automatique. L'essai humain sera réalisé
avec le compte TestUser et le mot de passe choisi par l'opérateur.

## 12. Longueur des mots de passe configurable

`password_min_length` et `password_max_length` sont définis dans la
configuration du plugin (défauts 12 et 256). La configuration refuse un
minimum nul, un minimum supérieur au maximum et un maximum supérieur à
256. L'inscription et la réinitialisation appliquent les mêmes bornes côté
serveur et dans le formulaire : les valeurs du formulaire sont injectées
par le serveur, sans constante de longueur dans HTML ou JavaScript.
Le décompte porte sur les caractères Unicode, y compris hors BMP.
La connexion continue d'accepter les mots de passe existants si les bornes
changent. La configuration active conserve le minimum déployé de 12 ;
le passage à 6 a été refusé par le contrôle automatique faute d'accord
explicite sur cette valeur. Les tests isolés vérifient un minimum de 6,
un maximum de 8, les limites invalides et un mot de passe avec emoji.

## 13. Purge des utilisateurs

`stationctl remote-user purge NOM` supprime définitivement un compte actif
ou révoqué, ses credentials, droits, lien et sessions. La transaction
conserve une trace d'audit ; les challenges en mémoire sont supprimés après
le commit. Le nom est libéré et peut être recréé avec un nouvel UUID ; aucun
ancien lien, challenge ou cookie ne donne accès à ce nouveau compte.
Le RPC reste exclusivement sur le canal opérateur. Aucun compte réel n'est
purgé automatiquement lors du développement ou de la mise en service.

## 14. Dashboard multi-stations (tranche C)

L'accueil affiche désormais état, média, audience et alertes par station
avec mise à jour SSE. Le détail expose le média et sa progression, le
préchargé, la suite explicitement simulée, les notes, les services et les
prochaines playlists. Les dernières données d'une station inaccessible
restent marquées périmées ; l'audience inconnue reste distincte de zéro.

`live.rs` mutualise un Watch gRPC et un poll de santé par station. Les tâches
appartiennent au runtime dédié du plugin et s'arrêtent avec lui. Les DTO
sont une projection explicite et bornée, sans endpoint, chemin, erreurs
brutes ni configuration. HTTPS gRPC valide les certificats via les racines
natives ; le HTTP distant est refusé au profit de HTTPS ou d'un tunnel.

Les flux SSE revalident la session et les droits chaque seconde, ont une
trame d'attente et un quota global configurable. Une station en panne et
un navigateur lent ne bloquent pas les autres. Aucun nouveau compte n'a
été créé et aucun mot de passe existant n'a été changé pour ces essais.

Validation : 31 tests Webmin passent, plus le smoke test facultatif sur
la station réelle. Les données du DTO concordent avec stationctl onair et
icecast status (média, programme, audience). Aperçu navigateur vérifié sur
ordinateur et à 360 px. La procédure est dans Doc/Webmin-dashboard.md.
