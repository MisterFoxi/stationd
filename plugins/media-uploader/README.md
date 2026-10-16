# Media uploader — point d’entrée unique

Plugin natif Rust avec page HTML/CSS/JavaScript embarquée, compilé dans StationD.
À activer **uniquement sur le StationD qui possède les droits d’écriture et de
tagging** de la bibliothèque NFS. Le plugin refuse de démarrer si
`media.read_only = true`. Les autres stations n’ont pas ce plugin à activer.

## Configuration

```toml
[[plugin]]
name = "media-uploader"
enabled = true

[plugin.config]
bind = "127.0.0.1:8092"
token = "<cle-generee-par-openssl-rand-hex-32>"
inbox = "Uploaded"
tag_field = "Type"
max_file_mb = 256
max_uploads = 2

[plugin.config.conversion]
output_format = "mp3"
ffmpeg = "ffmpeg"
mp3_bitrate_kbps = 320
max_output_mb = 512
timeout_seconds = 900
```

Toutes ces options ont les valeurs par défaut montrées. `bind` attend une IP
et un port. Pour un accès direct sur le réseau privé, utiliser l’IP de cette
machine. `inbox` est un seul nom de dossier sous `media.library_path`.
Le plugin ne prend ni liste de stations ni endpoint distant : les fichiers
sont reçus par ce daemon avec ses propres droits. Pas de capacité WASM requise.

Générer la clé :

```sh
openssl rand -hex 32
```

Copier le résultat directement dans `token`, sous `[plugin.config]` du
`stationd.toml`. La clé doit contenir 32 à 512 caractères ASCII imprimables
sans espace. Elle ne dépend pas d’une variable d’environnement ou d’un `.env`.
Le fichier TOML contient donc un secret et doit rester accessible uniquement
aux administrateurs et au service StationD.

L’interface demande cette même clé et la garde seulement en mémoire pendant
l’ouverture de la page. Chaque upload utilise `Authorization: Bearer …`.
Ne pas placer la clé dans une URL. Les routes d’upload exigent cette clé.
Le listener sert du HTTP ; pour un accès hors du réseau privé, le placer derrière
un reverse proxy HTTPS. Conserver les chemins à la racine de ce listener et
adapter les limites de taille et les délais du proxy au traitement des médias.

Après modification de `token`, utiliser `stationctl plugin reload media-uploader`
pour recharger le plugin. Aucune recréation de conteneur n’est nécessaire.

Ouvrir `http://hôte:8092/`. La page accepte la sélection multiple et le dépôt
par glisser-déposer, affiche la progression du transfert puis la préparation
(conversion si nécessaire, tagging et scan). Les fichiers du lot sont envoyés
successivement. Une erreur d’authentification, une indisponibilité ou une
connexion perdue suspend les transferts restants. Aucun renvoi automatique.

## Formats et conversion

**Entrées : MP3, WAV, AIFF, FLAC, Ogg/Vorbis, Opus, M4A, AAC et WMA.**
**Sortie diffusée : MP3 uniquement.** Les MP3 reçus sont gardés sans réencodage.
Les autres formats passent par FFmpeg dans la zone tampon, avant tout tagging
ou publication. Par défaut : MP3 stéréo 44,1 kHz à 320 kbit/s. Le débit est
configurable parmi les débits MP3 standard de 32 à 320 kbit/s.

Le profil `output_format` est explicite et prévu pour être étendu quand la
diffusion et le tagging supporteront d’autres sorties ; une valeur différente
de `mp3` est refusée dans cette version. FFmpeg est déjà inclus dans les images
Docker de développement et de production. En installation native, fournir
FFmpeg avec l’encodeur `libmp3lame`, ou son chemin via `conversion.ffmpeg`.

Les métadonnées standard prises en charge sont copiées pendant la conversion.
Le core écrit ensuite les tags métier `uploaded` et l’UUID StationD. Un MP3
reçu directement garde ses trames personnalisées, comme lors d’un tagging normal.

