#!/usr/bin/env bash
# Installation / mise à jour de stationd sur un nœud (VM ou bare-metal), en
# root, depuis le dossier du bundle décompressé (produit par docker/package.sh).
#
#   ./install.sh [--media /mnt/nfs/radio]
#
# --media : chemin de la médiathèque (monté sur l'hôte), pris en compte à la
# première installation seulement (ensuite : éditer /opt/stationd/.env).
#
# Ne touche jamais à /srv/stationd/stationd.toml ni aux données. L'image
# précédente reste chargée : retour arrière = remettre l'ancien
# STATIOND_VERSION dans /opt/stationd/.env puis `docker compose up -d`.
set -euo pipefail

OPT=/opt/stationd
SRV=/srv/stationd
here="$(cd "$(dirname "$0")" && pwd)"

die() { echo "install.sh : $*" >&2; exit 1; }

media=""
while [ $# -gt 0 ]; do
  case "$1" in
    --media) [ $# -ge 2 ] || die "--media attend un chemin"; media="$2"; shift 2 ;;
    *) die "option inconnue : $1" ;;
  esac
done

[ "$(id -u)" = 0 ] || die "à lancer en root"
command -v docker >/dev/null || die "docker absent (paquets docker-ce + containerd.io, jamais docker.io)"
docker compose version >/dev/null 2>&1 || die "plugin docker compose absent (docker-compose-plugin)"

tag="$(cat "$here/VERSION")"
image="$here/stationd-$tag.image.tar.gz"
(cd "$here" && sha256sum --quiet -c SHA256SUMS) || die "sommes de contrôle invalides : bundle corrompu"

echo "== chargement de l'image stationd:$tag"
docker load -i "$image"

# Identité hôte de stationd : propriétaire de /srv/stationd, mêmes UID/GID
# dans le conteneur (init-perms).
getent group stationd >/dev/null || groupadd --system stationd
id stationd >/dev/null 2>&1 \
  || useradd --system -g stationd -d "$SRV" -M -s /usr/sbin/nologin stationd

install -d -m 0750 -o stationd -g stationd "$SRV" "$SRV/data" "$SRV/playlist" "$SRV/radio"
install -m 0640 -o stationd -g stationd "$here/stationd.example.toml" "$SRV/stationd.example.toml"

install -d -m 0755 "$OPT"
install -m 0644 "$here/compose.yaml" "$OPT/compose.yaml"

if [ ! -f "$OPT/.env" ]; then
  media="${media:-/mnt/nfs/radio}"
  [ -d "$media" ] || die "médiathèque $media absente (montage NFS ?) — préciser --media"
  tz="$(cat /etc/timezone 2>/dev/null || true)"
  cat > "$OPT/.env" <<EOF
STATIOND_VERSION=$tag
STATIOND_UID=$(id -u stationd)
STATIOND_GID=$(getent group stationd | cut -d: -f3)
MEDIA_PATH=$media
MEDIA_GID=$(stat -c %g "$media")
TZ=${tz:-UTC}
EOF
  chmod 0644 "$OPT/.env"
  echo "== $OPT/.env créé"
else
  [ -z "$media" ] || die "--media ignoré : $OPT/.env existe déjà (modifier MEDIA_PATH / MEDIA_GID dedans)"
  sed -i "s/^STATIOND_VERSION=.*/STATIOND_VERSION=$tag/" "$OPT/.env"
  echo "== $OPT/.env : STATIOND_VERSION=$tag"
fi

cat <<EOF

stationctl (à mettre dans le ~/.bashrc de l'administrateur) :
  alias stationctl='docker compose -f $OPT/compose.yaml exec -u stationd station stationctl'
  (répertoire courant dans le conteneur : $SRV → grid.toml, playlist/ …)
EOF

if [ ! -f "$SRV/stationd.toml" ]; then
  cat <<EOF

Première installation — reste à faire avant le démarrage :
  1. $SRV/stationd.toml, d'après $SRV/stationd.example.toml :
       [media] library_path = "$(grep '^MEDIA_PATH=' "$OPT/.env" | cut -d= -f2-)"
       [liquidsoap] control_socket = "/run/stationd/liquidsoap.sock"
       plugins WASM : wasm = "/usr/lib/stationd/plugins/<nom>.wasm"
  2. fallback et bruit de fond dans $SRV/radio/ (fallback_path / halted_path)
  3. chown -R stationd:stationd $SRV
  4. docker compose -f $OPT/compose.yaml up -d
EOF
  exit 0
fi

echo "== démarrage"
docker compose -f "$OPT/compose.yaml" up -d
echo "journal : docker compose -f $OPT/compose.yaml logs -f station"
