#!/usr/bin/env bash
# Télécharge les modèles TensorFlow de l'analyse offline (famille Discogs-EffNet)
# depuis le model zoo d'Essentia (https://essentia.upf.edu/models/) dans CE
# dossier, à plat (noms de base attendus par tools/essentia_analyze.py).
#
#   models/fetch.sh
#
# À lancer sur la machine de dev (réseau requis). Écrit aussi SHA256SUMS : une
# fois validé, committe-le et re-vérifie avec `sha256sum -c SHA256SUMS`.
#
# ATTENTION LICENCE : la plupart de ces modèles MTG sont en CC BY-NC-SA
# (usage non commercial). À vérifier selon l'usage de la radio.
set -euo pipefail

cd "$(dirname "$0")"
base="https://essentia.upf.edu/models"

# <catégorie>/<nom-du-modèle> — le .pb et le .json sont téléchargés tous deux.
models=(
  "feature-extractors/discogs-effnet/discogs-effnet-bs64-1"
  "classification-heads/genre_discogs400/genre_discogs400-discogs-effnet-1"
  "classification-heads/danceability/danceability-discogs-effnet-1"
  "classification-heads/mood_happy/mood_happy-discogs-effnet-1"
  "classification-heads/mood_aggressive/mood_aggressive-discogs-effnet-1"
  "classification-heads/mood_party/mood_party-discogs-effnet-1"
  "classification-heads/mood_relaxed/mood_relaxed-discogs-effnet-1"
)

get() { # <url> <dest>
  [ -s "$2" ] && { echo "déjà là : $2"; return; }
  echo "→ $2"
  curl -fSL --retry 3 -o "$2" "$1" \
    || { echo "échec du téléchargement : $1" >&2; rm -f "$2"; exit 1; }
}

for m in "${models[@]}"; do
  name="$(basename "$m")"
  get "$base/$m.pb"   "$name.pb"
  get "$base/$m.json" "$name.json"   # métadonnées (clé "classes")
done

sha256sum -- *.pb *.json > SHA256SUMS
echo
echo "OK. Modèles dans $(pwd). SHA256SUMS écrit — committe-le et vérifie avec :"
echo "  sha256sum -c SHA256SUMS"
