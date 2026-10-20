# V1 — première étape : Icecast commun

## Décision actuelle : icecastd et StationDeck

Le service/container Icecast **V1** est désormais nommé **icecastd** dans
le Compose et le DNS interne pour le distinguer de l'Icecast 0.5. Les commandes
`make icecast-*`, les chemins, tags d'images et l'identité du projet/volumes restent
inchangés. La procédure de migration ci-dessous concerne uniquement le banc V1.
StationDeck assure l'authoring ;
son backend permanent distribue la configuration à son agent icecastd par gRPC.
Cet agent fait partie du démarrage normal (`make icecast-up`), sans overlay.
Démarrer StationDeck une première fois pour initialiser le réseau et le jeton.
Sans publication ni cache validé, Icecast attend ; aucun fallback sur le TOML local.
Guide : [démarrage et cache](stationdeck-daemon-adapters.md).
Voir [configuration et plugins](stationdeck-configuration.md).
La console retenue est maintenant une **WebUI egui/WASM**, servie en container,
et non une console native : cette décision remplace la formulation initiale ci-dessous.

Développement sur `v1-dev`, propagation des versions validées sur `V1`.
`main` et `dev` restent consacrées à la version 0.5.

## Migration du service V1 vers icecastd

Sur un nouveau banc, les commandes habituelles lancent directement `icecastd`.
Sur un banc déjà démarré avec le service V1 `icecast`, arrêter d'abord ce service
avec l'ancien Compose **avant de mettre à jour les fichiers**, puis lancer le
nouveau Compose. Ne pas supprimer les volumes. Les connexions audio sont coupées
pendant cette migration ; sources et auditeurs doivent se reconnecter.

```sh
# Avant la mise à jour, dans le checkout V1 uniquement :
docker compose -f docker/icecast/compose.yaml stop icecast
# Après la mise à jour :
make icecast-up
# Si les sources synthétiques et le routeur étaient actifs :
make icecast-switch-up
```

Si les fichiers sont déjà à jour, identifier l'ancien container uniquement par
les labels `com.docker.compose.project=stationd-v1-icecast` et
`com.docker.compose.service=icecast`, puis arrêter ce container avant le lancement.
Ne pas toucher au service `icecast` du Compose racine de la référence 0.5.
Le container V1 obtenu se nomme `stationd-v1-icecast-icecastd-1`.
L'ancien container arrêté peut ensuite être retiré sans supprimer de volume.

Pour un paquet déjà installé, arrêter `icecast` avec le wrapper `compose` de
l'installation avant la mise à niveau. L'installateur refuse un ancien service
encore actif. Dans le `router.toml` conservé de cette installation, remplacer
uniquement l'ancienne URL interne `http://icecast:8000` par
`http://icecastd:8000` ; conserver les autres paramètres locaux. Reprendre les
services avec le nouveau paquet. Les archives de sauvegarde précédentes restent
restaurables dans leur environnement isolé avec leur nom DNS d'origine.

## Cap retenu

V1 en Rust, services sous forme de daemons, échanges internes en gRPC.
Le daemon administratif central gérera programmes, playlists, médias,
configurations, supervision, contrôle des StationD et bascules.
Console native egui, sans HTML ; suppression future de la TUI et
enrichissement de stationctl. L'architecture à plugins avec chargement
dynamique existante doit être conservée et étendue à l'administration et
à la console.

Chaque StationD exécutera ses programmes et pilotera Liquidsoap, avec
continuité de diffusion quand l'administration centrale est indisponible.
Icecast reste commun et les médias restent sur le NFS existant.
Configurations et TOML dans Git ; secrets et états d'exécution hors Git.

Une bascule changera la station alimentant un point d'écoute stable, sans
transfert d'état de lecture : immédiate ou ad hoc / best effort à une
frontière favorable (fin de morceau ou de bloc publicitaire).
Les liens externes déjà traités comme playlists restent un autre mécanisme.

## Périmètre livré ici

