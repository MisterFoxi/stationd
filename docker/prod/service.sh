#!/usr/bin/env bash
# Host lifecycle, installed in the station's own scripts/ directory.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dir="$(dirname "$here")"
die() { echo "stationd service: $*" >&2; exit 1; }
dc() { docker compose --project-directory "$dir" --env-file "$dir/.env" -f "$dir/compose.yaml" "$@"; }
[ "$(id -u)" = 0 ] || die "à lancer avec sudo"
case "${1:-}" in
  start)
    [ -f "$dir/stationd.toml" ] || die "$dir/stationd.toml absent"
    environment="$(dc config --environment)"
    tag="$(sed -n 's/^STATIOND_VERSION=//p' <<< "$environment")"
    mode="$(sed -n 's/^STATIOND_BIND_MODE=//p' <<< "$environment")"
    case "${mode:-auto}" in
      auto)
        # Route lookup is local: no packet is sent to this address.
        address="$(ip -4 route get 1.1.1.1 | awk '{for(i=1;i<=NF;i++) if($i=="src") {print $(i+1); exit}}')"
        [ -n "$address" ] || die "aucune IPv4 source sur la route par défaut"
        [[ "$tag" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || die "version invalide"
        tmp="$(mktemp -d "$dir/.network.XXXXXXXX")"
        trap 'rm -rf "$tmp"' EXIT
        cp -p "$dir/stationd.toml" "$tmp/stationd.toml"
        docker run --rm -i --entrypoint python3 -v "$dir/stationd.toml:/stationd-config.toml:ro" \
          "stationd:$tag" - "$address" < "$here/network.py" > "$tmp/stationd.toml"
        if ! cmp -s "$dir/stationd.toml" "$tmp/stationd.toml"; then
          cp -p --no-clobber "$dir/stationd.toml" "$dir/stationd.toml.before-auto-ip"
          mv -f "$tmp/stationd.toml" "$dir/stationd.toml"
        fi
        echo "stationd: IPv4 de ce nœud = $address (port gRPC conservé)"
        ;;
      static) ;;
      *) die "STATIOND_BIND_MODE doit être auto ou static";;
    esac
    bash "$here/configure-paths.sh"
    if ! dc up -d --force-recreate --wait --wait-timeout 60; then
      dc logs --tail 80 >&2 || true
      die "station indisponible après démarrage"
    fi
    ;;
  stop) dc stop ;;
  *) die "usage: $0 start|stop";;
esac