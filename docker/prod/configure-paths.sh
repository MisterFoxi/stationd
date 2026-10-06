#!/usr/bin/env bash
# Regenerate Compose bind mounts from stationd.toml without editing the TOML.
# sudo bash scripts/configure-paths.sh (then docker compose up -d)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="$(dirname "$here")"
template="$here/compose.template.yaml"
envfile=""
output=""
image=""
media=/mnt/nfs/radio
die() { echo "configure-paths.sh : $*" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --dir|--template|--env-file|--output|--image|--media)
      [ $# -ge 2 ] || die "$1 attend une valeur"
      case "$1" in
        --dir) dir="$2";; --template) template="$2";; --env-file) envfile="$2";;
        --output) output="$2";; --image) image="$2";; --media) media="$2";;
      esac
      shift 2;;
    *) die "option inconnue : $1";;
  esac
done
[ "$(id -u)" = 0 ] || die 'à lancer avec sudo'
dir="$(realpath -m "$dir")"
envfile="${envfile:-$dir/.env}"
output="${output:-$dir/compose.yaml}"
[ -f "$template" ] && [ -f "$here/paths.py" ] || die 'générateur incomplet'
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
if [ -z "$image" ]; then
  tag="$(docker compose --project-directory "$dir" --env-file "$envfile" -f "$dir/compose.yaml" config --environment | sed -n 's/^STATIOND_VERSION=//p')"
  [[ "$tag" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || die 'STATIOND_VERSION invalide'
  image="stationd:$tag"
fi
config="$dir/stationd.toml"
if [ ! -f "$config" ]; then
  : > "$tmp/empty.toml"
  config="$tmp/empty.toml"
fi
docker run --rm -i --entrypoint python3 \
  -v "$config:/stationd-config.toml:ro" "$image" - "$dir" "$media" \
  < "$here/paths.py" > "$tmp/plan" || die 'lecture du TOML impossible'
[ -s "$tmp/plan" ] || die 'plan de montages vide'
groups=()
targets=()
: > "$tmp/volumes"
while IFS=$'\t' read -r role source target location mount; do
  if [ "$location" = local ] && [ "$role" != media ]; then
    install -d -m 2770 -o stationd -g stationd "$source"
  fi
  [ -d "$source" ] || die "[$role] dossier hôte absent : $source (vérifier le montage NFS)"
  gid="$(stat -c %g "$source")"
  [[ "$gid" =~ ^[0-9]+$ ]] || die "groupe invalide : $source"
  groups+=("$gid")
  if [ "$location" = external ]; then
    duplicate=0
    for existing in "${targets[@]}"; do [ "$target" != "$existing" ] || duplicate=1; done
    if [ "$duplicate" = 0 ]; then
      printf '      - %s\n' "$mount" >> "$tmp/volumes"
      targets+=("$target")
    fi
  fi
done < "$tmp/plan"
gids="$(printf '%s\n' "${groups[@]}" | sort -nu | tr '\n' ' ')"
grep -q '^ *# STATIOND_EXTERNAL_VOLUMES$' "$template" || die 'modèle Compose incompatible'
awk -v volumes="$tmp/volumes" -v gids="$gids" '
  /# STATIOND_PATH_GROUPS$/ {print "      STATIOND_PATH_GIDS: \047" gids "\047"; next}
  /# STATIOND_EXTERNAL_VOLUMES$/ {while ((getline line < volumes)>0) print line; close(volumes); next}
  {print}
' "$template" > "$tmp/compose.yaml"
docker compose --project-directory "$dir" --env-file "$envfile" -f "$tmp/compose.yaml" config --quiet
install -m 0644 "$tmp/compose.yaml" "$output.new"
mv -f "$output.new" "$output"
echo "Compose généré depuis $dir/stationd.toml : $output"