Un service Docker Icecast 2.5 indépendant de StationD et de Liquidsoap.
Sa configuration centrale est `config/icecast/icecast.toml`, traduite en XML
par le petit outil Rust autonome `tools/icecast-config`.
Ce générateur ne dépend pas du workspace StationD. Le service Icecast
fonctionne indépendamment du daemon administratif.
Le premier socle du daemon administratif et son client gRPC sont décrits
dans [Administration Icecast](v1-icecast-admin.md). La console egui et les
bascules ad hoc viendront ensuite. Les points `/test-a.mp3` et `/test-b.mp3` servent
au banc de validation ; /radio.mp3 est le point stable du [banc de bascule immédiate](v1-icecast-switch.md).

La dockerisation historique reste disponible via le Compose racine.
Pour cette étape, utiliser exclusivement `docker/icecast/compose.yaml`
ou les cibles `make icecast-*`. Un `make up` historique lance StationD.

## Sur stationdev1

Le partage `V:` correspond à `/data/dev` sur la VM.
Commandes SSH via l'alias `stationdev1.lan`, utilisateur `foxi`.

Depuis `/data/dev` :

```sh
make icecast-init
make icecast-up
make icecast-test
make icecast-logs
make icecast-down
```

Équivalents sans Make :

```sh
python3 docker/icecast/init-secrets.py
docker compose -f docker/icecast/compose.yaml up -d --build --wait admin
docker compose -f docker/icecast/compose.yaml --profile test run --build --rm smoke
docker compose -f docker/icecast/compose.yaml down
```

Pour les tests, l'interface est publiée sur l'adresse LAN de la VM :
`http://192.168.1.118:18000/status.xsl`, administration sur
`http://192.168.1.118:18000/admin/`.

`ICECAST_BIND` choisit l'adresse hôte (défaut `192.168.1.118`, VM de test).
`ICECAST_PORT` choisit le port hôte (défaut 18000).
Le gRPC administratif reste sur le loopback, port 50061.
Si la VM change d'adresse LAN, adapter ICECAST_BIND.

Le port interne doit rester 8000 pour ce Compose (publication et healthcheck).

## Configuration et secrets

Le TOML contient l'identité du serveur, les limites et les mounts autorisés.
Chaque mount référence son propre fichier de mot de passe ; aucun mot de
passe source global ne permet de créer des mounts arbitraires.

`icecast-init` crée des secrets aléatoires dans `.local/icecast/secrets`,
sans écraser les secrets existants. Le répertoire est privé (0700).
Compose monte chaque fichier séparément dans `/run/secrets` pour que
l'utilisateur non privilégié du conteneur puisse le lire. Les fichiers
sont 0644 dans ce répertoire privé : ne pas rendre ses parents accessibles.
Le partage SMB doit rester limité aux développeurs autorisés.

Le TOML est monté en lecture seule. Au démarrage, le générateur valide le
schéma et les références, puis écrit atomiquement un XML privé (0600)
dans le tmpfs `/tmp`. Aucun secret n'est affiché. Le XML ne persiste pas
dans l'image ni dans Git. Le contexte Docker exclut `.local`, `.env`,
les médias et les états locaux. Le conteneur tourne comme `icecast2`,
sans capacités et avec une racine en lecture seule.

La procédure de validation, d'application avec retour arrière et de suivi géographique
est décrite dans [Configuration et GeoLite](v1-icecast-config.md).

## Validation

Tests Rust : XML bien formé et échappement, restrictions de mounts,
secrets manquants, références hors répertoire, configuration invalide.

```sh
cargo test --locked --manifest-path tools/icecast-config/Cargo.toml
```

Le banc Docker lance deux sources FFmpeg (sinusoïdes 440 et 880 Hz,
MP3 mono 64 kbit/s) et deux auditeurs Python à durée bornée. Il vérifie :

- refus d'un mauvais mot de passe et d'un mount non déclaré ;
- authentification de l'API administrative ;
- présence simultanée des deux sources et des deux auditeurs ;
- réception et décodage effectif du MP3 ;
- remise à zéro des auditeurs et retrait des sources après le test.

