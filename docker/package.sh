#!/usr/bin/env bash
# Packager d'exploitation — à lancer sur la machine de dev (devstationd), à la
# racine du dépôt ou d'ailleurs :
#
#   docker/package.sh [--allow-dirty] [--vm utilisateur@hôte]
#
# 1. compile stationd, stationctl et stationd-tui (--release --locked) et les plugins WASM
#    dans le conteneur de dev (qui doit tourner : docker compose up -d) ;
# 2. construit l'image d'exploitation (docker/Dockerfile.prod, sans toolchain) ;
# 3. vérifie l'image (bibliothèques, Liquidsoap, Icecast, plugins) ;
# 4. produit dist/stationd-<version>-<rev>.tar : image + compose.yaml +
#    install.sh + stationd.example.toml + scripts/ + examples/ + radio/ + SHA256SUMS.
#
# Sur le nœud :
#   scp dist/stationd-<tag>.tar <vm>:/tmp/
#   ssh <vm> 'cd /tmp && tar xf stationd-<tag>.tar && sudo stationd-<tag>/install.sh'
#
# --allow-dirty : accepte un arbre git modifié (tag suffixé -dirty).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

die() { echo "package.sh : $*" >&2; exit 1; }
step() { echo; echo "== $*"; }

allow_dirty=0
vm="${VM:-}"
while [ $# -gt 0 ]; do
  case "$1" in
    --allow-dirty) allow_dirty=1; shift ;;
    --vm)
      [ $# -ge 2 ] && [ -n "$2" ] || die "--vm attend une cible SSH"
      vm="$2"; shift 2 ;;
    *) die "option inconnue : $1" ;;
  esac
done
if [ -n "$vm" ]; then
  [[ "$vm" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.@:-]*$ ]] || die "cible SSH invalide : $vm"
  for cmd in ssh scp; do
    command -v "$cmd" >/dev/null || die "commande requise absente : $cmd"
  done
fi

# --- Version ----------------------------------------------------------------
version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)"
[ -n "$version" ] || die "version introuvable dans Cargo.toml"
rev="$(git rev-parse --short=8 HEAD)"
tag="$version-$rev"
if [ -n "$(git status --porcelain)" ]; then
  [ "$allow_dirty" = 1 ] || die "arbre git modifié (commit, ou --allow-dirty)"
  tag="$tag-dirty"
fi

dc() { docker compose -f "$root/compose.yaml" "$@"; }
dc ps --status running --services | grep -qx station \
  || die "conteneur de dev arrêté : docker compose up -d"

# --- 1. Compilation (conteneur de dev) --------------------------------------
step "compilation stationd / stationctl / stationd-tui ($tag)"
# Tous les binaires du workspace, dont stationd, stationctl et stationd-tui.
# Toujours release, indépendamment de la branche ou du PROFILE de Make.
dc exec -T -u dev station cargo build --release --locked --workspace --bins

