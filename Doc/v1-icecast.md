# V1 — première étape : Icecast commun

Développement sur `v1-dev`, propagation des versions validées sur `V1`.
`main` et `dev` restent consacrées à la version 0.5.

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
