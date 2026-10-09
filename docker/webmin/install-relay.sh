#!/usr/bin/env bash
# Usage sur la VM de management : sudo bash install-relay.sh [IP_TRAEFIK]
set -euo pipefail

die() { echo "install-relay.sh : $*" >&2; exit 1; }
[ "$#" -le 1 ] || die "usage : $0 [IP_TRAEFIK]"
traefik_ip="${1:-192.168.1.94}"
[[ "$traefik_ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || die "IP Traefik IPv4 invalide"
IFS=. read -r -a octets <<< "$traefik_ip"
for octet in "${octets[@]}"; do
  (( 10#$octet <= 255 )) || die "IP Traefik IPv4 invalide"
done
[ "$EUID" -eq 0 ] || die "exécuter avec sudo sur la VM de management"
for cmd in systemctl systemd-analyze curl install mktemp; do
  command -v "$cmd" >/dev/null || die "commande requise absente : $cmd"
done
proxyd=""
for candidate in /usr/lib/systemd/systemd-socket-proxyd /lib/systemd/systemd-socket-proxyd; do
  if [ -x "$candidate" ]; then proxyd="$candidate"; break; fi
done
[ -n "$proxyd" ] || die "systemd-socket-proxyd absent"
curl --noproxy '*' --fail --silent --show-error --max-time 5 \
  --output /dev/null http://127.0.0.1:8090/login \
  || die "le management web ne répond pas sur 127.0.0.1:8090 ; vérifier remote-supervision"

work_dir="$(mktemp -d)"
trap 'rm -f -- "$work_dir/stationd-webmin.socket" "$work_dir/stationd-webmin.service"; rmdir -- "$work_dir"' EXIT
cat > "$work_dir/stationd-webmin.socket" <<EOF
[Unit]
Description=StationD Webmin relay for Traefik

[Socket]
ListenStream=0.0.0.0:18090
NoDelay=true
IPAddressDeny=any
IPAddressAllow=$traefik_ip/32
IPAddressAllow=localhost

[Install]
WantedBy=sockets.target
EOF
cat > "$work_dir/stationd-webmin.service" <<EOF
[Unit]
Description=Relay Traefik to the loopback StationD Webmin plugin
Requires=stationd-webmin.socket
After=network.target

[Service]
ExecStart=$proxyd 127.0.0.1:8090
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
RestrictAddressFamilies=AF_INET AF_INET6
IPAddressDeny=any
IPAddressAllow=$traefik_ip/32
IPAddressAllow=localhost
EOF
systemd-analyze verify "$work_dir/stationd-webmin.socket" "$work_dir/stationd-webmin.service"

# Sauvegarder les unités existantes avant remplacement.
backup_suffix="$(date -u +%Y%m%dT%H%M%SZ).$$"
for unit in stationd-webmin.socket stationd-webmin.service; do
  if [ -e "/etc/systemd/system/$unit" ]; then
    cp -p -- "/etc/systemd/system/$unit" "/etc/systemd/system/$unit.before-$backup_suffix"
  fi
  install -m 0644 "$work_dir/$unit" "/etc/systemd/system/$unit"
done
systemctl daemon-reload
systemctl stop stationd-webmin.service stationd-webmin.socket
systemctl enable stationd-webmin.socket
systemctl start stationd-webmin.socket

if ! curl --noproxy '*' --fail --silent --show-error --max-time 5 \
  --output /dev/null http://127.0.0.1:18090/login; then
  systemctl status --no-pager stationd-webmin.socket stationd-webmin.service || true
  die "le relais ne répond pas ; consulter journalctl -u stationd-webmin.service -u stationd-webmin.socket"
fi
echo "Relais installé et vérifié : 0.0.0.0:18090 -> 127.0.0.1:8090"
echo "Traefik autorisé : $traefik_ip ; le pare-feu doit aussi autoriser cette IP sur le port 18090."
echo "Depuis Traefik : curl -I --connect-timeout 5 http://HomestoneMgr.lan:18090/login"
