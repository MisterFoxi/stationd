# StationDeck — browser UI and modules

## Icecast stream supervision through an autonomous plugin

The standalone [icecastd-deck plugin](../plugins/icecastd-deck/README.md) consumes
an injected typed service. Its manifest, grants, binding, lifecycle and view pass
through the same registry as other modules. The separate `stationdeck-icecast`
provider connects to the Deck gateway; the gateway calls IcecastAdmin.GetStatus.
No StationD daemon or StationD status endpoint is used for this workflow.

Rebuild with `make deck-daemon-build`, then start with `make deck-daemons`
(which initializes missing credentials). Services use the shared
[discovery protocol](stationdeck-service-discovery.md). Then enable Icecast provider and Icecast streams plugin in Modules. Set the Deck gateway token (`make deck-token`) in **Tokens**, connect in Icecast connection, then open Icecast streams. The upstream admin token stays on the server. This does not
replace configuration authoring; stream supervision and publishing configuration
are separate contributions.

The current plugin is compiled into the trusted V1 assembly, not dynamically
loaded code. The isolated `make deck-smoke` recipe includes a real protobuf
IcecastAdmin fixture and verifies plugin blocking, typed injection, stream data,
upstream failure, credential cleanup and explicit stream management commands.
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
its views, cancels pending work and clears private runtime data. Saved tokens remain in Tokens.
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
Retrieve it in your own terminal, then paste it into **Tokens → Deck gateway**:

```sh
make deck-token
```

The token is saved for this origin in the separate `stationdeck.tokens.v1` browser localStorage entry and shared by the StationD and Icecast clients. Disconnect/disable cancels requests and clears private runtime data, while retaining the saved token. **Forget** in Tokens removes it and invalidates active clients. It is never forwarded as a StationD credential.
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
The isolated smoke image enables the `ui-test` feature. It exposes only current
control rectangles and module lifecycle states as `data-qa-*` canvas attributes.
Playwright uses these read-only markers to click actual egui controls; no module
action or credential is exposed through a test API. The normal image has no DOM
markers. The smoke recipe accounts for demo + overview enabled by default and
checks provider binding, required-consumer blocking and credential cleanup.

Compose 2.24.4+ is needed for the `!reset` test override.

## Plugin workspace

The shell displays one floating egui window per available plugin. A plugin with
multiple contributions uses tabs inside its window. The launcher opens a window,
selects its view and raises it. Closing a window only hides its UI: the module and
its in-flight work remain active. Disable the module in Modules to stop it.
Unavailable or unauthorized contributions are still filtered by the registry.

Windows can be moved, resized, collapsed and pinned (movement/resize lock).
Arrange windows resets geometry into columns, using one column on smaller
viewports. Windows stay constrained to the workspace, below the permanent status
strip and above the footer. Settings and Modules are separate host windows.

Open state, selected tab, pin, position and content size are saved alongside
existing browser preferences; old V1 preferences migrate through default fields.
Credentials and remote snapshots are not included. Invalid saved preferences are
preserved until explicitly replaced, as before.

The permanent status strip calls the optional read-only `DeckModule::summary`
contribution for running modules, even when their windows are hidden. Station
overview and Icecast report actual service snapshots (explicitly labelled as
snapshots), pending reads or unavailability. The strip does not initiate polling
or turn a failed read into zero listeners. A module's summary must use its usual
revocable service handles and must not perform network work on the UI thread.

## Browser behavior

- English is the baseline for UI messages and diagnostics.
- Home, Modules and Settings remain available without a station connection.
- Ctrl+Home: Home; Ctrl+,: Settings while the canvas has focus. Browser-reserved
  shortcuts and tab closure are left to the browser.
- Dark/light theme, zoom and enabled modules are stored locally per browser
  origin. Changes are saved immediately. Tokens are saved separately in Tokens.
- Invalid/future preferences are preserved until explicitly replaced. Storage
  denial is reported; the interface remains usable with temporary preferences.
- Requests are single-flight, bounded by timeout and cancellable. Late responses
  from a disabled/disconnected module are ignored. No mutation is retried.
- JavaScript, WASM and WebGL are required. Initial loading errors have an HTML
  message outside the canvas. Chromium is the initial tested browser; other
  browsers and screen-reader coverage remain to be qualified.

## Extension seam

`stationdeck-services::manifest` parses the closed V1 TOML schema at runtime.
The bundled manifests live in `Doc/stationdeck/` and select compiled constructors;
there is no dynamic code loading. `stationdeck-services::registry` has no egui,
tonic or daemon dependency. It resolves SemVer module/service requirements,
optional dependencies, cycles and explicit per-consumer provider bindings.
Invalid modules retain individual diagnostics; unrelated modules can run.

Providers initialize/start before consumers, with ASCII module-id tie breaking.
Publication is atomic after successful startup. Failure removes services and
views, revokes retained handles, stops required consumers in reverse order and
blocks them with a dependency chain. Optional consumers receive a dependency
change notification. A failed provider requires an explicit retry. Disconnected
gRPC remains running and its service reports idle/error until a successful read.

`DeckModule` adds UI rendering to lifecycle hooks through an adapter. `ModuleCatalog`
filters contributions by running state and current permissions. Module settings
show state/diagnostics and offer an explicit retry. Activation intent is saved
independently of availability: blocked modules remain in preferences.

`assembly.rs` owns local grants and the typed service association. Role definitions
never grant permissions automatically. The typed `OverviewService` is provided
by demo or gRPC and injected into the same `OverviewModule`. Choose its provider
in Modules, then enable that provider and Station overview. Both providers may
coexist. Fresh preferences enable demo + overview. Rebinding restarts only the
consumer and required dependents; configuration credentials/drafts stay available.
Disabling the provider blocks the required consumer and removes its view.

