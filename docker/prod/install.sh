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
# fois, .env. Il ne modifie jamais le contenu de stationd.toml, grid.toml,
# playlist/, radio/ ni data/ ; il (ré)applique seulement les droits.
# Le compte qui lance sudo rejoint le groupe stationd : config, grille,
# playlists et radio/ s'éditent sans sudo ; data/ reste le répertoire de
# stationd (le groupe n'y crée ni n'y supprime rien).
# Mise à jour : seule la ligne STATIOND_VERSION du .env change ; l'ancienne
# image reste chargée (retour arrière = remettre l'ancienne version dans
# .env, puis docker compose up -d).
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

# Droits (réappliqués à chaque passage) : répertoires partagés en 2770 — le
# setgid donne au groupe stationd ce que l'administrateur y crée ; data/ :
# répertoire en 0750 (le groupe n'y crée ni n'y supprime rien).
install -d -m 2770 -o stationd -g stationd "$dir" "$dir/playlist" "$dir/radio"
install -d -m 0750 -o stationd -g stationd "$dir/data"
find "$dir/playlist" "$dir/radio" -mindepth 1 -type d -exec chmod 2770 {} +
chgrp -R stationd "$dir/playlist" "$dir/radio"
chmod -R g+rwX "$dir/playlist" "$dir/radio"
find "$dir" -maxdepth 1 -name '*.toml' -exec chgrp stationd {} + -exec chmod g+rw {} +

admin="${SUDO_USER:-}"
relog=""
if [ -n "$admin" ] && [ "$admin" != root ] \
   && ! id -nG "$admin" | tr ' ' '\n' | grep -qx stationd; then
  usermod -aG stationd "$admin"
  relog="$admin ajouté au groupe stationd : se reconnecter pour éditer $dir sans sudo."
fi
install -m 0644 "$here/compose.yaml" "$dir/compose.yaml"
install -m 0660 -o stationd -g stationd "$here/stationd.example.toml" "$dir/stationd.example.toml"

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

[ -z "$relog" ] || { echo; echo "$relog"; }

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