plugins=()
for manifest in plugins/*/Cargo.toml; do
  dir="$(dirname "$manifest")"
  step "compilation plugin $dir"
  dc exec -T -u dev -w "/src/$dir" station \
    cargo build --release --locked --target wasm32-unknown-unknown
  plugins+=("$dir")
done

# --- 2. Contexte de build ---------------------------------------------------
stage="$root/dist/stage"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/plugins" "$stage/share" "$stage/models"

# target/ du crate principal = volume nommé du conteneur, invisible de l'hôte.
for b in stationd stationctl stationd-tui; do
  dc cp "station:/src/target/release/$b" "$stage/bin/$b"
done
# target/ des plugins = sur le dépôt monté. Un seul .wasm par crate.
for dir in "${plugins[@]}"; do
  n=0
  for w in "$dir"/target/wasm32-unknown-unknown/release/*.wasm; do
    [ -f "$w" ] || continue
    cp "$w" "$stage/plugins/"
    n=$((n + 1))
  done
  [ "$n" = 1 ] || die "$dir : $n fichier(s) .wasm au lieu d'un"
done
# Fallback et bruit de fond livrés dans l'image : défauts de fallback_path /
# halted_path (config.rs, DEFAULT_FALLBACK_PATH / DEFAULT_HALTED_PATH).
for f in error.mp3 bruit.mp3; do
  [ -s "radio/$f" ] || die "radio/$f absent ou vide"
done
# Tout radio/, y compris les sous-répertoires et fichiers cachés.
cp -r radio/. "$stage/share/"
cp -r docker/rootfs "$stage/rootfs"

# Analyse média offline : l'extracteur Essentia (livré via bin/ → /usr/local/bin,
# chmod 0755 par le COPY de Dockerfile.prod) et ses modèles TF (bakés via models/).
[ -f tools/essentia_analyze.py ] || die "tools/essentia_analyze.py absent"
cp tools/essentia_analyze.py "$stage/bin/essentia_analyze.py"
install -m 0755 scripts/update-geolite2.sh "$stage/bin/update-geolite2.sh"
if [ -d models ] && [ -n "$(ls -A models 2>/dev/null)" ]; then
  cp -r models/. "$stage/models/"
else
  echo "package.sh : AVERTISSEMENT — models/ vide ou absent : l'analyse Essentia" \
       "ne produira rien tant que les modèles TF ne sont pas fournis" \
       "(cf. tools/essentia_analyze.py)." >&2
fi

# --- 3. Image ---------------------------------------------------------------
step "image stationd:$tag"
docker build -f docker/Dockerfile.prod \
  --build-arg VERSION="$version" --build-arg REVISION="$rev" \
  -t "stationd:$tag" "$stage"

step "vérification de l'image"
docker run --rm --entrypoint /bin/sh "stationd:$tag" -euc '
  if ldd /usr/local/bin/stationd /usr/local/bin/stationctl /usr/local/bin/stationd-tui | grep "not found"; then
    echo "bibliothèque manquante" >&2; exit 1
  fi
  /usr/local/bin/stationctl --help >/dev/null
  /usr/local/bin/stationd-tui --help >/dev/null
  liquidsoap --version | head -n1
  icecast2 -v
  ls /usr/lib/stationd/plugins
  test -s /usr/share/stationd/error.mp3 && test -s /usr/share/stationd/bruit.mp3
  test -d /usr/share/zoneinfo/Europe
  test -x /usr/local/bin/essentia_analyze.py
  test -x /usr/local/bin/update-geolite2.sh
  for cmd in curl tar gzip stty; do command -v "$cmd" >/dev/null; done
  python3 --version >/dev/null
'

# Vérifier aussi sous l'identité qui charge les plugins, pas seulement root.
docker run --rm --user stationd --entrypoint /bin/sh "stationd:$tag" -euc '
  for w in /usr/lib/stationd/plugins/*.wasm; do
    test -f "$w" && test -r "$w" || { echo "plugin inaccessible à stationd: $w" >&2; exit 1; }
  done
  test -r /usr/share/stationd/error.mp3
  test -r /usr/share/stationd/bruit.mp3
'

# --- 4. Bundle --------------------------------------------------------------
out="$root/dist/stationd-$tag"
rm -rf "$out" "$out.tar"
mkdir -p "$out"
step "export de l'image"
docker save "stationd:$tag" | gzip > "$out/stationd-$tag.image.tar.gz"
cp docker/prod/compose.yaml stationd.example.toml "$out/"
install -d "$out/scripts"
install -m 0755 scripts/update-geolite2.sh "$out/scripts/update-geolite2.sh"
cp -r examples "$out/examples"
cp -r radio "$out/radio"
install -m 0755 docker/prod/install.sh "$out/install.sh"
install -m 0755 docker/prod/client.sh "$out/client.sh"
echo "$tag" > "$out/VERSION"
(cd "$out" && find . -type f -printf '%P\0' | sort -z | xargs -0 sha256sum -- > "$root/dist/SHA256SUMS.tmp")
mv "$root/dist/SHA256SUMS.tmp" "$out/SHA256SUMS"
tar -C "$root/dist" -cf "$out.tar" "stationd-$tag"
rm -rf "$out" "$stage"

step "prêt : dist/stationd-$tag.tar ($(du -h "$out.tar" | cut -f1))"
if [ -n "$vm" ]; then
  bash "$root/docker/deploy.sh" "$out.tar" "$vm"
else
  cat <<EOF
  make package VM=<cible SSH>  # compiler et installer automatiquement
  bash docker/deploy.sh dist/stationd-$tag.tar <cible SSH>  # installer ce paquet
EOF
fi