Service handles recheck provider and consumer leases on every call. Policy changes
revoke old contexts; stopping cancels in-flight operations and clears private runtime data. Saved credentials are removed explicitly in Tokens.
Network grants name fixed logical profiles, not arbitrary URLs. Local grants do
not confer remote identity, server roles, or authoring permissions: the existing
configuration backend continues enforcing its own read/edit/publish scopes.

Lifecycle hooks must be nonblocking on the UI thread. Existing stop hooks abort
requests synchronously and invalidate late responses; they do not wait for remote
work. The registry cannot preempt a blocking hook or isolate a panic in trusted
compiled code. Awaiting future asynchronous resource cleanup with a global five
second deadline is not implemented by this synchronous seam.

## Registry validation pending (18 October 2026)

Tests have been added, **not run**. Compilation and browser validation are also
left to the user. Suggested targeted command in the existing Rust/container setup:

```sh
cargo test --locked -p stationdeck-services -p stationdeck-ui -p stationdeck-demo -p stationdeck-grpc -p stationdeck-config-ui -p stationdeck
```

Added cases cover bundled manifests, unknown fields, namespace/reference errors,
cycles including optional edges, missing/incompatible dependencies, duplicates,
startup failure/cleanup, dependency chains, ambiguous providers, typed interface
mismatch, rebinding, optional provider loss, required provider failure/retry,
and retained handle revocation. An assembly test uses the common consumer with
both providers enabled and switches its binding. Rerun the browser recipe too.

## Validation on 17 October 2026

Twelve Rust tests pass, including rejected/stale responses, identifier checks,
module shutdown, credential clearing, direct read-only denial, explicit theme,
and gateway authentication/absence of mutation routes. Formatting and native/
WASM Clippy pass; gateway linting excludes generated dependency code.
Chromium tests use the actual protobuf contract over tonic: successful status,
missing/wrong token denial without reaching gRPC, sanitized upstream failure,
cancellation and an empty credential field after module reactivation. Browser
layout storage is checked to exclude credentials; dedicated token persistence, restoration and explicit deletion are tested. Theme/storage reload, initial WASM
failure, MIME type and responsive sizing are tested as well.
Screenshots are inspected and kept under `.local/stationdeck-web-qa/` outside Git.
A real station target is intentionally not configured by the automated recipe.

### Operational indicators

The launcher includes disabled and blocked plugins, with painted status LEDs and
text labels. Green means available, amber means pending/blocked/degraded, red
means failed, and an outlined black LED means inactive/not connected. Window
visibility and module lifecycle are independent from this operational status.
`DeckModule::health` defaults to the registry's Running label; transport modules
and consumers override it with their actual request/service state. No successful
connection is inferred from enabling a module. The permanent strip uses shared
compact instruments for snapshots and known values, and removes values after an
unavailable read instead of presenting a fabricated zero audience.

The sidebar uses one compact entry per plugin: name and overall status LED only.
Detailed state remains in the tooltip and plugin window; multiple contributions
are selected using the window's tabs. Reopening preserves the selected tab.

## Tokens

The host-owned Tokens window is always available, including with all plugins
disabled. It owns the masked credential fields, optional Show, automatic saving,
per-token Forget and Forget all. Deck gateway is shared by StationD and Icecast;
Configuration author and Icecast control are separate credentials. Storage is local to this browser
profile and origin, using localStorage; it is not an encrypted vault.
Layout preferences, drafts and token storage remain separate.

Saved values are versioned and bounded. Invalid/future data is preserved until
explicit replacement; denied writes/removals are reported with a retry action.
Disconnecting, disabling or reloading does not delete stored credentials, and no
network connection is started automatically. Changing/forgetting a token cancels
pending requests and clears previous snapshots/private drafts on the next host
tick, including for closed windows. Providers receive only their credential slot.
## Icecast stream management

Open **Icecast streams → Management** after connecting the provider. Select a
configured mount to update its title, list its listeners, disconnect one listener
or disconnect its source. Disconnections have a targeted confirmation and Cancel.
A source encoder may reconnect automatically. The Stream router section loads
GetRoute and switches between the inputs returned by the administration service;
it requires an enabled router. Mounts and inputs come from the service snapshot.

Run `make deck-icecast-control-token` in the operator terminal, then save the
result in **Tokens → Icecast control**. Initialization creates this additional
credential once in the gateway secret volume, preserving the existing read token.
The gateway rejects using the same credential for reading and management. Its
POST `/api/icecast/control` accepts only the closed, validated command DTO and
calls the existing IcecastAdmin RPCs with the server-held administration token.
The shared Deck gateway read token cannot authorize any management operation.
No browser-supplied endpoint or upstream credential is accepted.

The plugin consumes the optional `stationdeck.icecast.control` v1 service,
requiring `icecastd.control` and permission `icecastd.commander`. Denying that
permission leaves stream supervision available. Removing the saved control token
cancels pending management and clears private operation results without clearing
the read snapshot. Disabling the provider revokes both services. Operations are
single-flight and never retried automatically. A timeout/cancellation cannot undo
an operation already accepted upstream: refresh its status before retrying.
Completed mutations trigger one stream-status refresh; listener lists remain
explicit snapshots refreshed with **List listeners**.

The containerized Chromium recipe exercises metadata, route switching, listener
listing/disconnection, cancellation and source disconnection through the real
protobuf fixture. It also verifies read-token denial without an upstream call,
sanitized command failures and independent control-token deletion.
