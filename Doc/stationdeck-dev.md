# StationDeck — browser UI and modules

## Configuration authority and authoring

StationDeck owns configuration authoring through its modules/plugins. A permanent
backend persists published revisions and distributes each target's configuration
via gRPC to daemons or compatibility adapters, independently of the browser.
An explicit FQDN:port takes precedence over automatic discovery; discovery is a
separate mechanism, not native gRPC broadcast. Targets retain their last valid
configuration and report the applied revision. Plugin schemas, permissions and
application capabilities are checked on both sides.

The V1 Icecast service is now called **icecastd** in the target architecture;
the Compose service and internal DNS are now `icecastd`. The `make icecast-*`
commands, image tags and configuration paths are unchanged.
See the [configuration contract](stationdeck-configuration.md).
The separate control backend and configuration module are implemented with a
schema-driven demo consumer and local command adapter hook. See the
[configuration setup and recipe](stationdeck-config-dev.md). Real StationD and
icecastd application adapters must still be provisioned; no station is modified
by enabling this module.
The status-only gateway below remains independent of configuration distribution;
`STATIONDECK_GRPC_ADDR` still selects a StationD 0.5 status endpoint, not the
configuration authority and not the Icecast administration endpoint.

StationDeck runs egui/eframe as WebAssembly in the browser. A container serves
HTML/JS/CSS/WASM and proxies a narrow status API to a separate gRPC gateway.
No X11, display socket, Xvfb or native client installation is required.
StationD 0.5 is reused unchanged.

## Start

On stationdev1, from `/data/dev` (the `V:` share):

```sh
make deck
```

Open [StationDeck](http://stationdev1.lan:18081/).
Use `STATIONDECK_PORT=18082 make deck` for another port, or
`STATIONDECK_BIND=127.0.0.1` to limit HTTP to the development host.
`make deck-down` stops only this project and preserves its gateway token.
No production deployment or Traefik changes are performed.

## First: the test module

The **Test module** is enabled by default. Its **Test station** view displays
simulated status using the same DTO and component as the gRPC module.
**Test tasks** demonstrates a short, nonblocking simulated task, cancellation
and a read-only test role. It never contacts or changes a real station.
Use **Modules** to disable/enable compiled modules. Disabling a module removes
its views, cancels pending work and clears connection credentials.
This is the first compiled-module catalog, not the full dependency resolver.

## Then: the gRPC module

The **gRPC connection** module is included and disabled by default. Enable it
in Modules and open **Station connection**. The browser calls the same-origin
HTTP gateway; only the gateway uses tonic/protobuf and the existing StationD
`Station.Status` RPC. There is no Quit, Shutdown or broadcast-control route.

Configure a fixed upstream reachable from the gateway container:

```sh
STATIONDECK_GRPC_ADDR=http://your-station-host:50051 make deck
```

HTTP and HTTPS targets are supported (HTTPS uses system CA roots). No target
is configured by default: the UI reports this instead of faking a connection.
The browser cannot choose an arbitrary proxy destination.

The gateway requires its own access token for status reads. An init service
creates it once in a private Docker volume, without printing it in logs.
Retrieve it in your own terminal, then paste it into **Gateway access token**:

```sh
make deck-token
```

The token stays in browser memory only, and is cleared on Disconnect or module
disable. It is not saved in localStorage or forwarded as a StationD credential.
This authenticates a fixed read-only gateway capability; user accounts,
station roles, mTLS and per-user authorization are not implemented here.
Do not equate this token with a StationD administrator identity.

## Build and verify in containers

```sh
make deck-build
make deck-check
make deck-smoke
```

Rust 1.99, wasm32-unknown-unknown, egui/eframe 0.33.3, wasm-bindgen 0.2.128.
The JS generator matches the exact wasm-bindgen version. BuildKit caches the
registry and targets. Default workspace members remain unchanged; frontend
checks verify that no daemon/tonic/protobuf dependency enters the WASM graph.

The runtime is unprivileged/read-only with temporary scratch space. No station
configuration, media, SQLite file or Docker socket is mounted. Only the gateway
mounts its token volume read-only; the init service initializes that volume.
The gateway has no published port. The HTTP healthcheck is not a StationD probe.

`deck-check` runs Rust unit tests, formatting and native/WASM Clippy, plus
gateway authorization/route checks. `deck-smoke` uses Chromium and an isolated
server implementing the real StationD gRPC contract. Tests publish no host
port and use an internal network without Internet egress. Their separate
project and token volume are removed on completion. The fixture binary is
feature-gated and excluded from the production gateway build.
Compose 2.24.4+ is needed for the `!reset` test override.

## Browser behavior

- English is the baseline for UI messages and diagnostics.
- Home, Modules and Settings remain available without a station connection.
- Ctrl+Home: Home; Ctrl+,: Settings while the canvas has focus. Browser-reserved
  shortcuts and tab closure are left to the browser.
- Dark/light theme, zoom and enabled modules are stored locally per browser
  origin. Changes are saved immediately. Tokens are never stored there.
- Invalid/future preferences are preserved until explicitly replaced. Storage
  denial is reported; the interface remains usable with temporary preferences.
- Requests are single-flight, bounded by timeout and cancellable. Late responses
  from a disabled/disconnected module are ignored. No mutation is retried.
- JavaScript, WASM and WebGL are required. Initial loading errors have an HTML
  message outside the canvas. Chromium is the initial tested browser; other
  browsers and screen-reader coverage remain to be qualified.

## Extension seam

`stationdeck-ui::DeckModule` declares module metadata, views, rendering and stop.
`ModuleCatalog` checks identifiers/namespaces/duplicates, orders views and owns
activation/removal. `stationdeck/src/assembly.rs` is the only place that knows
the concrete modules. `stationdeck-services` holds DTOs and request generations,
independent of egui, tonic and the daemon. Full manifest parsing, dependency
resolution, typed service injection, role attribution and active subscriptions
remain later work; the example TOML manifests are still design documents.

## Validation on 17 October 2026

Twelve Rust tests pass, including rejected/stale responses, identifier checks,
module shutdown, credential clearing, direct read-only denial, explicit theme,
and gateway authentication/absence of mutation routes. Formatting and native/
WASM Clippy pass; gateway linting excludes generated dependency code.
Chromium tests use the actual protobuf contract over tonic: successful status,
missing/wrong token denial without reaching gRPC, sanitized upstream failure,
cancellation and an empty credential field after module reactivation. Browser
storage is checked to exclude credentials. Theme/storage reload, initial WASM
failure, MIME type and responsive sizing are tested as well.
Screenshots are inspected and kept under `.local/stationdeck-web-qa/` outside Git.
A real station target is intentionally not configured by the automated recipe.
