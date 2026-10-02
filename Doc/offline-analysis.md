# Analyse média offline (Essentia)

Analyse audio hors-ligne pendant les scans : tempo, tonalité, loudness et
labels sémantiques (danceability, genre, mood). Remplace l'estimateur BPM
maison (`Doc/offline-bpm.md`, module `bpm_analysis`), jugé insuffisant.

## Principe

- **Cœur, pas plugin.** L'analyse est pilotée par `stationd` (pas par un plugin
  WASM) : c'est du DSP/ML lourd, hors du périmètre « ajout de fonctionnalités »
  des plugins. Le module est `src/media_analysis.rs`.
- **Hors-process.** `stationd` lance un extracteur en sous-process (borné +
  timeout + kill), jamais via un shell. Un crash de l'extracteur ne tue pas
  `stationd` (invariant d'isolation conservé). Aucun I/O réseau, aucun playback.
- **Les tags sont la source de vérité.** Les descripteurs sont écrits dans les
  tags du fichier (via `scan_writeback`, copie staged + vérif + remplacement
  atomique + mtime préservé). La table SQLite `media_analysis` (migration 0030)
  n'est qu'un **cache reconstructible** : un scan la repeuple depuis les tags,
  sans relancer l'analyse. Crash / VM neuve = un scan de tags suffit.
- **Idempotence.** Un marqueur `TXXX:STATIOND_ANALYSIS = <version d'analyseur>`
  est posé après une écriture vérifiée. Seuls les fichiers sans marqueur à jour
  sont (ré)analysés ; bumper `ANALYSIS_VERSION` déclenche une ré-analyse ciblée.

## Contrat de descripteurs (figé)

Ce qui est tagué ⇔ colonnes de `media_analysis` ⇔ ce que les playlists filtrent.
Changer la liste impose une ré-analyse de toute la bibliothèque.

| Champ | Colonne | Origine |
|---|---|---|
| bpm | `bpm REAL` (TBPM entier arrondi) | signal (RhythmExtractor2013) |
| key / scale | `key`, `scale` TEXT | signal (KeyExtractor) |
| loudness_lufs | `loudness_lufs REAL` | signal (EBU R128 intégré) |
| replaygain_db | `replaygain_db REAL` | signal (ReplayGain) |
| danceability | `danceability REAL` 0..1 | TensorFlow |
| genre_top / genre_prob | `genre_top` TEXT, `genre_prob REAL` | TensorFlow (genre_discogs400) |
| mood / mood_prob | `mood` TEXT, `mood_prob REAL` | TensorFlow (têtes binaires d'humeur) |
| provenance | `analyzer_version` TEXT, `analyzed_at INTEGER` | nous |

Les champs signal sont des faits fiables ; les champs TF sont des estimations à
pondérer par leur `*_prob`. `genre_prob` est structurellement bas (400 classes).

## Extracteur (`tools/essentia_analyze.py`)

Script Python (essentia-tensorflow) qui émet le contrat JSON sur stdout :
`{"bpm":…,"key":…,"scale":…,"loudness_lufs":…,"replaygain_db":…,
"danceability":…,"genre_top":…,"genre_prob":…,"mood":…,"mood_prob":…}`.

- Signal : `MonoLoader` + `RhythmExtractor2013` / `KeyExtractor` / `ReplayGain`,
  `AudioLoader` + `LoudnessEBUR128`.
- TensorFlow (famille **Discogs-EffNet**) : embeddings partagés
  (`TensorflowPredictEffnetDiscogs`, sortie `PartitionedCall:1`), puis têtes
  `TensorflowPredict2D`. Les noms de nœuds d'entrée/sortie sont lus dans le
  `schema` du `.json` de chaque modèle (robuste aux exports figés vs SavedModel).
- Mood : pas de tête mood/theme unique pour effnet → on combine les têtes
  binaires `mood_{happy,aggressive,party,relaxed}` et on garde la plus marquée.
  Note calibration : ces têtes sont indépendantes, le `max` peut favoriser une
  humeur ; à seuiller par tête si un titre franchement énergique sort `relaxed`.

### Modèles

Model zoo Essentia (https://essentia.upf.edu/models.html), déposés à plat dans
`models/` via `models/fetch.sh` (génère un `SHA256SUMS` à épingler). **Licence :
beaucoup de modèles MTG sont en CC BY-NC-SA (non commercial)** — à valider selon
l'usage de la radio.

## Configuration (`[analysis]`)

```toml
[analysis]
enabled = true            # off par défaut : aucune analyse, cache reconstruit depuis les tags
extractor = "tools/essentia_analyze.py"  # vide = STATIOND_ESSENTIA, sinon défaut
profile = ""              # profil extracteur optionnel
timeout_secs = 90         # délai dur par fichier
jobs = 1                  # réservé (analyse séquentielle)
max_per_scan = 0          # 0 = illimité ; N = au plus N fichiers non marqués par scan
```

`max_per_scan` borne l'analyse (chère) sans borner la réconciliation : utile
pour tester sur quelques fichiers, ou grignoter une grosse médiathèque scan
après scan (le marqueur évite de refaire les précédents).

## CLI / gRPC

Le RPC `Scan` porte un drapeau `reanalyze` (contrat `library_v1.proto`) :

```sh
stationctl library scan              # analyse les non-marqués (si enabled)
stationctl library scan --reanalyze  # force la ré-analyse de tous les fichiers
```

## Docker

Images `prod`/`dev` : `python3` + `pip install essentia-tensorflow`
(ARG `ESSENTIA_TF_VERSION`). Prod : l'extracteur part dans `bin/` et les
modèles sont bakés via `models/` (staging par `docker/package.sh`,
`COPY models/ → /usr/share/stationd/models/`). Dev : script et modèles via le
dépôt monté (`/src/tools`, `/src/models`) ; rendre le script exécutable
(`chmod +x tools/essentia_analyze.py`). Variables : `STATIOND_ESSENTIA`,
`STATIOND_ESSENTIA_MODELS`.

## Vérification

```sh
cargo check --workspace
cargo test --lib media_analysis
# extracteur seul :
STATIOND_ESSENTIA_MODELS=models python3 tools/essentia_analyze.py <fichier>
```

## À suivre

- Parallélisme (`jobs`) et éventuel seuil de confiance avant d'écrire un label.
- Mood : têtes binaires effnet combinées par `max` (calibration indépendante) ;
  à seuiller si un titre franchement énergique ressort `relaxed`.

L'ancien module `bpm_analysis` (heuristique BPM maison) a été retiré, remplacé
par cette analyse.
