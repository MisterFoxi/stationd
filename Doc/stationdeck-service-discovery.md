# Generic service discovery

Status: protocol, resolver, adapters, deployment configuration and validation
scenarios are written. Compilation and tests have not been run by the agent.
The operator rebuilds and starts the containers to activate the new protocol.

## Contract

`stationdeck-discovery` has no service-specific identifiers, application DTOs,
Icecast dependency, egui dependency or gRPC dependency. The descriptor provides
an authority, stable instance id, lease duration and a list of interfaces. Each
interface supplies a qualified id, SemVer, transport, URI scheme, port and TLS
indicator. A future service reuses the same descriptor and announcement hook.

The shared Rust hook reads STATIONDECK_SERVICE_DESCRIPTOR,
STATIONDECK_DISCOVERY_KEY_FILE and optional STATIONDECK_SERVICE_DISCOVERY_BIND
(default UDP 50072). A companion executable exposes the same hook for non-Rust
services inside the same network namespace. It must announce the actual address
of the service; an arbitrary sidecar IP is not a service address.

Queries contain a protocol version, authority and fresh nonce. Responses contain
the descriptor, process boot id, selected local IPv4 address and HMAC-SHA256
signature. Verification requires an explicitly provisioned key for that instance.
The signed address must match the sender address, so relaying a genuine packet
cannot redirect API credentials to the relay. Wrong authority, nonce, signature,
source address, version, schema or untrusted identity is ignored. Packets,
interfaces, replies and scan duration are bounded. No secret travels over UDP.

Discovery is presence, not proof of RPC health and not authorization to call it.
The service's normal RPC authentication/authorization still applies. The current
Icecast admin API has one server-side token; this change adds no per-user server
roles or permissions to that existing API.

## Resolution and plugin binding

The gateway can load a generic policy from STATIONDECK_SERVICE_CATALOG. It defines
the authority, discovery destination, trusted identities/key files and interface
bindings. Each binding defines a required version and transport, optional selected
instance, optional explicit endpoint, server-side API credential file and transport
trust settings. `config/stationdeck/services.json` is the development bootstrap.

An explicit endpoint is checked and used before any discovery. Otherwise the
resolver takes a fresh signed inventory and selects one compatible, allowed
instance. No candidate, expired offers, incompatible versions and ambiguous
candidates give distinct diagnostics. No first-response selection, retained Docker
address or automatic fallback after an authentication failure is used. Fresh scans
on the next request rediscover a restarted instance without an old endpoint cache.

`GET /api/services`, protected by the Deck read token, exposes the discovered
inventory and binding status. It never exposes signing keys, API tokens or their
file paths. This API applies equally to present and future service interfaces.

The Icecast HTTP bridge is the first adapter to resolve its interface through this
policy. The existing explicit Icecast endpoint remains a higher-priority override.
The `icecastd-deck` plugin stays unchanged: it still requests the same typed service
from the module registry. Its provider's transport changes behind that contract.
New wire interfaces still require a typed adapter; discovery cannot invent their
business API or bypass deployment grants.

## Prepared existing-service descriptions

- configuration-1: configuration gRPC and authoring HTTP.
- icecast-admin-1: stream status gRPC.
- icecast-monitor-1: monitoring gRPC.
- icecast-router-1: routing gRPC.

All four processes have the same optional announcement startup hook. Merely adding
a descriptor file does not start the hook; deployment must explicitly provision
its key and set the bootstrap variables. Configuration's older UDP 50071 discovery
remains compatible for existing agents; service discovery uses UDP 50072 separately.

## Deployment configuration

The following configuration changes were approved and applied to the repository:

1. Attach the gateway and the three Icecast administration processes to the
   existing private StationDeck configuration/discovery bridge. Keep their
   current networks and do not publish a new host port.
2. Provision a separate signing key per announced instance. Do not reuse the
   browser token or an API token as an announcement key.
