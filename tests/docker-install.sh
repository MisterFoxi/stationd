#!/usr/bin/env bash
# Tests de packaging / installation avec Docker et comptes système simulés.
# Aucun accès root, daemon Docker ou compilation Rust nécessaire.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/mock" "$tmp/node" "$tmp/media" "$tmp/bundle/radio/sub" "$tmp/bundle/examples" "$tmp/system/bin" "$tmp/system/libexec"
export TEST_HOME="$tmp"
export TEST_LOG="$tmp/log"
export TEST_MEDIA="$tmp/media"
: > "$TEST_LOG"
for cmd in chown chgrp flock groupadd useradd; do
  printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/mock/$cmd"
done
cat > "$tmp/mock/id" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -u) [ "${2:-}" = stationd ] && echo 982 || echo "${TEST_UID:-0}" ;;
  -nG) echo foxi ;;
  *) echo 'uid=982(stationd) gid=982(stationd)' ;;
esac
EOF
cat > "$tmp/mock/getent" <<'EOF'
#!/usr/bin/env bash
echo "$2:x:982:"
EOF
cat > "$tmp/mock/usermod" <<'EOF'
#!/usr/bin/env bash
printf 'usermod %s\n' "$*" >> "$TEST_LOG"
EOF
cat > "$tmp/mock/install" <<'EOF'
#!/usr/bin/env bash
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-g) shift 2;;
    -m) if [[ "$OSTYPE" = msys* ]]; then shift 2; else args+=("$1" "$2"); shift 2; fi;;
    *) args+=("$1"); shift;;
  esac
done
exec /usr/bin/install "${args[@]}"
EOF
cat > "$tmp/mock/sudo" <<'EOF'
#!/usr/bin/env bash
echo sudo >> "$TEST_LOG"
exec "$@"
EOF
cat > "$tmp/mock/sleep" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "$tmp/mock/docker" <<'EOF'
#!/usr/bin/env bash
printf 'docker' >> "$TEST_LOG"
printf ' %q' "$@" >> "$TEST_LOG"
printf '\n' >> "$TEST_LOG"
if [ "$1" = info ]; then exit "${TEST_INFO_FAIL:-0}"; fi
if [ "$1" = save ]; then printf image; exit 0; fi
if [ "$1" = compose ]; then
  for arg in "$@"; do
    case "$arg" in
      --environment) printf 'MEDIA_PATH=%s\n' "$TEST_MEDIA"; exit 0;;
      ps) echo station; exit 0;;
      cp) dest="${@: -1}"; printf binary > "$dest"; exit 0;;
    esac
  done
  if [[ " $* " = *" status "* ]]; then exit "${TEST_STATUS_FAIL:-0}"; fi
fi
exit 0
EOF
cat > "$tmp/mock/git" <<'EOF'
#!/usr/bin/env bash
case "$1" in rev-parse) echo deadbeef;; status) ;; esac
EOF
chmod +x "$tmp/mock/"*
if [[ "$OSTYPE" = msys* ]]; then
  # Windows ne prend pas en charge les modes setgid Unix.
  printf '#!/usr/bin/env bash\nexit 0\n' > "$tmp/mock/chmod"
  /usr/bin/chmod +x "$tmp/mock/chmod"
fi
export PATH="$tmp/mock:$PATH"
# Rediriger uniquement les chemins système du script testé vers le bac à sable.
sed -e "s|/run/lock/stationd-install.lock|$tmp/install.lock|g" \
    -e "s|/usr/local/libexec|$tmp/system/libexec|g" \
    -e "s|/usr/local/bin|$tmp/system/bin|g" \
    "$root/docker/prod/install.sh" > "$tmp/bundle/install.sh"
