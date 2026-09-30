#!/bin/sh
# Fetch the free monthly DB-IP City Lite MMDB, without an API key.
# Attribution: IP Geolocation by DB-IP (https://db-ip.com), CC BY 4.0.
# Usage: sh scripts/update-dbip.sh [destination.mmdb] [YYYY-MM]
set -eu

destination=${1:-./data/geoip/dbip-city-lite.mmdb}
release=${2:-$(date -u +%Y-%m)}
case "$release" in
    [0-9][0-9][0-9][0-9]-0[1-9]|[0-9][0-9][0-9][0-9]-1[0-2]) ;;
    *) echo 'Expected a release in YYYY-MM format' >&2; exit 1 ;;
esac
case "$destination" in
    /*|./*|../*) ;;
    *) destination=./$destination ;;
esac
directory=$(dirname "$destination")
mkdir -p "$directory"
# Same filesystem as destination: a successful rename is atomic. A failed
# download/decompression leaves the previous database untouched.
temporary=$(mktemp -d "$directory/.dbip.XXXXXX")
cleanup() {
    rm -f "$temporary/database.gz" "$temporary/database.mmdb"
    rmdir "$temporary"
}
trap cleanup 0
trap 'exit 1' 1 2 15
curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' \
    --connect-timeout 15 --max-time 300 --retry 2 --max-filesize 268435456 \
    "https://download.db-ip.com/free/dbip-city-lite-$release.mmdb.gz" \
    --output "$temporary/database.gz"
gzip -t "$temporary/database.gz"
gzip -dc "$temporary/database.gz" > "$temporary/database.mmdb"
# Sanity check the MMDB metadata marker. The host validates and decodes the
# database itself at startup; this check is not a cryptographic signature.
tail -c 131072 "$temporary/database.mmdb" | LC_ALL=C grep -a -q 'MaxMind.com' || {
    echo 'Downloaded file has no MMDB metadata marker' >&2
    exit 1
}
chmod 0644 "$temporary/database.mmdb"
mv -f "$temporary/database.mmdb" "$destination"
printf 'Installed DB-IP City Lite %s at %s\n' "$release" "$destination"
printf 'IP Geolocation by DB-IP (https://db-ip.com). Restart stationd to load it.\n'