3. Provision a private server-side service-credential directory. The gateway
   receives a read-only mount of explicitly authorized verification keys and
   RPC credentials. Each service receives only its own signing key. Bootstrap
   initialization remains idempotent and rejects unexpected mismatched material.
4. Set the descriptor/key variables on each announcing service and load the
   generic service policy in the gateway. Remove the fixed admin hostname and
   Icecast-specific network selection from the previous profile; retain the
   command as a compatibility alias.

Icecast signing keys and authorized gateway credentials are provisioned by
`make icecast-init`. Existing values are preserved; a mismatch refuses replacement.
The configuration authority uses its own `config-discovery-secrets` volume,
read-only in control and gateway. Other service keys are never mounted in control.
The private host credential directory is prepared by `deck-bootstrap`.

From the repository root on the Docker host:

```sh
make deck-daemon-build
make deck-daemons
```

The build command rebuilds the gateway, control and Icecast images, including the
new announcement hooks. The start command initializes missing credentials before
Compose creates containers. `make deck` enables generic discovery; `deck-icecast`
is retained as a compatibility alias. No Icecast hostname is fixed in that path.
No running services were rebuilt or restarted by the implementation agent.

## Validation supplied, not executed

The library tests cover a published HMAC vector, packet signatures, nonce replay,
authority/source mismatch, unknown instances, ambiguity, SemVer incompatibility,
transport denial, lease expiration, explicit-endpoint precedence, real UDP and
service restart. A fictional future HTTPS analytics service proves that neither
the protocol nor the selector needs a new service-specific branch.

The existing container checks include the new crate. The isolated browser recipe removes the explicit Icecast endpoint, discovers its
signed fixture and checks the protected inventory before reading streams. Its
credentials are disposable Compose volumes, with no live credential bind mount.
The operator runs `make deck-check`, `make deck-smoke` and `make deck-config-smoke`.
For live validation, run the browser recipe with Icecast's explicit endpoint
unset, verify the protected inventory, compare stream status, and repeat after
admin restart. A second compatible announced instance without an explicit binding
must produce ambiguity, never a silent switch. Future services follow the same
recipe with their descriptor and explicitly provisioned trust/permissions.

## Monitor and audio router consumers

The normal deployment no longer provides `admin_url` in `monitor.toml` or
`icecast_url` in `router.toml`. Monitor resolves `stationdeck.icecast.streams`
using `discovery-monitor.json` for each observation. Discovery errors mark admin
unavailable and expose a stable diagnostic code; no old address is reused.

Router resolves `stationdeck.icecast.audio` using `discovery-router.json` for each
input preparation and reconnection. The source output uses the same resolved
origin as its prepared input. An input discovered on a different server cannot be
switched into an existing output connection. Established audio connections are
retained; reconnects resolve again, without DNS-name or previous-IP fallback.

Managed Icecast starts the generic discovery companion in its own network
namespace only after its first successful start. The lifecycle supervisor monitors
and terminates this companion with its other children. `services-icecast.json`
describes the HTTP interface; `icecast-discovery-key` is its distinct signing key.
Router receives only that upstream verification key in addition to its own
announcement key. Monitor receives admin's verification key, in addition to its
own announcement key. `icecast-init` provisions missing material before startup.

Explicit TOML origins still take priority, and `STATIONDECK_ICECAST_URL` is an
optional router override. The isolated legacy `compose.standalone-test.yaml`
recipe sets that override because its standalone Icecast has no managed companion;
normal deployments do not use it. These clients currently support HTTP only and
refuse HTTPS/TLS origins instead of downgrading them.

Operator validation after rebuilding: observe monitor's successful admin reads;
start the optional router with the switch profile and verify its selected stream;
recreate admin, then Icecast with a changed container IP and verify the next
observation/reconnection recovers. Remove the trusted key or introduce a duplicate
signed instance to verify no arbitrary server selection or old-address fallback.
No builds or test suites were run by the implementation agent.
