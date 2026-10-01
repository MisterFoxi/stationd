#!/usr/bin/env bash
# Lanceur partagé, appelé avec le répertoire d'installation et le client.
set -euo pipefail
dir="$1"
client="$2"
shift 2
case "$client" in stationctl|stationd-tui) ;; *) echo "client inconnu" >&2; exit 1 ;; esac
docker_cmd=(docker)
# Les groupes d'une session existante ne changent pas après usermod.
# Le repli sudo permet d'utiliser les commandes avant la reconnexion SSH.
if [ ! -r "$dir/.env" ] || ! docker info >/dev/null 2>&1; then
  if [ "$(id -u)" = 0 ]; then
    echo "Docker indisponible : vérifier le service docker." >&2
    exit 1
  fi
  docker_cmd=(sudo docker)
fi
args=(compose --project-directory "$dir" -f "$dir/compose.yaml" exec -u stationd)
if [ "$client" = stationctl ]; then
  args+=(-T)
else
  # Une TUI exige un terminal interactif ; stationctl accepte les pipelines.
  if [ ! -t 0 ] || [ ! -t 1 ]; then
    echo "stationd-tui exige un terminal interactif." >&2
    exit 1
  fi
  args+=(-e "TERM=${TERM:-xterm-256color}" -e "LANG=${LANG:-C.UTF-8}")
  for name in LC_ALL LC_MESSAGES; do
    [ -z "${!name:-}" ] || args+=(-e "$name=${!name}")
  done
fi
exec "${docker_cmd[@]}" "${args[@]}" station "/usr/local/bin/$client" "$@"
