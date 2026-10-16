#!/bin/sh
set -eu
umask 077
station-icecast-config render /etc/stationd/icecast.toml /run/secrets /tmp/icecast.xml
exec icecast2 -c /tmp/icecast.xml

