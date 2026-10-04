# Listener stats V1

Small, opt-in extension to the existing plugin architecture. No core database
migration, new HTTP dependency, public API, dashboard, or external GeoIP service.
The host reads a local GeoLite2 City or DB-IP City Lite MMDB using the maxminddb crate.

## Enable

Build the guest from the repository root:

```sh
cargo build --manifest-path plugins/listener-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
```

Add `listener_snapshots = true` to the existing `[icecast]` section (default:
false). Keep its existing admin credentials and polling intervals. Then add:

```toml
[[plugin]]
name = "listener-stats"
enabled = true
wasm = "plugins/listener-stats-wasm/target/wasm32-unknown-unknown/release/listener_stats_wasm.wasm"
capabilities = ["db", "listener_details", "geoip"]
```

Restart stationd after changing the Icecast configuration. Build/package the
WASM explicitly as with the other example plugins; no binary is in this patch.
The existing host creates `<data>/plugins/listener-stats.db` and applies the
guest's ordered, immutable migrations before loading it. Existing DB size and
query deadline limits apply. A failed batch rolls back completely.

## Event contract

`ListenerSnapshot` is separate from the unchanged `ListenersSampled` count.
It is emitted once per managed mount after its request completes; `at` is
station epoch seconds. Mounts are polled sequentially, deduplicated, then the
sampler sleeps for the configured normal/sleeping interval. This is not a
simultaneous snapshot across mounts. Detailed polling and global audience
polling use independent futures; stopping the sampler cancels both.

```json
{"ListenerSnapshot":{"mount":"/radio.mp3","at":1790000000,"listeners":[{"id":"42","ip":"192.0.2.1","connected_seconds":120,"user_agent":"Player"}]}}
```

- `listeners: []`: successful observation of zero clients.
- `listeners: null`: unknown (HTTP/auth/timeout/oversized/invalid XML, absent
  mount, or inconsistent count). No invented zero or inferred departure.
- An absent event is a gap: delivery remains best-effort; a full plugin queue
  can drop it. Consumers must not infer continuous coverage.
- A listener ID is scoped to a mount and Icecast process. Restarts can reuse it.
- Only plugins declaring `listener_details` receive these events. Detailed
  events are omitted from the core journal. IPs and user agents are transient unless the operator enables the host GeoIP debug capture described below (IPs only).

The HTTP client reuses Basic auth, the three-second whole-request timeout and
the 2 MiB response cap. The parser accepts the capitalized Icecast 2.4 fields
and lowercase/namespaced Icecast 2.5 fields, with at most 10,000 clients per
mount. It rejects duplicates and partial responses. No forwarding header is
trusted: behind a proxy, Icecast may report the proxy IP.

