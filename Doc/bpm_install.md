# Analyse BPM hors antenne

Patch incremental : stationd-bpm-analysis.patch.
Base : dev 026207e8470f337e56992d7523859f7d9ec41234 avec les correctifs
metadata, migration 0023 et stationd-mp3-writeback.patch deja appliques.
Ne pas reappliquer les anciens correctifs.

Depuis la racine du depot, apres avoir copie le nouveau patch :

```sh
git apply --check stationd-bpm-analysis.patch
git apply stationd-bpm-analysis.patch
docker compose exec -u root station sh -c 'apt-get update && apt-get install -y --no-install-recommends ffmpeg'
make plugins P=custom-tags-wasm
make test
make restart
make ctl A="library scan"
```

L'installation de FFmpeg ci-dessus ne recree pas le conteneur. Les Dockerfiles
sont aussi corriges pour les prochaines constructions d'image. `make restart`
recompile et relance stationd seul. Le scan analyse les fichiers directement :
aucun besoin de les programmer ou de les jouer a l'antenne.

Ta configuration actuelle active l'analyse automatiquement car tempo.enabled
est true et source_tags contient BPM. Tu peux rendre le choix explicite en
ajoutant cette ligne a la table EXISTANTE, sans dupliquer la table :

```toml
[plugin.config.tempo]
enabled = true
analyze_missing = true
source_tags = ["BPM"]
```

Conserve les plages slow/medium/fast et la configuration creation deja en place.
Changer la configuration demande un redemarrage du daemon.

Apres un scan concluant :

```sh
eyeD3 '/mnt/nfs/radio/Punk/TEN LITTLE DRINKERS.mp3'
```

Le fichier doit contenir un BPM standard (TBPM), TXXX:tempo (slow, medium ou
fast selon tes plages), et le TXXX:creation deja present. Aucun BPM precis
n'est annonce ici pour ton fichier : il n'a pas ete fourni a cet environnement.

Si l'analyse est trop incertaine, le journal nomme le fichier avec
"BPM not estimated; no BPM written" et une raison. Le fichier reste indexe,
sans estimation forcee. L'algorithme peut confondre demi-tempo et double-tempo ;
il analyse au plus les 120 premieres secondes et exige au moins 30 secondes.
Un BPM valide deja present est conserve. Le premier scan d'une grande
bibliotheque sans BPM sera plus long ; les fichiers avec TBPM ne sont pas
reanalysses.

Tests d'integration optionnels, avec FFmpeg installe et le plugin compile :

```sh
docker compose exec -u dev station cargo test --locked --lib real_mp3_analysis_writeback_and_rescan -- --ignored
docker compose exec -u dev -e STATIOND_CUSTOM_TAGS_WASM=/src/plugins/custom-tags-wasm/target/wasm32-unknown-unknown/release/custom_tags_wasm.wasm station cargo test --locked --lib offline_mp3_scan_through_real_wasm -- --ignored
```