La conversion est limitée à 15 minutes et 512 Mio de sortie par défaut. Une
sortie atteignant sa limite est refusée : aucun fichier tronqué n’est publié.
Le nombre de conversions simultanées suit `max_uploads`. FFmpeg reçoit des
arguments fixes, sans shell, un démultiplexeur imposé selon le format et
uniquement le protocole local `file`. Ses diagnostics sont drainés, avec au
plus 16 Kio conservés. La conversion ne tient pas le verrou partagé et ne
bloque pas l’acteur bibliothèque pendant son exécution.

**En cas d’échec de conversion, l’original reste en tampon**, avec `error.txt`
et les diagnostics disponibles dans `conversion.log`. La réponse d’erreur et
l’interface affichent l’identifiant du dossier tampon (`staging_id`). Rien
n’entre dans la bibliothèque. Une annulation pendant la conversion tue FFmpeg
et conserve également l’original reçu. Après conversion et publication
réussies, les fichiers de travail sont supprimés.

## Flux d’import

1. Réception en flux, sans charger le fichier entier en mémoire, dans
   `<library>/.stationd-upload/<uuid>/<nom>.part`. Tous les scanners ignorent
   ce suffixe, y compris sur les stations en lecture seule.
2. Réception limitée à `max_file_mb`, avec au plus `max_uploads` transferts
   et traitements simultanés. Une réception vide ou interrompue est nettoyée.
   Limites : 15 minutes au total et 30 secondes sans données.
3. Conversion éventuelle dans le même tampon, vers `.converted.mp3.part`.
4. L’acteur bibliothèque sérialise l’import avec les scans et éditions de tags.
   Le verrou partagé protège le tagging et la publication. Le contenu MP3
   et sa durée sont vérifiés ; un nouvel UUID StationD est attribué, même
   pour une copie provenant de la bibliothèque.
5. Remplacement de `TXXX:Type` (ou du champ configuré) par la seule valeur
   `uploaded`, puis relecture pour vérifier les écritures.
6. Renommage atomique vers `<library>/Uploaded/<uuid>/<nom>.mp3`.
   Le nom est conservé avec extension MP3 après conversion. Deux fichiers
   homonymes ne s’écrasent jamais. Les autres stations ne voient que le MP3
   complet, taggé et identifié.
7. Scan habituel avec ses plugins et sa politique d’analyse. Une réponse
   distingue le média indexé du fichier enregistré dont le scan a échoué.
   Dans ce dernier cas, corriger puis relancer le scan sans réuploader.

Après un arrêt brutal du processus, des `.part` peuvent rester en tampon ;
ils restent invisibles aux scans et peuvent être retirés par l’opérateur.
Le plugin ne supprime jamais automatiquement les fichiers publiés.

Le classement reste dans la TUI : filtrer sur `uploaded`, sélectionner les
médias puis remplacer ce type par `song`, `jingle` ou autre. L’exclusion de
diffusion existante et les scans NFS ne changent pas. Le champ configuré doit
être une source du plugin `custom-tags` (`tags = ["Type"]` pour le défaut).

## API et validation

`POST /api/upload/:filename` reçoit le fichier brut avec
`Content-Type: application/octet-stream`. Le nom UTF-8 est encodé dans l’URL.
Aucun chemin fourni par le client n’est accepté. Réponse `201` :

```json
{"path":"Uploaded/<uuid>/exemple.mp3","indexed":true,"scan_error":null}
```

Une réponse `201` avec `indexed:false` conserve le MP3 publié : vérifier le
scan et ne pas réuploader. Un échec de conversion rend `422` avec `error`
et `staging_id` ; l’original est conservé. Après perte de connexion ou annulation
après réception complète, vérifier la TUI et le tampon avant de renvoyer.

```sh
make uploader-test
# inclut les tests de conversion réelle FFmpeg :
cargo test --locked --lib upload -- --include-ignored
```

Le plugin suit `stationctl plugin start|stop|restart|reload media-uploader`.
Le rechargement relit le TOML ; des réglages invalides conservent le serveur
actif. Aucun changement n’est requis dans les plugins WASM.