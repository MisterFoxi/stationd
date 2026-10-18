# Administrer Icecast V1

## Évolution retenue : configuration par StationDeck

Le service/DNS Icecast V1 est désormais **icecastd** dans le Compose. `station-admin` reste le service administratif
distinct. StationDeck sera l'autorité d'authoring, avec backend permanent et
récupération gRPC par les démons/adaptateurs. Le backend et l'agent de démonstration
sont livrés ; le premier adaptateur Icecast réel est câblé, recette à faire :
[câblage SD/icecastd](stationdeck-daemon-adapters.md). Ce protocole reste distinct
de l'API d'administration existante sur 50061.
Voir [configuration, découverte et plugins](stationdeck-configuration.md).
La console retenue est une WebUI egui/WASM ; elle complète l'interface web native
Icecast conservée pour le dépannage.

Deux accès sont disponibles sur stationdev1, indépendamment de StationD :

- `stationctl` V1 → daemon Rust `station-admin` en gRPC → API native Icecast ;
- interface web native d'Icecast, conservée pour l'administration et le dépannage.

Le code est dans `crates/stationd-admin` (workspace autonome).
La future console egui pourra utiliser le même contrat gRPC.
Ce socle ne remplace pas encore toutes les commandes stationctl de la 0.5 :
le binaire V1 de cette étape est livré dans l'image `stationd-v1-admin:dev`.
Aucun nouveau frontend HTML n'est développé.

## Démarrage sur stationdev1

Dans `/data/dev` :

```sh
make icecast-init
make icecast-up
make icecast-test
```

`icecast-init` crée désormais aussi `admin-api-token`, sans remplacer les
secrets existants. `icecast-up` lance les deux services `icecastd` et `admin`.
Icecast ne dépend pas du daemon administratif. L'arrêter ne redémarre pas
Icecast et ne coupe pas ses connexions.

## stationctl via gRPC

Sur la VM, le raccourci Make utilise le client V1 dans le service admin :

```sh
make icecast-ctl
make icecast-ctl A='listeners /test-a.mp3'
make icecast-ctl A='metadata /test-a.mp3 "Artiste — Titre"'
make icecast-ctl A='kick-listener /test-a.mp3 42'
make icecast-ctl A='kick-source /test-a.mp3'
```

Équivalent direct :

```sh
docker compose -f docker/icecast/compose.yaml exec -T admin stationctl icecast status
```

Les commandes kick déconnectent immédiatement la cible.
Un client source configuré pour se reconnecter peut revenir ensuite.
Les seules cibles acceptées sont les mounts du TOML central.
Les réponses status et listeners sont en JSON ; une opération refusée ou
non appliquée donne un code de sortie non nul.

Depuis un client Rust externe, utiliser le contrat
`crates/stationd-admin/proto/icecast_admin.proto` :
`GetStatus`, `ListListeners`, `SetMetadata`, `KickListener`, `KickSource`,
`GetRoute`, `SwitchRoute`, `GetConfig`, `CheckConfig`, `GetGeography`,
`GetHistoryStatus`, `QueryHistory`.
Le serveur exige la metadata `Authorization: Bearer <admin-api-token>`.
Le client lit le jeton dans un fichier, jamais dans un argument de mot de passe.

Le port hôte gRPC est `127.0.0.1:50061` (variable `STATION_ADMIN_PORT`).
Pour un client installé sur l'hôte :

```sh
stationctl --addr http://127.0.0.1:50061 \
  --token-file .local/icecast/secrets/admin-api-token icecast status
```

Le trafic HTTP vers Icecast reste sur le réseau privé Docker ; le mot
de passe Icecast est détenu par le serveur d'administration, pas par le
client gRPC. Le service admin tourne comme `nobody`, sans socket Docker,
sans accès aux médias et sans capacités.

## Interface native Icecast

Pour les tests, Icecast est publié sur l'adresse LAN de stationdev1 :
ouvrir `http://192.168.1.118:18000/admin/`.
Le nom `http://stationdev1.lan:18000/admin/` fonctionne aussi si sa résolution
pointe vers cette adresse.

Le bind hôte est configurable : `ICECAST_BIND` (défaut `192.168.1.118`,
adresse LAN de cette VM de test). `ICECAST_PORT` reste à 18000 par défaut.
Si l'adresse LAN change, fournir la nouvelle adresse dans ICECAST_BIND.
Pour revenir au loopback :

```sh
ICECAST_BIND=127.0.0.1 docker compose -f docker/icecast/compose.yaml up -d --wait icecastd
```

