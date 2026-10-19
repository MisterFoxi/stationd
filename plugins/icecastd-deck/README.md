# Icecast Deck plugin

This is a standalone compiled Deck plugin, following the trusted V1 extension
contract. `module.toml` declares its identity, version, required typed service,
local capability/permission and navigation contribution. It is assembled into the
WebAssembly application; it is not a dynamically downloaded WASM plugin.

The plugin depends only on `stationdeck-ui`, `stationdeck-services` and egui. It
knows no endpoint, gateway, Icecast administration token, protobuf or concrete
provider. `start` requests `stationdeck.icecast.streams` through its granted
`Context`; `stop` drops that access and local messages. Every call checks the
registry's revocable service handle. Required provider loss blocks the plugin
and removes its navigation view.

The deployment binds this plugin to `icecast-gateway` from the separate
`stationdeck-icecast` crate. That provider owns browser authentication, HTTP,
timeouts, single-flight requests, cancellation and late-response invalidation.
The server gateway calls the existing `IcecastAdmin` RPCs; it exposes
a separately authenticated management route and keeps the upstream administration token server-side.

## Live validation

Rebuild and start the service stack from `/data/dev` on stationdev1:

```sh
make deck-daemon-build
make deck-daemons
make deck-token
```

`deck-daemons` initializes missing credentials before starting containers. The
admin, monitor and optional router advertise versioned interfaces on the shared
private discovery network. The gateway verifies each instance with its own key
and resolves the stream interface through `config/stationdeck/services.json`.
The upstream API token remains server-side. `make deck-icecast` is a compatibility
alias for `make deck`; no Icecast-specific overlay is required.

A configured explicit endpoint still takes priority over discovery. Future
services use the same protocol and policy; their business interface needs its own
typed adapter. See [service discovery](../../Doc/stationdeck-service-discovery.md).

In Modules, enable **Icecast provider** and **Icecast streams plugin**. In **Tokens**, save the shared Deck gateway token, then connect in **Icecast connection**. Then open
**Icecast streams**. Each configured mount shows source presence, listener count,
title and content type. Refresh obtains another snapshot. Upstream unavailability
hides the previous snapshot and does not label every stream disconnected.

Validate registry behavior as well:

1. Enable the plugin alone: it must be Blocked with no stream view.
2. Enable the provider: the plugin must become Running, with its view present.
3. Connect and compare the mounts with `make icecast-ctl A="status"`.
4. Disable the provider: the plugin must become Blocked and lose its view.
5. Re-enable the provider: the saved token remains available, but the old snapshot must
   not return before a new authenticated read.
6. Leave the StationD gRPC module disabled throughout.

For the isolated browser fixture, `make deck-smoke` now exercises the same plugin,
manifest, policy, binding, HTTP bridge and real IcecastAdmin protobuf contract.
It verifies connected/offline mounts, audience, upstream failure, service removal
and credential cleanup, while asserting that no StationD status call is made by
the Icecast scenario. The containerized recipe covers explicit management commands as well.

## Management

The Management tab consumes the optional `stationdeck.icecast.control` v1
contract via its revocable handle and the `icecastd.commander` permission.
It supports metadata changes, listener snapshots and targeted disconnections,
source disconnection, and switching configured router inputs. Missing command
permission or credential leaves the read view usable. Source/listener disconnects
require a targeted confirmation inside the window; cancellation sends no command.

Obtain `make deck-icecast-control-token` in an operator terminal and save it in
Tokens → Icecast control. This additional credential is persisted independently
from the shared read token; forgetting it cancels pending management without
clearing read snapshots. Operations are sent once. After an uncertain timeout,
refresh before retrying. See [the operator guide](../../Doc/stationdeck-dev.md#icecast-stream-management).
