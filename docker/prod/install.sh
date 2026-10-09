#!/usr/bin/env bash
# Installe / met à jour le nœud et ses commandes stationctl / stationd-tui.
# sudo ./install.sh [--dir /opt/stationd] [--media /mnt/nfs/radio] [--admin USER]
# --media est utilisé à la première installation seulement.
# Les fichiers de configuration et les médias existants sont préservés.
set -euo pipefail
dir=/opt/stationd
media=/mnt/nfs/radio
admin="${SUDO_USER:-}"
here="$(cd "$(dirname "$0")" && pwd)"
die() { echo "install.sh : $*" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --dir|--media|--admin)
      [ $# -ge 2 ] && [ -n "$2" ] || die "$1 attend une valeur"
      case "$1" in --dir) dir="$2";; --media) media="$2";; --admin) admin="$2";; esac
      shift 2 ;;
    *) die "option inconnue : $1" ;;
  esac
done
[ "$(id -u)" = 0 ] || die "à lancer avec sudo (ou en root avec --admin USER)"
for cmd in docker flock sha256sum getent usermod groupadd useradd install realpath systemctl ip; do
  command -v "$cmd" >/dev/null || die "commande requise absente : $cmd"
done
case "$dir" in /*) ;; *) die "--dir doit être un chemin absolu" ;; esac
dir="$(realpath -m "$dir")"
[ "$dir" != / ] && [ "$dir" != "$here" ] || die "répertoire d'installation invalide : $dir"
[ -z "$admin" ] || [ "$admin" = root ] || id "$admin" >/dev/null 2>&1 || die "compte administrateur inconnu : $admin"
# Un seul installateur à la fois, même pour des répertoires différents :
# les lanceurs /usr/local/bin sont partagés.
exec 9>/run/lock/stationd-install.lock
flock -n 9 || die "une installation stationd est déjà en cours"
docker compose version >/dev/null 2>&1 || die "plugin docker compose absent"
docker info >/dev/null 2>&1 || die "daemon Docker indisponible"
for f in VERSION SHA256SUMS compose.yaml stationd.example.toml client.sh configure-paths.sh paths.py service.sh network.py service-unit.py scripts/update-geolite2.sh radio/error.mp3 radio/bruit.mp3; do
  [ -s "$here/$f" ] || die "bundle incomplet : $f absent ou vide"
done
[ -d "$here/examples" ] || die "bundle incomplet : examples/"
tag="$(cat "$here/VERSION")"
[[ "$tag" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || die "VERSION invalide"
[ -s "$here/stationd-$tag.image.tar.gz" ] || die "image du bundle absente"
(cd "$here" && sha256sum --quiet -c SHA256SUMS) || die "bundle corrompu (SHA256SUMS)"
# Refuser un mauvais montage avant de changer l'installation.
if [ ! -f "$dir/.env" ] && [ ! -f "$dir/stationd.toml" ]; then
  case "$media" in /*) ;; *) die "--media doit être un chemin absolu" ;; esac
  [ -d "$media" ] || die "médiathèque $media absente (montage NFS ?) : préciser --media"
fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
docker load -i "$here/stationd-$tag.image.tar.gz"
# Vérifier les clients et leurs bibliothèques AVANT de changer la version active.
docker run --rm --entrypoint /bin/sh "stationd:$tag" -euc '
  /usr/local/bin/stationctl --help >/dev/null
  /usr/local/bin/stationd-tui --help >/dev/null
  test -s /usr/share/stationd/error.mp3
  test -s /usr/share/stationd/bruit.mp3
  test -x /usr/local/bin/update-geolite2.sh
  for cmd in curl tar gzip stty timeout; do command -v "$cmd" >/dev/null; done
'
getent group stationd >/dev/null || groupadd --system stationd
id stationd >/dev/null 2>&1 || useradd --system -g stationd -d "$dir" -M -s /usr/sbin/nologin stationd
# Préparer et valider .env avant de remplacer les fichiers actifs.
previous=0
if [ -f "$dir/.env" ]; then
  previous=1
  cp "$dir/.env" "$tmp/previous.env"
  [ ! -f "$dir/compose.yaml" ] || cp "$dir/compose.yaml" "$tmp/previous.compose.yaml"
  awk -v tag="$tag" '
    /^STATIOND_VERSION=/ { if (!seen++) print "STATIOND_VERSION=" tag; next }
    { print }
    END { if (!seen) print "STATIOND_VERSION=" tag }
  ' "$dir/.env" > "$tmp/.env"
else
  # Les chemins contenant $, # ou des espaces restent littéraux dans Compose.
  [[ "$media" != *"'"* && "$media" != *$'\n'* ]] || die "--media contient un caractère non pris en charge"
  cat > "$tmp/.env" <<EOF
STATIOND_VERSION=$tag
STATIOND_UID=$(id -u stationd)
STATIOND_GID=$(getent group stationd | cut -d: -f3)
MEDIA_PATH='$media'
MEDIA_GID=$(stat -c %g "$media" 2>/dev/null || getent group stationd | cut -d: -f3)
TZ='${TZ:-UTC}'
STATIOND_BIND_MODE=auto
EOF
fi
# Le TOML est la source des chemins ; MEDIA_PATH sert au bootstrap sans TOML.
media_line="$(docker compose --project-directory "$dir" --env-file "$tmp/.env" -f "$here/compose.yaml" config --environment | sed -n 's/^MEDIA_PATH=//p')"
bash "$here/configure-paths.sh" --dir "$dir" --image "stationd:$tag" \
  --template "$here/compose.yaml" --env-file "$tmp/.env" \
  --output "$tmp/compose.yaml" --service-output "$tmp/stationd.service" --media "${media_line:-$media}"
install -d -m 2770 -o stationd -g stationd "$dir" "$dir/playlist" "$dir/radio" "$dir/grid"
install -d -m 2770 -o stationd -g stationd "$dir/data" "$dir/data/geoip"
install -d -m 0755 "$dir/scripts"
install -m 0755 "$here/scripts/update-geolite2.sh" "$dir/scripts/update-geolite2.sh"
cp -r --no-clobber "$here/radio/." "$dir/radio/"
if [ -f "$dir/grid.toml" ] && [ ! -e "$dir/grid/grid.toml" ]; then
  mv "$dir/grid.toml" "$dir/grid/grid.toml"
fi
find "$dir/playlist" "$dir/radio" "$dir/grid" -mindepth 1 -type d -exec chmod 2770 {} +
chgrp -R stationd "$dir/playlist" "$dir/radio" "$dir/grid"
chmod -R g+rwX "$dir/playlist" "$dir/radio" "$dir/grid"
find "$dir" -maxdepth 1 -name '*.toml' -exec chgrp stationd {} + -exec chmod g+rw {} +
install -m 0644 "$tmp/compose.yaml" "$dir/compose.yaml"
install -m 0644 "$here/compose.yaml" "$dir/scripts/compose.template.yaml"
install -m 0755 "$here/configure-paths.sh" "$dir/scripts/configure-paths.sh"
install -m 0644 "$here/paths.py" "$dir/scripts/paths.py"
install -m 0755 "$here/service.sh" "$dir/scripts/service.sh"
install -m 0644 "$here/network.py" "$dir/scripts/network.py"
install -m 0644 "$here/service-unit.py" "$dir/scripts/service-unit.py"
install -d -m 0755 /etc/systemd/system
install -m 0644 "$tmp/stationd.service" /etc/systemd/system/stationd.service
systemctl daemon-reload
systemctl enable docker.service stationd.service
install -m 0660 -o stationd -g stationd "$here/stationd.example.toml" "$dir/stationd.example.toml"
# Rafraîchir les exemples sans effacer ceux en place avant la copie.
cp -r "$here/examples" "$tmp/examples"
chown -R stationd:stationd "$tmp/examples"
find "$tmp/examples" -type d -exec chmod 2770 {} +
find "$tmp/examples" -type f -exec chmod 0660 {} +
rm -rf "$dir/examples"
cp -a "$tmp/examples" "$dir/examples"
install -m 0640 -o root -g stationd "$tmp/.env" "$dir/.env.new"
mv -f "$dir/.env.new" "$dir/.env"
# Lanceurs root-owned : aucun binaire natif / alias à configurer sur le nœud.
install -d -m 0755 /usr/local/libexec /usr/local/bin
install -m 0755 "$here/client.sh" /usr/local/libexec/stationd-client
for client in stationctl stationd-tui; do
  {
    printf '#!/usr/bin/env bash\n'
    printf 'exec /usr/local/libexec/stationd-client %q %q "$@"\n' "$dir" "$client"
  } > "$tmp/$client"
  install -m 0755 "$tmp/$client" "/usr/local/bin/$client"
done
relog=""
if [ -n "$admin" ] && [ "$admin" != root ]; then
  getent group docker >/dev/null || groupadd --system docker
  for group in stationd docker; do
    if ! id -nG "$admin" | tr ' ' '\n' | grep -qx "$group"; then
      usermod -aG "$group" "$admin"
      relog="Reconnecter la session SSH de $admin pour activer les groupes stationd/docker. Les lanceurs utilisent sudo en attendant."
    fi
  done
fi
echo "Commandes installées : /usr/local/bin/stationctl et /usr/local/bin/stationd-tui"
[ -z "$relog" ] || echo "$relog"
if [ ! -f "$dir/stationd.toml" ]; then
  echo "Installation prête, configuration requise : créer $dir/stationd.toml depuis stationd.example.toml."
  echo "Puis : sudo systemctl start stationd"
  exit 0
fi
dc() { docker compose --project-directory "$dir" -f "$dir/compose.yaml" "$@"; }
if systemctl restart stationd.service; then
  if [ -f "$dir/data/stationd.stopped" ]; then
    echo "Installation mise à jour ; stationd reste volontairement arrêté. Reprise : stationctl station start"
  else
    echo "stationd $tag installé ; conteneur démarré et gRPC disponible. Administration : stationctl ; stationd-tui"
  fi
else
  dc logs --tail 80 >&2 || true
  if [ "$previous" = 1 ]; then
    install -m 0640 -o root -g stationd "$tmp/previous.env" "$dir/.env"
    [ ! -f "$tmp/previous.compose.yaml" ] || install -m 0644 "$tmp/previous.compose.yaml" "$dir/compose.yaml"
    echo "Échec du démarrage : restauration de la version précédente dans .env." >&2
    systemctl restart stationd.service >&2 || true
  fi
  die "service stationd en échec ; consulter journalctl -u stationd et docker compose logs dans $dir"
fi
