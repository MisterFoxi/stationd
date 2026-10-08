# Identité durable des médias

Chaque média possède un UUID stable, indépendant de son chemin. La migration
0033 introduit `media_identity(uuid, uri)` : `uuid` est l'identité, `uri`
est actuellement le chemin relatif à la racine média, avec sa casse conservée.

## Registre et tags

Le scan recherche le tag réservé `STATIOND_UUID`. Un UUID valide présent dans
le fichier est retrouvé dans le registre ; si l'ancien chemin n'existe plus,
sa localisation est actualisée. Le déplacement effectué par StationD actualise
également cette correspondance, dans la transaction de réorganisation.

Sans tag, un chemin déjà connu reprend son UUID. Un nouveau média reçoit un
UUID v4. L'UUID est écrit dans le fichier avant l'enregistrement de sa nouvelle
correspondance ; l'écriture est relue et vérifiée. Après une interruption entre
le fichier et SQLite, le scan suivant retrouve l'identité dans le tag.

Écriture native : ID3v2 TXXX pour MP3/WAV/AIFF, commentaires Vorbis pour
FLAC/Ogg/Opus/Speex, atom MP4 libre `----:com.stationd:STATIOND_UUID` pour
M4A/M4B, item APE pour APE/WavPack/Musepack. Les tags existants sont conservés.
Pour les formats sans conteneur pris en charge (notamment AAC ADTS), l'identité
reste en SQLite et un avertissement signale l'absence de tag portable.

Les écritures utilisent un fichier temporaire voisin, vérifient la relecture,
conservent mtime et permissions, puis remplacent le fichier. Sous Linux, le
propriétaire, le groupe et les attributs étendus, dont les ACL exposées par
le système de fichiers, sont également conservés. Les changements
taille/mtime pendant l'opération font échouer l'écriture. Les liens symboliques
ne sont pas suivis pour écrire. Une erreur d'écriture est remontée, jamais
interprétée comme un tag enregistré. Les fichiers précédemment écrits ne sont
pas annulés si un fichier suivant échoue : filesystem et SQLite ne partagent
pas de transaction.

Une copie conserve aussi le tag de l'original : si deux fichiers distincts
portent le même UUID, le scan refuse la fusion et signale le conflit avant
d'écrire les UUID de ce scan. Un tag invalide, nul, multiple ou contradictoire
avec une identité confirmée du registre est également refusé. Aucun UUID
valide n'est remplacé
automatiquement pour résoudre un conflit. La résolution explicite des copies
n'est pas exposée comme commande dans ce patch.

L'UUID est réservé au cœur. Les plugins ne le dérivent pas, et l'éditeur de
tags refuse sa modification ou sa suppression. Il n'apparaît pas parmi les
champs utilisateur éditables.

## États internes et compatibilité

L'index média, les genres, métadonnées, tags, analyses, logs de diffusion,
épisodes joués, queues et curseurs possèdent une référence `media_uuid`.
Les lectures d'états durables et les jointures de sélection utilisent cette
identité. Les candidats et les lignes de bibliothèque exposent aussi
`media_uuid` en Rust. Les anciennes API/protos et les déclarations de fichiers
des playlists conservent leurs chemins ; ces chemins restent des localisations,
et une déclaration static par ancien chemin doit être actualisée si ce chemin
change.

Les colonnes et FK historiques par chemin des vues restent pour compatibilité ;
les UUID sont ajoutés avec des index uniques et des triggers de reprise pour
les anciens imports SQL. Un déplacement actualise les caches de chemins tout
en conservant les UUID. Une purge de l'index ne purge pas le registre.

`broadcast_log.rel_path` reste le chemin au moment de la sélection, pour
préserver la trace historique. Les contraintes de piste, descriptions et
statistiques retrouvent le média via son UUID ; les statistiques par média
regroupent les anciens et nouveaux chemins et affichent la localisation
courante. Les épisodes gardent leur garde-fou taille/mtime, qui suit les
écritures de tags réalisées par StationD.

La migration attribue une identité commune aux références existantes d'un
même chemin, y compris les références historiques sans ligne d'index. Elle ne
devine pas quels anciens chemins différents correspondaient au même fichier.

## Simulation et activation

La copie SQLite de simulation copie le registre en premier, puis les vues et
états dépendants. Les anciens plugins WASM qui ne renvoient pas l'UUID restent
compatibles : le cœur restitue l'identité du candidat d'origine.

Les tags sont écrits au prochain scan complet du MO (`read_only = false`)
avec le binaire intégrant ce patch, ou après une édition effectuée sur le MO. La migration
SQLite seule ne modifie aucun fichier audio. Ce patch ne change pas encore
l'algorithme shuffle.

Les tests couvrent migration, tag portable, idempotence, rescan, purge,
reconstruction de base, déplacement externe, conservation des états et du
chemin historique, statistiques par UUID, conflit de copie et tag réservé.
Des fixtures de silence synthétique vérifient les conteneurs MP3, WAV, FLAC,
Ogg, Opus, M4A et AIFF ; APE/Musepack/Speex nécessitent encore leurs propres
fixtures de validation.


## Bibliothèque partagée : MO maître et stations secondaires

Le MO est l'unique instance qui attribue les UUID, enrichit les fichiers,
écrit les tags et réorganise la bibliothèque. Les autres instances scannent
les fichiers pour reconstruire leur propre SQLite. Configurer les rôles
explicitement (le mode historique écrivain reste le défaut) :

```toml
# MO
[media]
library_path = "/mnt/nfs/radio"
read_only = false
```

