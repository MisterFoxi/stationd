# V1 — sauvegarde centrale et restauration

`docker/icecast/backup.py` sauvegarde les données détenues par les services
centraux. Il ne dénombre pas les stations à partir des conteneurs et ne dépend
pas des sources du banc A/B. Les StationD pourront être déployés ailleurs et
devront sauvegarder leurs données propres indépendamment.

## Préparer et sauvegarder

Sur stationdev1, depuis `/data/dev` :

```sh
make icecast-recovery-image   # auxiliaire autonome à installer une fois
make icecast-backup
```

L'auxiliaire fournit Python/SQLite pour la sauvegarde et FFmpeg pour les sondes
optionnelles de restauration. Aucun conteneur source n'a besoin d'être lancé.
Le script identifie les conteneurs propriétaires de données `icecast`, `admin`
et, s'il existe, `router`. Ces rôles centraux sont indépendants du nombre de
stations. Le TOML sur disque doit correspondre au document chargé par admin.
Le routeur peut être arrêté : son conteneur existant permet de retrouver son
volume et sa sélection. Icecast et admin doivent permettre la lecture du TOML
actif ; ce script n'est pas une procédure de récupération d'un daemon inaccessible.

L'archive de format 2 contient :

- **toute l'arborescence `config/`**, avec les chemins relatifs conservés,
  y compris les configurations de stations présentes dans cette arborescence ;
- **toute l'arborescence privée `.local/icecast/secrets/`**, sans liste de
  secrets ou de stations codée en dur ; les secrets Compose doivent s'y trouver ;
- la base GeoLite déclarée, si activée ;
- **tout le volume administratif** : bases SQLite et autres fichiers d'état,
  avec leurs sous-répertoires conservés ;
- la sélection du routeur lorsqu'il est présent, sans état de lecture ;
- le déploiement des services centraux, leurs identifiants d'images immuables,
  celui de l'auxiliaire, le commit Git, les tailles et sommes SHA-256 des fichiers.

Les conteneurs sources et leurs images ne font pas partie de l'inventaire.
Une station arrêtée reste sauvegardée si ses configurations et données sont
présentes dans les emplacements centraux ci-dessus. Les données situées sur
les machines StationD, les médias NFS et les fichiers hors de ce périmètre
nécessitent leur propre sauvegarde. Le registre de stations de la future
administration V1 n'est pas encore implémenté : aucun registre fictif n'est créé
par cet outil. Les configurations et l'état déjà présents sont conservés.

SQLite est reconnu par son en-tête et copié avec son API de sauvegarde en
ligne, puis vérifié par `PRAGMA integrity_check`. Les fichiers de journal/WAL
associés ne sont pas copiés séparément. Les autres fichiers d'état sont copiés
avec contrôle de stabilité. Configurations, secrets et GeoLite sont également
contrôlés pendant la capture. Aucun service actif n'est arrêté. Les captures
SQLite, configurations et sélection sont successives, pas simultanées.

Les archives sont privées dans `.local/icecast/backups/` : fichier 0600,
répertoire 0700, hors Git. **Elles contiennent les secrets en clair.** Conserver
leur copie hors VM dans un stockage protégé. Aucun transfert externe, calendrier
ou effacement automatique des anciennes sauvegardes n'est créé ici.

Les images Docker ne sont pas incluses : les conserver avec `docker image save`
puis `docker image load` pour une reprise sur un autre hôte. Le manifeste donne
les identifiants nécessaires. Les images absentes ne sont pas remplacées par un
téléchargement implicite. Le fragment Traefik est inclus dans `config/`, mais
pas le fichier global des autres applications ni les certificats de cet hôte.

## Restauration isolée sans émetteurs

```sh
make icecast-restore-test BACKUP=.local/icecast/backups/icecast-....tar.gz
```

Le test restaure uniquement les services centraux dans un réseau et des volumes
uniques, sans publier de ports ni changer Traefik. Il contrôle les SHA-256 des
fichiers restaurés dans les volumes, l'authentification, la sélection conservée
et le chargement des plugins administratifs. Aucune source réelle n'est attendue :
un routeur déconnecté en l'absence d'émetteur n'est pas un échec de restauration.
L'historique est préservé par la copie exacte de la base, sans dépendre d'un nom
de plugin ou d'une table particulière.

Pour vérifier également les connexions audio restaurées :

```sh
make icecast-restore-test BACKUP=CHEMIN_ARCHIVE STREAM_PROBES=1
# ou
python3 docker/icecast/backup.py test CHEMIN_ARCHIVE --stream-probes
```

Le test génère une sonde synthétique par mount source déclaré dans le TOML.
Les sorties alimentées par le routeur ne reçoivent pas de sonde concurrente.
Le nombre de sondes vient de la configuration, avec les secrets propres aux
mounts ; il n'y a ni liste A/B ni nombre de stations imposé. Ces sondes ne sont
pas des StationD et ne restaurent pas les programmes ou médias des stations.
Les profils et émetteurs du déploiement actif ne sont pas réutilisés.

Les conteneurs, volumes et fichiers temporaires du test sont retirés ensuite.
Le banc actif n'est pas modifié. Les anciennes archives de format 1 restent
extractibles et testables : seuls leurs services centraux sont restaurés ; les
anciennes sources enregistrées dans ces archives sont ignorées.

## Extraction et récupération opérateur

```sh
python3 docker/icecast/backup.py extract CHEMIN_ARCHIVE .local/icecast/restored-offline
```

La destination doit être nouvelle. Les doublons, liens, chemins absolus,
traversées et sommes incorrectes sont refusés. La racine et le répertoire de
secrets restent privés ; les secrets sont lisibles par les binds individuels
Compose. Les sommes détectent une corruption, pas une origine malveillante.
Les limites techniques de lecture sont 1 Gio décompressé et 16 384 fichiers ;
un dépassement provoque un refus explicite, jamais une sauvegarde tronquée.
Ces limites concernent la taille des données, pas un nombre de stations.
Les limites du schéma Icecast restent applicables séparément.

L'extraction ne remplace pas le système actif. Pour une reprise réelle, valider
la sauvegarde et les images sur un banc isolé, arrêter les écrivains concernés,
restaurer les configurations/secrets et les volumes avec les propriétaires
attendus (UID/GID 65534 pour admin/router), puis remettre en service et contrôler
les connexions. Ne pas copier une base restaurée sous un daemon encore actif.

## Tests

```sh
make icecast-backup-test
python3 -B tests/icecast-backup-many.py CHEMIN_ARCHIVE --extra-sources 3
```

Les tests unitaires couvrent les archives dangereuses/corrompues, les copies
imbriquées et les plans de sondes pour 0, 1, 3, 25 et 100 sources. Le plan pour
100 sources teste l'absence d'hypothèse A/B ; il ne relève pas les limites du
serveur Icecast. Le test réel étend une copie temporaire d'archive avec trois
sources et configurations de stations supplémentaires, sans toucher au dépôt
ou aux services actifs.

Validation sur stationdev1, le 16 octobre 2026 : les sept tests unitaires passent,
la restauration centrale sans source fonctionne, les archives de format 1 restent
restaurables et l'essai avec trois sources supplémentaires a connecté les cinq
sources résultantes. Les services actifs sont restés en place, sélection B et
collecte historique sans erreur. Les références de secrets des nouvelles archives
sont relatives pour permettre une récupération sur un autre chemin d'hôte.
