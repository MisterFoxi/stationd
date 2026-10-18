# V1 — paquet de déploiement Icecast

Depuis `/data/dev` sur la machine de développement :

```sh
make icecast-dist
make icecast-dist ARGS=--allow-dirty
make icecast-dist VM=utilisateur@vm ARGS=--allow-dirty
```

`icecast-package` et `icecast-dist` suivent le fonctionnement de `package` /
`dist`, avec un script dédié au socle central V1. Les commandes historiques
restent celles de StationD 0.5. La cible `VM` utilise le déployeur SSH existant,
avec transfert du paquet et installation par sudo. Sans `VM`, aucun service
n'est installé ou démarré. Aucun commit ni push n'est effectué.

Le paquet est produit sous `dist/stationd-icecast-<version>-<révision>-<date>.tar`.
Un arbre modifié nécessite `--allow-dirty` et porte la marque `dirty`.
Les images sont compilées en release dans Docker, avec les lockfiles Rust.
Pour emballer les images `:dev` déjà construites :

```sh
make icecast-dist ARGS="--allow-dirty --skip-build"
```

Ce raccourci reprend les images locales telles quelles : reconstruire d'abord
si les sources Rust ou les plugins ont changé.

## Contenu et services

Le paquet contient les trois images versionnées Icecast, administration et
supervision, un Compose sans étape de build, les quatre TOML centraux, la base
GeoLite configurée si activée, les installateurs et les sommes SHA-256.
Le WASM existant reste embarqué dans l'image d'administration et chargé
dynamiquement. Aucune toolchain n'est nécessaire sur la VM cible.

Les seuls services livrés sont `icecastd`, `admin`, `router` et `monitor`.
Les sources synthétiques, smoke tests, StationD et Liquidsoap ne sont pas livrés.
Aucune sauvegarde, image de récupération, archive d'état ou secret existant
n'est inclus. Le Compose livré retire le montage et le contrôle disque des
sauvegardes ; la configuration du banc de développement reste inchangée.

Les mots de passe sources sont dérivés des mounts du TOML installé, sans liste
de stations ni dépendance au banc A/B. Les identités et mounts livrés restent
ceux de la configuration préparée sur le développement : les adapter au site.
La base GeoLite est un fichier nécessaire au déploiement, distinct d'un état
d'exploitation sauvegardé.

## Installation manuelle sur une VM Linux

Prérequis : Docker Engine avec son plugin Compose, Python 3.11 ou supérieur,
Bash et accès au daemon Docker. Docker doit démarrer avec la VM ; les services
emploient `restart: unless-stopped`.

```sh
tar -xf stationd-icecast-<tag>.tar
sudo ./stationd-icecast-<tag>/install.sh --bind IP_LAN_DE_LA_VM
```

Par défaut, l'installation est `/opt/stationd-icecast`, le port audio 18000,
et le bind `127.0.0.1`. Avec la livraison automatique `VM=...`, cette écoute
locale permet de terminer la configuration avant exposition au réseau.
Pour choisir ensuite l'adresse : relancer le même installateur avec `--bind`,
ou modifier `/opt/stationd-icecast/.env`, puis :

```sh
sudo /opt/stationd-icecast/compose up -d --no-build --pull never icecastd
sudo /opt/stationd-icecast/ctl status
sudo /opt/stationd-icecast/ctl monitoring
sudo /opt/stationd-icecast/ctl route
```

L'administration gRPC reste sur le loopback, port 50061 ; les RPC de routeur
et de supervision restent sur le réseau Docker. Traefik est configuré
séparément pour cibler `IP_LAN_DE_LA_VM:18000`. La console egui pourra employer
l'API gRPC existante. Sans source connectée, les alertes de source sont attendues.

L'installateur vérifie les sommes et les identifiants d'images, initialise
seulement les secrets manquants, puis valide le TOML avec le générateur Rust
avant activation. Les configurations, secrets, GeoLite et volumes déjà présents
sont conservés. Il refuse un répertoire non géré ou un projet Docker appartenant
à une autre installation. Une mise à jour d'Icecast peut interrompre les
auditeurs ; choisir le moment du déploiement en conséquence.

Options de l'installateur : `--dir CHEMIN_ABSOLU`, `--project NOM`, `--bind IP`,
`--port PORT`, `--no-start`. Le projet existant ne peut pas être renommé pendant
une mise à jour, afin de conserver les mêmes volumes.

## Vérification sans lancer les services

```sh
make icecast-dist-test
make icecast-dist-install-test BUNDLE=dist/stationd-icecast-<tag>.tar
sudo ./stationd-icecast-<tag>/install.sh --dir /opt/icecast-validation \
  --project icecast-validation --no-start
```

`--no-start` charge les images, prépare les fichiers et exécute uniquement le
générateur de configuration dans un conteneur sans réseau. Il ne lance aucun
daemon audio ou administratif.


Validation le 16 octobre 2026 : huit tests unitaires réussis (périmètre central,
chemins portables, secrets pour 0/1/7/100 mounts, intégrité, échec initial et
retour à la version précédente). La commande normale avec build Docker a produit
un paquet. Son installateur a été validé avec les vraies images en préparation
seule, puis réexécuté après ajout d'un mount local : configurations, secrets et
environnement conservés, nouveau secret provisionné. Aucun service du banc actif
n'a été redémarré et aucune VM distante n'a été déployée pendant cette validation.
