#!/usr/bin/env bash
# Packager d'exploitation — à lancer sur la machine de dev (devstationd), à la
# racine du dépôt ou d'ailleurs :
#
#   docker/package.sh [--allow-dirty]
#
# 1. compile stationd, stationctl (--release --locked) et les plugins WASM
#    dans le conteneur de dev (qui doit tourner : docker compose up -d) ;
# 2. construit l'image d'exploitation (docker/Dockerfile.prod, sans toolchain) ;
# 3. vérifie l'image (bibliothèques, Liquidsoap, Icecast, plugins) ;
# 4. produit dist/stationd-<version>-<rev>.tar : image + compose.yaml +
#    install.sh + stationd.example.toml + SHA256SUMS.
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
case "${1:-}" in
  "") ;;
  --allow-dirty) allow_dirty=1 ;;
  *) die "option inconnue : $1" ;;
esac

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
step "compilation stationd / stationctl ($tag)"
dc exec -T -u dev station cargo build --release --locked --bin stationd --bin stationctl

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
mkdir -p "$stage/bin" "$stage/plugins"

# target/ du crate principal = volume nommé du conteneur, invisible de l'hôte.
for b in stationd stationctl; do
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
cp -r docker/rootfs "$stage/rootfs"

# --- 3. Image ---------------------------------------------------------------
step "image stationd:$tag"
docker build -f docker/Dockerfile.prod \
  --build-arg VERSION="$version" --build-arg REVISION="$rev" \
  -t "stationd:$tag" "$stage"

step "vérification de l'image"
docker run --rm --entrypoint /bin/sh "stationd:$tag" -euc '
  if ldd /usr/local/bin/stationd /usr/local/bin/stationctl | grep "not found"; then
    echo "bibliothèque manquante" >&2; exit 1
  fi
  liquidsoap --version | head -n1
  icecast2 -v
  ls /usr/lib/stationd/plugins
  test -d /usr/share/zoneinfo/Europe
'

# --- 4. Bundle --------------------------------------------------------------
out="$root/dist/stationd-$tag"
rm -rf "$out" "$out.tar"
mkdir -p "$out"
step "export de l'image"
docker save "stationd:$tag" | gzip > "$out/stationd-$tag.image.tar.gz"
cp docker/prod/compose.yaml stationd.example.toml "$out/"
install -m 0755 docker/prod/install.sh "$out/install.sh"
echo "$tag" > "$out/VERSION"
(cd "$out" && sha256sum -- * > SHA256SUMS)
tar -C "$root/dist" -cf "$out.tar" "stationd-$tag"
rm -rf "$out" "$stage"

step "prêt : dist/stationd-$tag.tar ($(du -h "$out.tar" | cut -f1))"
cat <<EOF
  scp dist/stationd-$tag.tar <vm>:/tmp/
  ssh <vm> 'cd /tmp && tar xf stationd-$tag.tar && sudo stationd-$tag/install.sh'
EOF
