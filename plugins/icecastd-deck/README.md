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
The server gateway calls the existing `IcecastAdmin.GetStatus` RPC; it exposes
no mutation routes and keeps the upstream administration token server-side.

## Live validation

Use the existing running icecastd/admin stack. From `/data/dev` on stationdev1:

```sh
make deck-icecast
make deck-token
```

The first command starts Deck with `docker/stationdeck/compose.icecast.yaml`.
It connects the gateway to the existing `stationd-v1-icecast_default` network and
mounts only `.local/icecast/secrets/admin-api-token`. It remembers this deployment choice in `.local/stationdeck/icecast.enabled`, so
subsequent `make deck`, `make deck-down` and `make deck-token` keep the same profile.
It does not start, restart,
or reconfigure icecastd. For a different deployment use
`STATIONDECK_ICECAST_NETWORK` and `STATIONDECK_ICECAST_GRPC_ADDR`.

In Modules, enable **Icecast provider** and **Icecast streams plugin**. In
**Icecast connection**, enter the Deck gateway token and connect. Then open
**Icecast streams**. Each configured mount shows source presence, listener count,
title and content type. Refresh obtains another snapshot. Upstream unavailability
hides the previous snapshot and does not label every stream disconnected.

Validate registry behavior as well:

1. Enable the plugin alone: it must be Blocked with no stream view.
2. Enable the provider: the plugin must become Running, with its view present.
3. Connect and compare the mounts with `make icecast-ctl A="status"`.
4. Disable the provider: the plugin must become Blocked and lose its view.
5. Re-enable the provider: the credential must be empty; the old snapshot must
   not return before a new authenticated read.
6. Leave the StationD gRPC module disabled throughout.

For the isolated browser fixture, `make deck-smoke` now exercises the same plugin,
manifest, policy, binding, HTTP bridge and real IcecastAdmin protobuf contract.
It verifies connected/offline mounts, audience, upstream failure, service removal
and credential cleanup, while asserting that no StationD status call is made by
the Icecast scenario. All new tests are supplied for the operator to run.