Il ne lit aucun média, ne monte pas le NFS et ne démarre ni StationD ni
Liquidsoap. Icecast reste lancé après le test ; `make icecast-down`
arrête uniquement ce projet Compose.


## Résultat vérifié le 16 octobre 2026

Sur stationdev1, branche v1-dev : les tests Rust, fmt et Clippy
passent. L'image Docker construit le générateur Rust avec le lockfile.
Icecast 2.5.0 est healthy sur le bind LAN de la VM, port 18000.
Le banc complet a validé deux sources simultanées et deux auditeurs,
16 384 octets MP3 reçus et décodés par auditeur, les compteurs d'audience,
le refus des accès invalides et le nettoyage des connexions.
StationD et Liquidsoap sont absents de l'image de ce service.

## Sauvegarde et restauration

La procédure et les commandes de contrôle sont décrites dans
[Sauvegarde et restauration Icecast V1](v1-icecast-backup.md).

Supervision centrale indépendante : voir [v1-icecast-monitor.md](v1-icecast-monitor.md).

Déploiement sur VM : [paquet Icecast V1](v1-icecast-deploy.md), via `make icecast-dist VM=cible`.

## Reroutage de flux depuis StationDeck

La fenêtre Icecast expose un seul onglet **Routing** : cartes origine/destination,
liaison active, confirmation et **Restore origin**. Une règle reste active sans
auditeur et prend en charge les arrivées suivantes. Le journal est intégré au
même écran. Les anciens groupes restent dans l'historique ; le choix de la source
programme est replié sous « Program feed & stream tools ».

Dans le déploiement géré, l'URL publique conserve son port 18000. L'entrée TCP
écoute sur 8001 dans le container et relaie vers Icecast privé sur 8000. Une règle
réécrit seulement le mount des nouveaux GET de son origine : la même URL continue
de servir la cible quand la VM source est arrêtée. SOURCE/PUT et leurs corps sont
transmis sans réécriture. Aucun redémarrage ou rechargement Icecast lors d'une
bascule. Les connexions existantes sont déplacées individuellement par son API.
Un accès direct au port interne 8000 contourne l'entrée : utiliser les URL
publiées pour bénéficier de la continuité pendant une panne de source.

L'entrée attribue aux connexions reroutées un marqueur aléatoire propre à la règle,
distinct de tout credential utilisateur. L'administration retrouve leurs IDs
Icecast pour le retour, sans rapprochement par IP ou user-agent humain. Les
marqueurs restent côté serveur. X-Forwarded-For conserve les IP d'écoute avec
confiance explicite du relais local ; un pair non approuvé ne peut imposer sa
chaîne XFF. La politique des proxies déjà approuvés est conservée.

Les règles passent par un socket Unix partagé entre superviseur et administration,
sans nouveau port de commande. Le token Icecast control reste distinct de celui
de supervision. Le journal SQLite privé `listener-migrations.sqlite` est dans le
data-dir de station-admin (`/var/lib/station-admin/plugins` du volume admin-state).
Il conserve IDs, marqueur, endpoints et états, sans IP ni credential d'auditeur.
Fermer le navigateur n'arrête pas la route ; redémarrer station-admin conserve
la règle dans l'entrée et le journal. Un redémarrage d'Icecast invalide les anciens
IDs et remet les règles de l'entrée à zéro : préparer alors une nouvelle route.
Le mécanisme reste limité aux mounts d'un même serveur Icecast.

Le retour désactive d'abord la règle, laisse finir les connexions déjà engagées
vers l'amont, puis rappelle les IDs suivis encore connectés. La source d'origine
doit être revenue ; les types MIME doivent être compatibles. Les déconnexions
sont signalées, sans les assimiler à des personnes reconnectées avec un autre ID.
Une interruption ou réponse perdue exige une reprise explicite, sans répétition
automatique d'action. Limites : 2 048 connexions par opération, 20 opérations
ouvertes, 1 000 enregistrées et 100 affichées avec priorité aux ouvertes. L'entrée
borne les connexions simultanées, en-têtes et buffers de transport.
