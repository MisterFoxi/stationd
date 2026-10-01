#!/usr/bin/env bash
# Vérifie les groupes des utilisateurs lecteurs, sans comptes système réels.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/root" "$tmp/cargo"
export TEST_LOG="$tmp/log"
cat > "$tmp/bin/getent" <<'EOF'
#!/usr/bin/env sh
[ "${TEST_EXISTING_GROUP:-0}" = 0 ] || echo "nfsmedia:x:1234:"
EOF
cat > "$tmp/bin/usermod" <<'EOF'
#!/usr/bin/env sh
echo "usermod $*" >> "$TEST_LOG"
EOF
cat > "$tmp/bin/groupadd" <<'EOF'
#!/usr/bin/env sh
echo "groupadd $*" >> "$TEST_LOG"
EOF
cat > "$tmp/bin/id" <<'EOF'
#!/usr/bin/env sh
# Pas de compte dev dans la production.
exit 1
EOF
cat > "$tmp/bin/install" <<'EOF'
#!/usr/bin/env sh
exit 0
EOF
chmod +x "$tmp/bin/"*
for existing in 0 1; do
  for user in stationd dev; do
    : > "$TEST_LOG"
    env -u STATIOND_UID -u STATIOND_GID PATH="$tmp/bin:$PATH" MEDIA_GID=1234 \
      STATIOND_USER="$user" STATIOND_ROOT="$tmp/root" CARGO_HOME="$tmp/cargo" \
      TEST_EXISTING_GROUP="$existing" sh "$root/docker/rootfs/etc/s6-overlay/scripts/init-perms"
    group=media
    if [ "$existing" = 1 ]; then
      group=nfsmedia
      ! grep -q groupadd "$TEST_LOG"
    else
      grep -qx 'groupadd -g 1234 media' "$TEST_LOG"
    fi
    grep -qx "usermod -aG $group liquidsoap" "$TEST_LOG"
    grep -qx "usermod -aG $group $user" "$TEST_LOG"
  done
done
echo 'OK: groupe NFS existant/nouveau, accès Liquidsoap + scanner stationd/dev'