cp "$root/docker/prod/client.sh" "$tmp/bundle/client.sh"
cp "$root/docker/prod/compose.yaml" "$tmp/bundle/compose.yaml"
printf config > "$tmp/bundle/stationd.example.toml"
printf error > "$tmp/bundle/radio/error.mp3"
printf bruit > "$tmp/bundle/radio/bruit.mp3"
printf extra > "$tmp/bundle/radio/sub/extra.mp3"
printf hidden > "$tmp/bundle/radio/.hidden"
printf example > "$tmp/bundle/examples/example.toml"
printf v1 > "$tmp/bundle/VERSION"
printf image > "$tmp/bundle/stationd-v1.image.tar.gz"
checksums() { (cd "$tmp/bundle"; find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS); }
checksums
# Un mauvais montage doit échouer AVANT de charger l'image ou écrire .env.
if bash "$tmp/bundle/install.sh" --dir "$tmp/node" --media "$tmp/missing" --admin foxi > "$tmp/output" 2>&1; then exit 1; fi
! grep -q 'docker load' "$TEST_LOG"
[ ! -f "$tmp/node/.env" ]
# Installation fraîche sans config : radio complet, lanceurs, groupes, pas de démarrage.
bash "$tmp/bundle/install.sh" --dir "$tmp/node" --media "$tmp/media" --admin foxi > "$tmp/output"
diff -r "$tmp/bundle/radio" "$tmp/node/radio"
test -x "$tmp/system/bin/stationctl"
test -x "$tmp/system/bin/stationd-tui"
grep -q 'usermod -aG docker foxi' "$TEST_LOG"
grep -q 'usermod -aG stationd foxi' "$TEST_LOG"
grep -q 'configuration requise' "$tmp/output"
# Mise à jour : garder config, fichiers personnalisés et montage ; ajouter VERSION si absente.
printf custom > "$tmp/node/radio/error.mp3"
printf config > "$tmp/node/stationd.toml"
printf 'MEDIA_PATH=%s\nSTATIOND_UID=982\nSTATIOND_GID=982\nMEDIA_GID=982\n' "$tmp/media" > "$tmp/node/.env"
cp "$tmp/node/stationd.toml" "$tmp/expected-config"
bash "$tmp/bundle/install.sh" --dir "$tmp/node" --admin foxi > "$tmp/output"
test "$(cat "$tmp/node/radio/error.mp3")" = custom
cmp "$tmp/expected-config" "$tmp/node/stationd.toml"
grep -qx STATIOND_VERSION=v1 "$tmp/node/.env"
grep -q 'répond' "$tmp/output"
# Échec de readiness : restaurer la version précédente et rendre un échec.
sed -i 's/STATIOND_VERSION=v1/STATIOND_VERSION=old/' "$tmp/node/.env"
if TEST_STATUS_FAIL=1 bash "$tmp/bundle/install.sh" --dir "$tmp/node" --admin foxi > "$tmp/output" 2>&1; then exit 1; fi
grep -qx STATIOND_VERSION=old "$tmp/node/.env"
grep -q 'restauration' "$tmp/output"
# Arrêt volontaire : ne pas réactiver le daemon, ni attendre un RPC impossible.
touch "$tmp/node/data/stationd.stopped"
TEST_STATUS_FAIL=1 bash "$tmp/bundle/install.sh" --dir "$tmp/node" --admin foxi > "$tmp/output"
grep -q 'volontairement arrêté' "$tmp/output"
# Corruption du bundle : refuser sans modification de la version active.
printf corrupt >> "$tmp/bundle/radio/bruit.mp3"
cp "$tmp/node/.env" "$tmp/expected-env"
if bash "$tmp/bundle/install.sh" --dir "$tmp/node" --admin foxi > "$tmp/output" 2>&1; then exit 1; fi
cmp "$tmp/expected-env" "$tmp/node/.env"
# Lanceur CLI : arguments exacts, hors du répertoire, pipes et repli sudo.
: > "$TEST_LOG"
bash "$root/docker/prod/client.sh" "$tmp/node" stationctl playlist add 'a b.toml'
grep -Fq 'a\ b.toml' "$TEST_LOG"
grep -q 'exec -u stationd -T station /usr/local/bin/stationctl' "$TEST_LOG"
TEST_UID=1000 TEST_INFO_FAIL=1 bash "$root/docker/prod/client.sh" "$tmp/node" stationctl status
grep -qx sudo "$TEST_LOG"
if bash "$root/docker/prod/client.sh" "$tmp/node" stationd-tui > "$tmp/output" 2>&1; then exit 1; fi
grep -q 'terminal interactif' "$tmp/output"
# Exécuter le packager complet avec compilation/image simulées.
mkdir -p "$tmp/repo/docker/prod" "$tmp/repo/docker/rootfs" "$tmp/repo/plugins/example/target/wasm32-unknown-unknown/release"
cp "$root/docker/package.sh" "$tmp/repo/docker/package.sh"
cp "$root/docker/prod/"{install.sh,client.sh,compose.yaml} "$tmp/repo/docker/prod/"
cp -r "$tmp/bundle/radio" "$tmp/repo/radio"
cp -r "$tmp/bundle/examples" "$tmp/repo/examples"
cp "$tmp/bundle/stationd.example.toml" "$tmp/repo/"
printf 'version = "0.1.0"\n' > "$tmp/repo/Cargo.toml"
printf manifest > "$tmp/repo/plugins/example/Cargo.toml"
printf wasm > "$tmp/repo/plugins/example/target/wasm32-unknown-unknown/release/example.wasm"
bash "$tmp/repo/docker/package.sh" > "$tmp/output"
mkdir "$tmp/unpack"
tar -C "$tmp/unpack" -xf "$tmp/repo/dist/stationd-0.1.0-deadbeef.tar"
bundle="$tmp/unpack/stationd-0.1.0-deadbeef"
(cd "$bundle"; sha256sum --quiet -c SHA256SUMS)
diff -r "$tmp/repo/radio" "$bundle/radio"
test -x "$bundle/client.sh"
grep -q -- '-p stationd-tui' "$TEST_LOG"
grep -q 'station:/src/target/release/stationd-tui' "$TEST_LOG"
echo 'OK: preflight, fresh install, update, radio, groups, launchers, rollback, intentional stop, checksum, CLI, packaging + TUI'

