#!/bin/sh
# Download GeoLite2 City using MaxMind's authenticated download permalink.
# Usage: sh scripts/update-geolite2.sh [destination.mmdb] [credentials.netrc]
# Without a netrc file, credentials are prompted on the controlling terminal.
set +x
set -eu
umask 077

destination=${1:-./data/geoip/GeoLite2-City.mmdb}
credentials=${2:-}
case "$destination" in
    /*|./*|../*) ;;
    *) destination=./$destination ;;
esac
directory=$(dirname "$destination")
mkdir -p "$directory"
temporary=$(mktemp -d "$directory/.geolite2.XXXXXX")
echo_disabled=false
cleanup() {
    if [ "$echo_disabled" = true ]; then stty echo < /dev/tty; fi
    rm -f "$temporary/credentials.netrc" "$temporary/database.tar.gz" "$temporary/members" "$temporary/database.mmdb"
    rmdir "$temporary"
}
trap cleanup 0
trap 'exit 1' 1 2 15

if [ -z "$credentials" ]; then
    printf 'MaxMind Account ID: ' > /dev/tty
    IFS= read -r account < /dev/tty
    case "$account" in
        ''|*[!0-9]*) echo 'Account ID must be numeric' >&2; exit 1 ;;
    esac
    printf 'MaxMind License Key (hidden): ' > /dev/tty
    stty -echo < /dev/tty
    echo_disabled=true
    IFS= read -r license < /dev/tty
    stty echo < /dev/tty
    echo_disabled=false
    printf '\n' > /dev/tty
    case "$license" in
        ''|*[!a-zA-Z0-9_-]*) echo 'Invalid license key format' >&2; exit 1 ;;
    esac
    credentials="$temporary/credentials.netrc"
    printf 'machine download.maxmind.com\nlogin %s\npassword %s\n' "$account" "$license" > "$credentials"
    unset account license
fi
[ -r "$credentials" ] || { echo 'Cannot read credentials netrc file' >&2; exit 1; }
curl --fail --location --silent --show-error --proto '=https' --proto-redir '=https' \
    --connect-timeout 15 --max-time 300 --retry 2 --max-filesize 268435456 \
    --netrc --netrc-file "$credentials" \
    'https://download.maxmind.com/geoip/databases/GeoLite2-City/download?suffix=tar.gz' \
    --output "$temporary/database.tar.gz"
tar -tzf "$temporary/database.tar.gz" > "$temporary/members"
member=$(LC_ALL=C grep -E '^GeoLite2-City_[0-9]+/GeoLite2-City\.mmdb$' "$temporary/members" || true)
[ -n "$member" ] && [ "$(printf '%s\n' "$member" | wc -l)" -eq 1 ] || {
    echo 'Archive does not contain exactly one GeoLite2-City.mmdb' >&2
    exit 1
}
# Bound extraction to 512 MiB. POSIX ulimit -f uses 512-byte blocks.
(ulimit -f 1048576; tar -xOzf "$temporary/database.tar.gz" "$member" > "$temporary/database.mmdb")
tail -c 131072 "$temporary/database.mmdb" | LC_ALL=C grep -a -q 'MaxMind.com' || {
    echo 'Downloaded file has no MMDB metadata marker' >&2; exit 1;
}
tail -c 131072 "$temporary/database.mmdb" | LC_ALL=C grep -a -q 'GeoLite2-City' || {
    echo 'Downloaded file is not a GeoLite2 City database' >&2; exit 1;
}
chmod 0644 "$temporary/database.mmdb"
mv -f "$temporary/database.mmdb" "$destination"
printf 'Installed GeoLite2 City at %s. Restart stationd to load it.\n' "$destination"