```toml
# Stations secondaires ; le montage peut lui-même être en lecture seule.
[media]
library_path = "/mnt/nfs/radio"
read_only = true
```

En mode `read_only`, aucun tag, UUID ou fichier de verrou n'est créé dans le
partage. Les plugins peuvent interpréter les tags pour reconstruire les
catégories de l'index local, mais leurs résultats ne sont pas réécrits dans les
médias. L'analyse audio est désactivée, même si `[analysis].enabled` est vrai.
Les éditions, renommages, réorganisations effectives et demandes de réanalyse
sont refusés avec un message demandant de les effectuer sur le maître. Les
aperçus de réorganisation et les lectures restent disponibles.

Un fichier sans tag UUID valide fait échouer le scan secondaire avec un
message explicite : terminer son scan sur le MO avant de scanner les stations.
Une station secondaire ne fabrique pas d'UUID local pour compenser un tag
absent. Cette exigence concerne aussi les formats sans tag portable : ils ne
peuvent pas être intégrés à ce modèle partagé sans prise en charge de leur
conteneur ou une autre source commune d'identité.

La migration 0034 ajoute `confirmed` au registre. Les UUID générés par les
migrations ou les anciennes API de chemins sont provisoires (`confirmed=0`).
Lors du premier scan, le tag du fichier fait autorité : si une autre station
a déjà écrit son UUID, toutes les références provisoires locales (index,
historique, épisodes, queues, curseurs) sont réattachées à cet UUID dans une
transaction. Le chemin historique du log reste intact. L'identité devient
confirmée après relecture du tag (`confirmed=1`) et un conflit ultérieur est
refusé. Les formats sans tag portable restent provisoires et ne garantissent
pas une identité commune à plusieurs bases indépendantes.

Les stations gardent chacune leur SQLite et leur historique sur un stockage
local. Seuls les fichiers médias et leurs tags sont partagés. Les racines
média peuvent avoir des chemins de montage différents ; elles doivent désigner
la même racine du partage pour utiliser le même verrou.

Le MO utilise la création exclusive du répertoire
`.stationd-library.lock` sur le partage, sans dépendre des verrous POSIX locaux.
Le scan acquiert le verrou avant de lire les fichiers et le conserve jusqu'à
la fin des écritures et de la mise à jour de l'index. Les éditions de tags et
la réorganisation utilisent le même verrou. Une lecture préparée avant
l'acquisition est rafraîchie avant l'attribution d'identité. Les tâches
bloquantes d'écriture conservent aussi le verrou si leur future est annulée.
L'attente est limitée à 30 secondes ; une station occupée fait échouer le
nouveau scan, qui peut être relancé ensuite. Les outils externes doivent être
arrêtés pendant les écritures ou respecter ce même protocole.

Le verrou contient un fichier `owner` (hôte, PID, jeton). Après un crash, il
peut rester présent : il n'est jamais volé sur un simple délai. Vérifier que
tous les écrivains sont arrêtés avant de supprimer le fichier `owner` puis
le répertoire de verrou. Aucune suppression automatique récursive du partage.

Pour les écritures du MO avec remplacement atomique, utiliser des UID/GID
cohérents entre le MO et le serveur NFS, et des droits permettant la création
dans
la racine et le répertoire du média. Le groupe seul ne permet pas à un
processus non privilégié de conserver le propriétaire d'un fichier appartenant
à un autre UID. Si le serveur refuse de restaurer propriétaire/groupe/ACL
(par exemple avec root-squash), StationD refuse le remplacement et conserve
l'original ; il ne dégrade pas silencieusement les droits. Les stations
secondaires ont uniquement besoin de lire les médias ; leurs UID peuvent
différer de celui du MO si les groupes/ACL autorisent cette lecture.

Validation : tests de deux bases concurrentes avec UUID provisoires distincts,
reprise des références, copie de simulation, exclusion entre processus,
absence de récupération automatique d'un verrou abandonné, propriétaire,
groupe, mode, attribut utilisateur et ACL POSIX. Le test opt-in
`master_and_replicas_on_nfs` crée un répertoire temporaire sous
`STATIOND_TEST_SHARED_ROOT`, écrit les UUID comme le MO puis vérifie les scans
secondaires, la reprise des références et la reconstruction de base sans
écriture audio ni analyse ; les bases restent sur le stockage local.
Un second test `shared_stations_on_nfs` couvre les écrivains concurrents et
l'exclusion entre processus, par précaution contre une mauvaise configuration.
Ces tests ne scannent pas les médias existants du partage. Exécution :

```sh
STATIOND_TEST_SHARED_ROOT=/mnt/nfs/radio \
  cargo test --locked --lib master_and_replicas_on_nfs -- --ignored --nocapture
```

## Empreintes de fichiers et cache NFS

Le scan relève taille/mtime sur le descripteur du fichier ouvert et lu, après
la revalidation NFS à l'ouverture. Il ne conserve pas un `stat` de chemin
potentiellement périmé effectué avant cette ouverture. Après l'écriture UUID,
l'empreinte est également reprise sur le fichier rouvert pour vérifier son tag.
Les contrôles de write-back ouvrent le fichier avant de relever son empreinte.
Un changement réel de taille ou de mtime reste refusé ; le diagnostic affiche
les valeurs attendues et observées.

Le test opt-in `master_full_scan_on_nfs` couvre l'enchaînement écriture UUID,
expiration du cache (pause de cinq secondes), write-back des métadonnées, scan
complet et rescan idempotent. `STATIOND_TEST_SOURCE_MEDIA` permet d'utiliser une
copie isolée d'un média existant ; l'original est uniquement lu. Sans cette
variable, la fixture synthétique MP3 est utilisée.