Le gRPC administratif reste publié sur le loopback de la VM.

Utilisateur : `admin` (champ `admin_user` du TOML central).
Le mot de passe est dans `/data/dev/.local/icecast/secrets/admin-password`
sur la VM. Le consulter localement dans un terminal autorisé :

```sh
cat .local/icecast/secrets/admin-password
```

Ne pas publier ce mot de passe dans Git ou dans une URL.
Les accès anonymes à l'administration sont refusés.
L'interface native affiche les sources, l'audience, les métadonnées
et les actions administratives fournies par Icecast.

## Configuration et disponibilité

La configuration persistante reste dans `config/icecast/icecast.toml`.
Après modification :

```sh
make icecast-reconfigure
make icecast-test
```

Cette opération recrée Icecast et l'administration et coupe les connexions
existantes. Le générateur valide le TOML et les secrets avant de lancer
Icecast. Le daemon charge la même liste de mounts au démarrage.

Pour arrêter/reprendre uniquement l'administration :

```sh
docker compose -f docker/icecast/compose.yaml stop admin
docker compose -f docker/icecast/compose.yaml up -d --wait admin
```

L'authentification du socle est un jeton d'opérateur local unique.
Les comptes, rôles, audit central, plugins administratifs, console egui
et bascules programmées restent les étapes suivantes de la V1.
Les fonctions de déplacement d'auditeurs de l'interface native Icecast
ne constituent pas encore le mécanisme de bascule StationD défini pour V1.


## Validation du 16 octobre 2026

- Deux tests Rust du socle administratif, formatage et Clippy réussis.
- Tests réels de l'interface native : connexion admin, métadonnées,
  liste et déconnexion d'un auditeur, déconnexion d'une source, avec
  continuité de l'autre source.
- Tests réels du client stationctl en gRPC : état, métadonnées, auditeurs,
  déconnexions, rejet d'un jeton invalide et d'un mount non déclaré.
- Test de continuité : le même auditeur reçoit encore du MP3 pendant
  l'arrêt du daemon administratif et après sa reprise, sans redémarrage
  d'Icecast. Rejouer ce contrôle sur le banc sans autre source active :

```sh
make icecast-continuity-test
```

Les sources synthétiques et conteneurs de test sont retirés ensuite.
Les deux services Icecast et administration restent disponibles.

## Tests interactifs « en live »

Deux sources synthétiques continues sont disponibles, sans StationD et
sans lecture de médias NFS. Depuis /data/dev :

```sh
make icecast-live-up
```

Ouvrir dans un navigateur ou VLC :

- A, sinusoïde douce 440 Hz : `http://192.168.1.118:18000/test-a.mp3`
- B, sinusoïde douce 880 Hz : `http://192.168.1.118:18000/test-b.mp3`
- Administration native : `http://192.168.1.118:18000/admin/`

La page d'administration permet d'observer les sources et les auditeurs.
Pour tester le gRPC depuis un second terminal :

```sh
make icecast-ctl
make icecast-ctl A='listeners /test-a.mp3'
make icecast-ctl A='metadata /test-a.mp3 "Essai en direct"'
```

Tester une déconnexion d'auditeur (identifiant obtenu via listeners) :

```sh
make icecast-ctl A='kick-listener /test-a.mp3 42'
```

Pour couper seulement la source A :

```sh
make icecast-ctl A='kick-source /test-a.mp3'
```

B doit continuer. Un lecteur configuré pour se reconnecter peut revenir
après un kick d'auditeur. Les sources de test n'ont pas de redémarrage
automatique : après un kick de source, `make icecast-live-up` les relance.

Terminer les sources continues sans arrêter Icecast ni l'administration :

```sh
make icecast-live-down
```

Arrêter ces sources avant `make icecast-test` ou
`make icecast-continuity-test`, qui utilisent les mêmes mounts.


## Point d'écoute stable et bascule A/B

Le banc de bascule immédiate est décrit dans [Bascule A/B](v1-icecast-switch.md).
Lancer make icecast-switch-up puis utiliser stationctl icecast switch a ou b.
Arrêter ce banc avec make icecast-switch-down avant les anciens tests automatiques.

## Configuration et géographie

Voir [Configuration centrale, Traefik et GeoLite](v1-icecast-config.md).
Les commandes config show/revision/check et geography utilisent le même gRPC authentifié.

L'historique et la route HTTPS sont décrits dans [Historique et Traefik](v1-icecast-history.md).