Protocol references:
[Icecast 2.4 admin implementation](https://github.com/xiph/Icecast-Server/blob/v2.4.4/src/admin.c)
and [Icecast 2.5 admin implementation](https://github.com/xiph/Icecast-Server/blob/v2.5.0/src/admin.c).

## GeoLite2 City (MaxMind)

The host accepts MaxMind GeoLite2 City in MMDB format, in addition to legacy
DB-IP City Lite databases. No API key is used by stationd itself, and no HTTP
request is made for listener lookups. Use a MaxMind account with a license key
permitted to download GeoLite2 City.

```sh
# From the dev host; enter Account ID and License Key at the prompts:
docker compose -f /data/dev/stationd/compose.yaml exec -it -u dev station \
  sh /src/scripts/update-geolite2.sh
```

The production bundle installs the same script in `scripts/`; the production
image provides `/usr/local/bin/update-geolite2.sh`. On the production node:

```sh
cd /opt/stationd
docker compose exec -it -u stationd station /usr/local/bin/update-geolite2.sh
```

The license key is hidden during entry and written only to a temporary mode-0600
netrc file removed on exit. It is not passed in process arguments or stored in
stationd.toml. A protected existing netrc may be supplied as the second argument
for non-interactive updates. The script uses HTTPS, authenticated downloads,
archive integrity and edition checks, a 512 MiB extraction limit, and an atomic
rename; failures preserve the installed file. The host fully validates MMDB
on loading. No background update schedule is created.

```toml
[geoip]
database = "./data/geoip/GeoLite2-City.mmdb"
debug_log = "./data/geoip/lookup-debug.jsonl" # optional temporary capture
```

After downloading, rebuild/restart stationd to load the database. Rebuild the
listener-stats WASM once for the updated UI attribution. No plugin database
reset or migration is needed. Existing aggregates
are retained with their original locations: IPs were not stored, so historical
rows cannot be corrected. Diagnostic records now include `database_type`, so
captures from different providers can be distinguished.

CLI outputs credit MaxMind and DB-IP because history can contain both. A UI
using GeoLite2 must include MaxMind attribution: this product includes GeoLite2
data created by MaxMind, available from https://www.maxmind.com.
See [MaxMind database updates](https://dev.maxmind.com/geoip/updating-databases/)
and [GeoLite terms](https://www.maxmind.com/en/geolite/eula).

## Local DB-IP City Lite GeoIP (legacy)

After applying the DB-IP patch, the host reads the same free database bundled
by default with AzuraCast: DB-IP City Lite in MMDB format. Download it on the
host or inside the development container (requires curl and gzip):

```sh
sh scripts/update-dbip.sh ./data/geoip/dbip-city-lite.mmdb
# Optional explicit monthly release:
sh scripts/update-dbip.sh ./data/geoip/dbip-city-lite.mmdb 2026-09
```

Paths must be visible to stationd, and the file readable by its runtime user.
For the development container, run from /src as user dev, using a writable
persistent data directory. Production needs a persistent volume and a path
inside that volume; /src is only the development checkout.

Add this top-level section (outside any [plugin.config] or [plugin.db]):

```toml
[geoip]
database = "./data/geoip/dbip-city-lite.mmdb"
```

Restart stationd after configuring or replacing the file. The host loads it
once into memory (about 122 MiB for the September 2026 release) and shares it
across plugins. No disk or network access happens per listener by default; optional debug capture writes a bounded local file. The configured
file is limited to 512 MiB and must identify itself as GeoLite2-City or DBIP-City-Lite.
An absent/invalid file produces a startup warning and leaves GeoIP unavailable;
it does not stop broadcasting or the listener count collection. A decoding
failure during lookup returns a host error and the guest rejects that batch.

`geoip_lookup` remains an ExtismHost import gated by capability `geoip`:

```json
{"ip":"8.8.8.8"}
```

The reply includes `ok`, `status`, `country`, `region` and `city`:

- `found`: uppercase country code and a nullable city (English name) and region. The region is the first
  administrative subdivision, using its French name when available, else English.
- `not_found`: no usable country or a private/reserved/documentation address;
  location fields are null. IPv4-mapped IPv6 is normalized before lookup.
- `unavailable`: no usable database was loaded; location fields are null.
- `ok:false`: undeclared capability, invalid input or database lookup error.

DB-IP's ZZ (unknown) is not recorded as a real country. The plugin caches
lookups only within one observation; clients sharing an IP still count
individually. The regions extension adds a nullable region to the host reply and migration 2
to the plugin database. Migration 1 remains unchanged and existing rows are
preserved with an unknown region. Historical unavailable buckets cannot be enriched retrospectively
because raw IPs were intentionally not stored.

DB-IP publishes monthly updates. Run the script again when wanted, then
restart stationd; this patch does not create a scheduler. Downloads use HTTPS,
a temporary file, gzip integrity checking and an MMDB marker sanity check.
Failures preserve the installed file; successful replacement uses a rename
on the same filesystem. This is not a cryptographic signature verification.

Data attribution: [IP Geolocation by DB-IP](https://db-ip.com),
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
A future statistics page must display that linked attribution wherever it
uses these results. This patch includes no UI and redistributes no database.
See [DB-IP City Lite](https://db-ip.com/db/download/ip-to-city-lite).

## Temporary IP diagnostics

To compare the IP received from Icecast with the exact host lookup result,
add `debug_log` to the existing top-level section:

```toml
[geoip]
database = "./data/geoip/dbip-city-lite.mmdb"
debug_log = "./data/geoip/lookup-debug.jsonl"
```

Rebuild/restart stationd; no guest rebuild or database migration is needed.
The parent directory must exist and be writable by stationd. Capture is
disabled when `debug_log` is absent. JSONL records include Unix timestamp `at`,
the input `ip`, `database_type`, `database_build_epoch`, and `result` containing `status`,
`country`, `region`, and `city` (or `error` for a lookup failure). These are
lookups, not individual connections: the guest caches each IP within one
snapshot. Mount, client ID and user agent are not included. A missing/unloaded
database cannot produce a capture; check the startup warnings.

```sh
# On the dev host, after returning from an external listening test:
tail -n 50 /data/dev/stationd/data/geoip/lookup-debug.jsonl
```

On Unix the file has mode 0600. It appends across restarts, without rotation
or automatic deletion. At 10 MiB (including earlier runs), or on a write
failure, capture stops with one warning; lookups and statistics continue.
An invalid capture path also leaves GeoIP operational.
To resume a full capture, stop stationd, move the file aside, then restart.
To disable, remove `debug_log` and restart stationd. Delete the diagnostic
file manually when finished; disabling does not erase existing IPs.
Historical aggregate rows cannot be joined back to IPs.

## Storage and queries

`listener_snapshot(mount, at, listeners)` stores concurrent audience, with
NULL for failed collection. `listener_geo` stores counts grouped by status,
country, region and city. Empty location strings mean unknown, not a real country.
Neither table contains IPs, user agents or client IDs. Each event replaces
its `(mount, at)` observation atomically, so replay does not double count.
Two observations for the same mount within one second replace one another.
Rows older than 30 days relative to the current event are deleted on each
write by default. Set [plugin.config] retention_days to an integer from 1 to
365 for another retention period (for example 90 for weekly reports). No background cleanup runs while the plugin/station is stopped, and
SQLite may reuse freed pages without shrinking its file.

```sh
stationctl plugin db listener-stats query "SELECT mount, at, listeners FROM listener_snapshot ORDER BY at DESC LIMIT 100"
stationctl plugin db listener-stats query "SELECT mount, status, country, city, listeners FROM listener_geo WHERE at = (SELECT max(at) FROM listener_geo)"
```

Counts describe concurrent connections at sample time. Summing across samples
does NOT give unique listeners. Exact sessions, connection durations, bot or
relay classification, and dashboards are outside V1.

## Listener commands

After rebuilding stationd/stationctl and the listener-stats WASM, restart the
host and reload the plugin. Migration 2 runs automatically; do not reset the
plugin database. Regions become available on new observations; past IPs were
not stored and old rows cannot be enriched retrospectively.

```sh
stationctl listeners regions
stationctl listeners regions --mount /radio.mp3
stationctl listeners stats --by hour --since 24h
stationctl listeners stats --by day --since 7d
stationctl listeners stats --by week --since 4w
stationctl listeners stats --by day --since 7d --mount /radio.mp3
```

Use `--plugin NAME` for a different plugin declaration name. Both commands
reuse the existing read-only plugin DB RPC. No new service or core DB is added.

`regions` shows the latest retained observation per mount, grouped by country
and administrative region, with its epoch timestamp. Confirmed zero audiences
are hidden; failed collection remains explicitly unknown rather than falling
back to an earlier successful observation.

`stats` shows period, mount, country, region, city, average observed concurrent
listeners, peak, successful observation count, collection failure count and
GeoIP status. Positive groups only are shown. A small positive average is
shown as `<0.01` rather than rounded to zero. The time window accepts hours,
days and weeks (1h to 365d); data availability is limited by retention and the
actual collection start. Changing retention does not recover deleted history.

Buckets use UTC; calendar weeks run Monday through Sunday. Boundary buckets
may be partial. The average is the arithmetic mean over successful snapshots
in the selected window: successful zero samples count in its denominator,
failed collections do not. It is not a time-weighted estimate, a sum of
visitors, or unique listeners. GeoIP-unknown listeners stay in explicit unknown
buckets. Mounts remain separate; observing a connection on multiple mounts
must not be interpreted as multiple unique people. Changing the polling
interval affects sample weighting. No observations are invented for gaps.

For example, to retain 90 days (subject to the existing plugin DB size cap):

```toml
# Under the listener-stats [[plugin]] declaration:
[plugin.config]
retention_days = 90
```

## Checks

```sh
cargo test --lib geoip
# Optional real-database check after downloading:
STATIOND_TEST_DBIP=./data/geoip/dbip-city-lite.mmdb cargo test --lib reads_real_dbip -- --ignored
# Optional host + WASM + SQLite roundtrip (build the guest first):
STATIOND_TEST_DBIP=./data/geoip/dbip-city-lite.mmdb STATIOND_TEST_LISTENER_WASM=plugins/listener-stats-wasm/target/wasm32-unknown-unknown/release/listener_stats_wasm.wasm cargo test --lib real_geoip_wasm -- --ignored
cargo test --lib listener_snapshot
cargo test --lib listener_stats_tests
cargo test --lib icecast_listener_tests
cargo test --test listener_stats
cargo test --bin stationctl listeners::tests
cargo build --manifest-path plugins/listener-stats-wasm/Cargo.toml --release --target wasm32-unknown-unknown
```

These cover XML variants and failures, mount escaping, HTTP authentication,
independent audience polling, capability gates, GeoIP input validation,
aggregation, confined DB migrations, replay, rollback and retention.
