# stationd

A self-hosted webradio automation daemon written in Rust. It is a
from-scratch rewrite of the *idea* of [AzuraCast](https://www.azuracast.com/),
built for one use case: **a single, self-hosted station**, driven entirely
from the command line.

stationd owns the station: the media library, the playlists, the broadcast
schedule (the *grid*) and the live air. It drives
[Liquidsoap](https://www.liquidsoap.info/) for playout and
[Icecast](https://icecast.org/) for streaming. It does not try to match
AzuraCast feature for feature.

> **Status:** in active development, and it already airs. The scheduler,
> playlists, media library, plugins and Liquidsoap integration run end to end
> on a real station (Liquidsoap 2.4 → Icecast 2.5). The public HTTP API and
> the web UI do not exist yet. See [Roadmap](#roadmap).

---

## Why

AzuraCast works, but it is built for many kinds of installs: multi-tenant,
shared hosting, many contributors. Most of its weight comes from that job
(PHP-FPM, MariaDB, Redis, Symfony's DI/event bus/Messenger, a plugin and
theme system), not from running a radio station.

stationd keeps only what one station needs:

- **Rust** — strong typing and explicit `Result`s. No silent failures: an
  error is reported, never swallowed. This is the lesson of a metadata write
  that failed without any log on the PHP fork.
- **SQLite embedded in the daemon** — each node is autonomous. There is no
  database server to run.
- **TOML files are the source of truth** for playlists and the grid. They
  can be diffed, reviewed and put under version control. The database is a
  view that can be rebuilt from them.
- **A complete CLI** — everything the daemon can do can be done from
  `stationctl`, with no GUI needed (think `git` or `docker`).

---

## Architecture

```
 browser / public site
        │  REST/JSON over HTTPS            (planned: `api`, Axum BFF)
        ▼
      api  ─────────────┐
        │ gRPC          │ gRPC
        ▼               ▼
    stationd  ◄──── stationctl (CLI, local, trusted)
        │
        ├── SQLite (station state, media index, playback history)
        ├── Liquidsoap ── loopback HTTP bridge (pull) + control socket
        └── Icecast 2.5 (via Liquidsoap)
```

Rules that shape the code:

| Principle | Meaning |
|---|---|
| **Single writer** | Only `stationd` drives Liquidsoap/Icecast and writes SQLite. `stationd` is never exposed publicly. |
| **CLI-first** | Every feature is designed in the gRPC contract first. `stationctl` calls stationd over gRPC, like the planned `api`. `api` is a pure translator and holds no business logic of its own. |
| **File-first** | Playlist and grid TOML files are canonical. `apply`/`sync` rebuilds the indexed view (family A); playback state such as cursors, cooldowns and history survives it (family B). |
| **No silent failures** | Unknown references are errors, `deny_unknown_fields` everywhere, an empty pool falls through to a lower priority (never dead air), a dropped override is logged. |
| **Pure core** | The grid resolver is a pure function of `(now, grid, state)`, with no clock and no I/O. It can therefore be tested and simulated without waiting for the wall clock, including across DST changes. |

---

## Concepts

### Media library

`stationctl library scan` walks the media root (local disk or NFS). It reads
tags and durations with [`lofty`](https://crates.io/crates/lofty) and
reconciles the index. Files that vanish are marked unavailable, not deleted.
Genres are compared case-insensitively, including accented characters.
Custom tags (`TXXX:…`, Vorbis/APE, MP4 freeform) are passed to plugins at
scan time.

### Playlists

One TOML file per playlist. A playlist is **always resolved at play time**
against the library; a fixed tracklist is just a special case.

| Mode | What it plays |
|---|---|
| `dynamic` | Media matching filters (`path`, `title`, `artist`, `album`, `year`, `duration`, `genre`). |
| `static` | An explicit list of files. |
| `remote` | A remote stream URL, relayed live for as long as the rule that names it wins (meant for `day_part` / `base_rotation`; inside a group, give it a `runtime`). |
| `queue` | A runtime buffer filled by listener requests or DJ injection (`stationctl queue push`), FIFO or LIFO. |
| `group` | Other playlists composed with a strategy: `sequence`, `shuffle`, `weighted`, `rotate`. Groups can nest; cycles are detected. |

Orders are `shuffle`, `sequential`, `newest` and `oldest` (cursor-based).
Group members have quotas: `take = N` tracks, or `runtime = "20m"` of wall
time. Playback constraints are enforced at play time:
`no_same_track_within`, `no_same_artist_within` and `unplayed_only` (play
each episode once).

```toml
# playlist/music.toml
name = "music"

[selection]
mode = "dynamic"
order = "shuffle"

[[selection.filter]]
field = "genre"
op = "has_any"
value = ["rock", "pop"]

[broadcast.constraints]
no_same_artist_within = "30m"
```

```toml
# playlist/evening-show.toml
name = "evening-show"

[selection]
mode = "group"
strategy = "sequence"
members = [
  { ref = "show-intro", take = 1 },
  { ref = "music", runtime = "45m" },
  { ref = "show-outro", take = 1 },
]
```

### The grid (scheduler)

`grid.toml` says what plays when, using four kinds of rules — plus `live`
slots for DJs. When several rules apply, the highest priority wins:

**override › `at_clock` hard › `at_clock` soft › `every` › `day_part` ›
`base_rotation` › fallback**

| Kind | Meaning |
|---|---|
| `base_rotation` | The floor: covers any time nothing else does. |
| `day_part` | A time window (`start`/`end`, optional `days`). Windows crossing midnight are supported. Without `end`, the day part is **open**: it runs until the next start of another day part (a programme grid where each show lasts until the next one); its `days` apply to the day it started. |
| `at_clock` | A fixed time (`at = "08:00"`, or `every_minutes`). `soft`: at the next track boundary. `hard`: cut in on the second (the current track is interrupted), and outranks everything except overrides; a hard mark that cannot be cut (station paused, more than 10 s late) airs soft instead. Optional `expiry`. |
| `every` | A cooldown: every N tracks (`min_tracks`) or every elapsed duration. |
| `live` | A DJ slot (`dj`, `start`, optional `days`; no `end`, no playlist): the DJ may connect from `start` until the next `day_part` or `live` starts. It selects nothing: the programme underneath goes on until the DJ connects. See *Live DJ*. |

Most boundaries are *soft*: stationd never cuts a track; it changes what
comes next. A source whose pool is empty falls through to the next priority,
down to the floor. Times are handled as UTC epochs internally and converted
to the station timezone (e.g. `Europe/Paris`) only for input and display.

```toml
schema_version = 1

[[rule]]                     # the floor: always covers the air
id = "floor"
kind = "base_rotation"
playlist_ref = "music"

[[rule]]                     # a weekday evening show
id = "evening"
kind = "day_part"
playlist_ref = "evening-show"
start = "20:00"
end = "21:00"
days = ["mon", "tue", "wed", "thu", "fri"]

[[rule]]                     # news at 08:00, highest grid priority
id = "news"
kind = "at_clock"
playlist_ref = "flash-info"
at = "08:00"
mode = "hard"
expiry = "2m"

[[rule]]                     # a station ID every 4 tracks at least
id = "station-id"
kind = "every"
playlist_ref = "jingles"
min_tracks = 4
```

`stationctl schedule preview` projects the grid over the next hours without
waiting for the clock, in UTC and in local time (useful across DST changes).
`schedule check` tells you whether each rule has enough media behind it.

### Broadcast control and overrides

- **Station state:** `running`, `paused`, `stopped`, or `draining` (stop at
  the next track boundary once there are zero listeners). The state survives
  restarts.
  - `pause` freezes the current track and airs background noise; `resume`
    continues the track where it stopped.
  - `stop` is graceful: it takes effect at the end of the current track.
  - `next` skips to the next track now.
- **Overrides** push a media or a playlist ahead of the grid, with an
  optional expiry and a track count.
  - `soft` airs at the next track boundary.
  - `hard` cuts the current track now.

### Live DJ

A DJ streams to a harbor input of Liquidsoap (`[live]`, port 8005, mount
`/live` by default) with any Icecast source client (butt, Mixxx…).

- **Who:** the DJ file (`[live] djs_path`, kept apart from `stationd.toml`)
  lists the DJs with an argon2 hash of their password
  (`stationctl dj hash`). It is re-read at every login: no restart needed.
  A software that cannot set a user name logs in as `source` with
  `dj,password` as the password. A DJ password contains neither `:` nor `,`.
- **When:** a `live` rule of the grid. Outside the window, the login is
  refused.
- **On air:** stationd decides every login; the live takes the air with a
  short fade (`fade`, 1.5 s), above everything — hard overrides and hard
  `at_clock` marks are held and air soft after the live.
- **Back to the programme:** when the DJ disconnects, or after
  `silence_timeout` seconds of silence (30 s). A new track is chosen at that
  moment (the one the live interrupted is dropped). A DJ cut for silence —
  or by `stationctl live kick` — is refused until the end of the slot.
- `stationctl live status` shows who is on air, the last live, the refused
  DJs and the last refused login. Plugins receive `LiveStarted` /
  `LiveEnded`.

### Plugins

Plugins add features; they do not process audio.

- **Native Rust plugins** or **WebAssembly guests** (via
  [extism](https://extism.org/)). A guest is a sandboxed `.wasm` that cannot
  crash the daemon.
- **Hooks:**
  - `on_event`: track resolved, library scanned, broadcast state changed,
    listeners sampled, …
  - `filter_pool`: remove candidates before a track is picked.
  - `on_scan`: turn custom tags into genres.
- **Host surface:** each plugin declares capabilities, e.g. `control`
  (stop / pause the station) or `push_override`.
- **Examples:** native `logger`, `blacklist` and `stop-when-idle` (in
  `src/plugin.rs`); WASM guests in `plugins/`: `blacklist-wasm`,
  `require-title-wasm`, `stop-when-idle-wasm`, `custom-tags-wasm`.

The contracts are in `Doc/plugin-{events,hooks,host}.md`.

### Liquidsoap integration

In this design Liquidsoap decides **nothing**. stationd writes the `.liq`
script at start-up from the `[liquidsoap]` config section. Liquidsoap runs
under its own service, so restarting stationd never cuts the air.

- **Pull:** Liquidsoap asks `POST /ls/v1/next` on a loopback HTTP bridge
  (shared token) for every track — only a few seconds before the current one
  ends, so the choice is made at the real air time — and reports what really
  starts airing on `POST /ls/v1/track`.
- **Control socket:** stationd sends `pause`, `resume`, `skip`, `flush` and
  `interrupt` to Liquidsoap for immediate actions.
- **Air chain:** stationd's tracks, then background noise while the station
  is halted, then a safety fallback file when there is nothing to air (or
  stationd is unreachable). A queue for hard overrides sits on top, and the
  live DJ harbor (`[live]`) above everything.
- **Watching the air:** `stationctl ls status` shows what is on air, what is
  queued next, the state of the air and the health of the control socket.

Details: [`Doc/liquidsoap.md`](Doc/liquidsoap.md).

### Icecast integration

stationd only **reads** Icecast, from its admin API (`/admin/stats`, admin
credentials in `[icecast]`, plain HTTP straight to Icecast, never through the
reverse proxy), every `poll_interval` seconds:

- **Audience:** the listeners of the mounts stationd feeds are summed and
  published as `ListenersSampled`, which drives `stop-when-idle`. A failed
  read (Icecast down, bad credentials, a mount without source) makes the
  audience *unknown*, never 0: a draining station keeps playing.
- **Mount health:** `stationctl icecast status` shows, per mount, whether a
  source is connected and since when, the announced and the measured bitrate
  (averaged over a minute; a stalled source reads 0), the listeners and the
  title Icecast shows.
- **Icecast's config** (optional, `[icecast.server]`): stationd writes
  `icecast.xml` like it writes the `.liq` — one mount per output with its own
  source password and UTF-8 titles, no global source password (only our
  mounts can be fed), admin credentials, trusted reverse proxies for
  `X-Forwarded-For` (Icecast 2.5). Icecast runs as its own service.

---

## Getting started

### Prerequisites

- A Linux host with **Docker** and the **compose** plugin. Everything else —
  Rust (stable) and the `wasm32-unknown-unknown` target, `protoc`,
  Liquidsoap **2.4**, Icecast **2.5** — ships in the development image
  (`docker/Dockerfile.dev`, Ubuntu 26.04 packages).
- The media library mounted on the host (NFS in our setup).

### Configure

Copy `stationd.example.toml` to `stationd.toml` (kept out of git) and adjust
it:

```toml
[station]
name = "My Radio"
timezone = "Europe/Paris"          # IANA zone; the grid resolves civil times against it

[server]
grpc_bind = "127.0.0.1:50051"

[database]
path = "./data/stationd.db"

[media]
library_path = "/mnt/nfs/radio"

[playlist]
path = "./playlist"

[liquidsoap]
script_path    = "./data/station.liq"
api_token      = "change-me"                 # ASCII shared secret
control_socket = "/run/stationd/liquidsoap.sock"  # created by the image
fallback_path  = "./radio/error.mp3"         # safety net (default: the production image's)
halted_path    = "./radio/bruit.mp3"         # looped while paused/stopped (idem)

[[liquidsoap.output]]
host     = "127.0.0.1"
port     = 8000
password = "icecast-source-password"
mount    = "/radio.mp3"
bitrate  = 192

[icecast]                                    # optional: audience + mount health
admin_url      = "http://127.0.0.1:8000"     # Icecast itself, not the proxy
admin_password = "icecast-admin-password"    # admin, not the source password

[icecast.server]                             # optional: stationd writes icecast.xml
trusted_proxies = ["192.168.1.94"]           # reverse proxy (X-Forwarded-For)
```

### Run (Docker)

stationd, Icecast and Liquidsoap run in one container, supervised by
[s6-overlay](https://github.com/just-containers/s6-overlay)
(`docker/Dockerfile.dev`, `compose.yaml`). The repository is bind-mounted at
`/src` and built inside the container; `target/` and the cargo registry live
in named volumes. The container uses the **host network**: Icecast listens
on the host's port 8000, gRPC (`127.0.0.1:50051`) and the Liquidsoap bridge
(`127.0.0.1:8081`) stay on the host's loopback.

Start-up order (`docker/rootfs/etc/s6-overlay/s6-rc.d`):

```
init-perms (oneshot) → stationd ──(ready: answers gRPC)──→ icecast → liquidsoap
```

stationd writes `data/station.liq` and `data/icecast.xml` before it opens its
gRPC port, so Icecast and Liquidsoap only start on up-to-date files.
Restarting stationd does not restart them: the station stays on air
(Liquidsoap plays the fallback meanwhile).

Users inside the image — same shared-group model as a native install:

| User | Groups | Why |
|---|---|---|
| `dev` (host UID/GID) | `stationd` | runs stationd and cargo; files on the mounted repository stay yours |
| `icecast2` | + `stationd` | reads the generated `icecast.xml` (`0640`, `file_group = "stationd"`) |
| `liquidsoap` | primary `stationd`, + media group (`MEDIA_GID`) | control socket (`0660`); reads the fallback, the noise and the media |

`init-perms` creates `/run/stationd` (tmpfs, `root:stationd`, `2770`) for the
control socket: set `[liquidsoap] control_socket =
"/run/stationd/liquidsoap.sock"`.

**Once, on the host:**

```sh
# the shared group (its GID is baked into the image)
getent group stationd || sudo groupadd --system stationd
# build arguments: host identity, shared group, group owning the media
printf 'DEV_UID=%s\nDEV_GID=%s\nSTATIOND_GID=%s\nMEDIA_GID=%s\n' \
  "$(id -u)" "$(id -g)" "$(getent group stationd | cut -d: -f3)" \
  "$(stat -c %g /mnt/nfs/radio)" > .env
# the container uses the host network: stop any native service first
sudo systemctl disable --now icecast2 icecast-stationd liquidsoap-stationd
```

**Build and start:**

```sh
docker compose build
docker compose up -d
docker compose exec -u dev station cargo build          # stationd + stationctl
docker compose logs -f station
```

On the very first start the binary does not exist yet: stationd's service
waits (`… absent: run cargo build`) and Icecast/Liquidsoap are held until
stationd answers — nothing else to do once `cargo build` has run. After a
change to `docker/` or `.env`: `docker compose build` then
`docker compose up -d --force-recreate`.

**Day to day:**

```sh
# rebuild + restart stationd only (the air is not cut)
docker compose exec -u dev station cargo build
docker compose exec station s6-svc -r /run/service/stationd

# after a change to [liquidsoap], [icecast], [icecast.server] or the outputs:
# restart stationd (rewrites the files), then the consumer
docker compose exec station s6-svc -r /run/service/icecast
docker compose exec station s6-svc -r /run/service/liquidsoap

# tests, plugins, TUI
docker compose exec -u dev station cargo test
docker compose exec -u dev -w /src/plugins/custom-tags-wasm station \
  cargo build --release --target wasm32-unknown-unknown
docker compose exec -u dev station cargo build --features tui
docker compose exec -it -u dev station ./target/debug/stationd-tui
```

**CLI** — an alias on the host keeps `stationctl` in step with the daemon:

```sh
alias stationctl='docker compose -f /data/dev/stationd/compose.yaml exec -u dev station /src/target/debug/stationctl'
stationctl library scan
stationctl playlist sync
stationctl schedule apply grid.toml
stationctl ls status
stationctl icecast status
```

**Listening:** `http://<host>:8000/radio.mp3` (the mount of
`[[liquidsoap.output]]`), directly or through the reverse proxy.

**Pitfalls:**
- Files Liquidsoap reads (`fallback_path`, `halted_path`, the media) must be
  readable by `liquidsoap`: through the media group (`MEDIA_GID`) or
  world-readable. A file in `0660 you:you` outside that group fails with
  *"Infallible source.dynamic … was not able to prepare source"*. stationd
  refuses to start when `fallback_path` / `halted_path` is missing, empty or
  unreadable, and warns when it is not world-readable. `api_token` must be
  printable ASCII.
- `COPY` carries the directory modes of the build context into the image
  (a restrictive `docker/rootfs/etc` once made `/etc` unreadable for every
  non-root user); the Dockerfile normalises them after the copy.
- The development image carries the toolchain; nodes run the production
  image below.

### Package & deploy

Production binaries are **compiled on the development machine** (inside the
development container: same Ubuntu, same glibc) and shipped in a runtime-only
image — no toolchain on the nodes. `docker/Dockerfile.prod`: Ubuntu 26.04 +
Icecast 2.5 + Liquidsoap + s6-overlay + `stationd` / `stationctl` + the WASM
plugins (`/usr/lib/stationd/plugins/`). Same s6 services as the development
image (`docker/rootfs`).

**Package** (on the development machine, development container running):

```sh
docker/package.sh                 # clean git tree required; --allow-dirty otherwise
# → dist/stationd-<version>-<rev>.tar
#   (image + compose.yaml + install.sh + stationd.example.toml + examples/ + SHA256SUMS)
```

It builds `stationd` / `stationctl` (`--release --locked`) and every
`plugins/*` crate (`wasm32-unknown-unknown`), builds the image
`stationd:<version>-<rev>`, checks it (shared libraries, Liquidsoap, Icecast,
plugins) and saves it.

**Deploy** (node: Docker from `docker-ce` + compose plugin, media mounted).
Everything lives in **one directory**, `/opt/stationd` by default:

```sh
scp dist/stationd-<tag>.tar <node>:/tmp/
ssh <node>
cd /tmp && tar xf stationd-<tag>.tar
sudo stationd-<tag>/install.sh            # [--dir /opt/stationd] [--media /mnt/nfs/radio]
```

```
/opt/stationd/                     mounted as /var/lib/stationd (stationd's working dir)
  compose.yaml  .env               installed by install.sh
  stationd.example.toml            installed by install.sh (reference)
  examples/                        installed by install.sh: sample playlists + grid (refreshed each run)
  stationd.toml  grid.toml …       yours — never touched by install.sh
  playlist/  radio/                playlists; your own fallback / noise files (optional)
  data/                            written by stationd (database, station.liq, icecast.xml)
```

`install.sh` loads the image, creates the system user `stationd` (owner of
the directory) and, the first time, `.env` (image tag, `stationd` UID/GID,
media path and its group — applied in the container at start-up). It starts
the station when `stationd.toml` exists; otherwise it stops there.

Permissions: the account running `sudo` joins the group `stationd` (log in
again once): `stationd.toml`, `grid.toml`, `playlist/` and `radio/` are edited
without `sudo`. The shared directories are `2770 stationd:stationd` (setgid:
what you create belongs to the group) and stationd writes with `umask 007`,
so the files it writes stay editable by the group; `data/` is `0750` (the
group creates and deletes nothing there). `install.sh` re-applies these
permissions on every run.

First install: write `stationd.toml` from `stationd.example.toml` — relative
paths resolve against the directory (`./playlist`…), `[media] library_path`
= the `--media` path, WASM plugins at
`wasm = "/usr/lib/stationd/plugins/<crate>.wasm"`,
`control_socket = "/run/stationd/liquidsoap.sock"` — then
`cd /opt/stationd && docker compose up -d`. The fallback and the background
noise ship in the image (`/usr/share/stationd/error.mp3`, `bruit.mp3`, from
the repository's `radio/`): leave `fallback_path` / `halted_path` out, or
point them at your own files in `radio/`. The only name to set is
`[station] name` (Icecast's `<location>`, default stream name); public
listen URLs are the reverse proxy's.

Update: same three commands with the new bundle (only `STATIOND_VERSION`
changes in `.env`). Roll back: put the previous version back in `.env`, then
`docker compose up -d` (older images stay loaded).

```sh
cd /opt/stationd
docker compose logs -f
docker compose exec -u stationd station stationctl status
```

---

## CLI overview

| Command | What it does |
|---|---|
| `stationctl status` / `quit` | Daemon status / clean shutdown |
| `library scan` \| `list` \| `genres` | Scan the media root, list the index (`--genre`), genre inventory |
| `playlist add` \| `sync` \| `list` | Register or reconcile playlist TOML files |
| `schedule validate` \| `apply` \| `export` \| `list` | Manage the grid |
| `schedule preview` \| `check` | Project the grid over time; check each rule has enough media |
| `station state` \| `pause` \| `resume` \| `stop` \| `stop-when-idle` \| `next` | Broadcast control |
| `override push` \| `list` \| `clear` | Content ahead of the grid (`--media`/`--playlist`, `--hard`, `--expiry`, `--tracks`) |
| `queue push` | Feed a `queue` playlist (listener request / DJ injection) |
| `plugin list` \| `start` \| `stop` \| `restart` \| `reload` | Plugin lifecycle |
| `ls render` \| `status` | Generated Liquidsoap script; bridge / air status |
| `icecast status` \| `render` | Audience and health of our mounts; generated `icecast.xml` |
| `live status` \| `kick` | Live DJ on air, last live, refused DJs; end the live now |
| `dj hash` | Hash a DJ password for the DJ file (read from stdin, not echoed) |
| `clock set` \| `show` \| `reset` | Freeze the station clock (testing) |
| `debug listeners <n>` | Inject a listener sample (overwritten by the next Icecast sample) |

`stationctl --addr http://host:port …` targets another daemon.

---

## Repository layout

```
src/
  main.rs            daemon: config, gRPC server, Liquidsoap bridge, shutdown
  bin/stationctl.rs  the CLI
  resolver.rs        pure grid resolver (rule kinds, priorities, live windows)
  clock.rs           epoch ↔ civil time, DST (jiff)
  grid_*.rs          grid grammar, index (family A), playback state (family B), engine
  playlist.rs        playlist model, parsing, validation
  selection.rs       playlist_ref → concrete media (filters, orders, groups, constraints)
  media*.rs          library scanner (lofty) and index
  station_control.rs broadcast state machine, override queue, manual clock
  plugin*.rs         plugin host (native + WASM/extism)
  ls_script.rs       Liquidsoap script generator
  ls_bridge.rs       loopback HTTP bridge (Liquidsoap → stationd)
  ls_control.rs      control socket (stationd → Liquidsoap)
  live.rs            live DJs: DJ file, login decision, live session
  icecast.rs         Icecast admin reader: audience sampling, mount health
  icecast_xml.rs     icecast.xml generator ([icecast.server])
  *_grpc.rs          thin gRPC translators
proto/               gRPC contracts (station, schedule, library, playlist, plugin, broadcast, liquidsoap, icecast, live)
migrations/          SQLite migrations (embedded with sqlx)
plugins/             example WASM guest plugins (separate crates)
docker/              images (Dockerfile.dev, Dockerfile.prod), s6-overlay services (rootfs/),
                     packager (package.sh) and node files (prod/: compose.yaml, install.sh)
compose.yaml         development container (repository mounted at /src)
examples/            sample playlists (one per mode) and grid, checked by tests/examples.rs
Doc/                 architecture and design decisions (mostly in French)
ETAT.md              development log / hand-over notes (French)
```

---

## Roadmap

**Liquidsoap, step 4 — end of track:**
- `TrackStarted` / `TrackFinished` events for plugins;
- exact track counting for `every` rules.

**Icecast:**
- listeners per mount in `ListenersSampled`;

**Later:**
- the public `api` layer (Axum, REST/JSON) and a web admin UI (server-side
  templates + htmx);
- roles and permissions (a fixed set per station);
- mTLS on the internal gRPC channel;
- live DJ: metadata sent by the DJ to Icecast, recording of the live;
- per-track cue points and loudness normalisation.

## License

Not specified yet.
