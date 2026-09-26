#!/usr/bin/env bash
# Installe ou met à jour stationd. Tout va dans UN répertoire.
#
#   sudo ./install.sh [--dir /opt/stationd] [--media /mnt/nfs/radio]
#
#   --dir    répertoire d'installation (défaut /opt/stationd)
#   --media  médiathèque montée sur l'hôte (défaut /mnt/nfs/radio) ;
#            lu seulement à la création du .env
#
# Le script installe compose.yaml, stationd.example.toml et, la première
# fois, .env. Il ne touche jamais à stationd.toml, grid.toml, playlist/,
# radio/ ni data/. Mise à jour : seule la ligne STATIOND_VERSION du .env
# change ; l'ancienne image reste chargée (retour arrière = remettre
# l'ancienne version dans .env, puis docker compose up -d).
set -euo pipefail

dir=/opt/stationd
media=/mnt/nfs/radio
here="$(cd "$(dirname "$0")" && pwd)"

die() { echo "install.sh : $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)   [ $# -ge 2 ] || die "--dir attend un chemin";   dir="$2";   shift 2 ;;
    --media) [ $# -ge 2 ] || die "--media attend un chemin"; media="$2"; shift 2 ;;
    *) die "option inconnue : $1" ;;
  esac
done

[ "$(id -u)" = 0 ] || die "à lancer en root"
command -v docker >/dev/null || die "docker absent (docker-ce + containerd.io, jamais docker.io)"
docker compose version >/dev/null 2>&1 || die "plugin docker compose absent"

tag="$(cat "$here/VERSION")"
(cd "$here" && sha256sum --quiet -c SHA256SUMS) || die "bundle corrompu (SHA256SUMS)"
docker load -i "$here/stationd-$tag.image.tar.gz"

# Utilisateur système stationd : propriétaire du répertoire ; mêmes UID/GID
# dans le conteneur (init-perms).
getent group stationd >/dev/null || groupadd --system stationd
id stationd >/dev/null 2>&1 \
  || useradd --system -g stationd -d "$dir" -M -s /usr/sbin/nologin stationd

install -d -m 0750 -o stationd -g stationd "$dir" "$dir/data" "$dir/playlist" "$dir/radio"
install -m 0644 "$here/compose.yaml" "$dir/compose.yaml"
install -m 0640 -o stationd -g stationd "$here/stationd.example.toml" "$dir/stationd.example.toml"

if [ -f "$dir/.env" ]; then
  sed -i "s/^STATIOND_VERSION=.*/STATIOND_VERSION=$tag/" "$dir/.env"
else
  [ -d "$media" ] || die "médiathèque $media absente (montage NFS ?) : préciser --media"
  cat > "$dir/.env" <<EOF
STATIOND_VERSION=$tag
STATIOND_UID=$(id -u stationd)
STATIOND_GID=$(getent group stationd | cut -d: -f3)
MEDIA_PATH=$media
MEDIA_GID=$(stat -c %g "$media")
TZ=$(cat /etc/timezone 2>/dev/null || echo UTC)
EOF
fi

if [ ! -f "$dir/stationd.toml" ]; then
  echo
  echo "stationd $tag installé dans $dir, pas démarré : $dir/stationd.toml manque."
  echo "Le créer d'après $dir/stationd.example.toml (README « Package & deploy »), puis :"
  echo "  cd $dir && docker compose up -d"
  exit 0
fi

cd "$dir"
docker compose up -d
echo
echo "stationd $tag démarré ($dir). Journal : cd $dir && docker compose logs -f"
