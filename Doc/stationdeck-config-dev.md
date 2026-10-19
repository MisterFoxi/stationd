# StationDeck configuration — authoring, authority and agents

The `configuration` compiled module edits plugin fields from a trusted schema
catalog. A permanent `stationdeck-control` container owns drafts and publications.
Agents pull their own published bundle over gRPC. This service is separate from
the existing read-only StationD status gateway; StationD 0.5 stays unchanged.

## Run the complete demo

On stationdev1, in `/data/dev` (V:), all builds and services run in containers:

```sh
make deck-config-demo
make deck-author-token
```

The second command displays the **configuration editor** token in your own
terminal. Open [StationDeck](http://stationdev1.lan:18081/), enable **Configuration
authoring** in Modules, save that token in **Tokens → Configuration author**, then open **Configuration** and connect.
The status gateway token from `make deck-token` grants no configuration rights.

Select `demo-1`, add `demo.settings`, edit its message and interval, then **Save
draft** and **Publish configuration**. Refresh to see the last confirmed applied
revision and the latest agent report. The demo consumer writes `runtime.json`
periodically using the published message and interval. The browser can be closed;
both the authority and agent keep running.

**Published history** can copy an older revision into a new draft. Saving and
publishing that draft creates a higher revision; previous publications stay intact.
Conflicting draft/publication versions return HTTP 409: reload before retrying.
Disconnecting or disabling the module clears private runtime data and local edits; the author token remains saved in Tokens. No configuration document is saved in localStorage. **Forget** in Tokens removes the saved author token and invalidates its clients.

`make deck-reader-token` displays a separate read-only credential. Its target and
permissions are enforced by the backend, including calls made outside the UI.
These are explicitly provisioned service/operator tokens, not user accounts or
an implicit StationD administrator role.

`make deck-down` stops the demo profile too and preserves all named volumes.

## Containers, state and credentials

| Component | Responsibility |
| --- | --- |
| `deck` | WASM WebUI, same-origin `/api/config/` proxy; Docker DNS refreshed dynamically. |
| `control` | HTTP authoring on 8080, gRPC on 50070, UDP discovery on 50071; no published host ports by default. |
| `config-init` | Creates missing operator and agent credentials from the trusted catalog; preserves existing tokens. |
| `config-demo-agent` | Optional `config-demo` profile, own identity and cache; consumes demo configuration. |

`config/stationdeck/catalog-daemons.json` is the default bootstrap **policy and schemas**, not the
published configuration. It declares authority, target identities, authorized
plugins, schemas, operator target scopes and `read`/`edit`/`publish` permissions.
It is mounted read-only. Policy changes require restarting the relevant services.
Credential files live in private Docker volumes, outside Git. The demo agent
mounts only its own credential subdirectory; it cannot read author tokens.

`config-state` stores `state.json`: format 1, drafts, immutable published history,
last confirmed applied revision and latest report. Replacement is atomic, fsynced
and protected by a single-writer lock. Corrupt state, wrong authority and unsupported
format fail startup without resetting data. This is a single-instance backend,
not a replicated database. Limits: 128 targets, 128 publications per target, 32 MiB
state; 64 documents per bundle within 128 KiB. No silent pruning of publications.
Back up this volume separately from the Icecast V1 backup, which does not include it.

`config-demo-state` stores `last-valid.json`, `effective.json` and the demo's
`runtime.json`. The agent restores and validates its last valid configuration
before attempting a network connection. First startup without a publication waits
and does not invent a configuration. Invalid identity/schema or obsolete/conflicting
revisions are rejected. Reconnection is bounded with backoff; reports use fixed,
redacted codes. `applied_revision` means last **confirmed** application. A failed
attempt is recorded separately and does not erase that confirmation or the cache.
After an external program succeeds but caching/reporting fails, reconcile the
actual daemon state; the report does not promise that external effects were undone.

## Discovery and explicit endpoints

The demo bootstrap is `config/stationdeck/agent.json`. A nonempty
`STATIONDECK_CONFIG_ENDPOINT` environment variable overrides its endpoint, e.g.
`STATIONDECK_CONFIG_ENDPOINT=https://deck-control.example:50070 make deck-config-demo`.
It is unrelated to the status gateway's `STATIONDECK_GRPC_ADDR`.

Bootstrap options:

- `endpoint: null`: UDP discovery for the configured authority.
- `endpoint: "https://deck-control.example:50070"`: explicit FQDN:port, selected
  before discovery. Bare FQDN:port defaults to HTTPS unless plaintext is enabled.
- `discovery_address: "255.255.255.255:50071"`: broadcasts on connected Linux IPv4
  subnets derived from `/proc/net/route`, including internal Docker networks with
  no default route. A specific IPv4:port can also be used.
- Multiple matching authority endpoints: refuse arbitrary selection and continue
  with the last valid configuration. Different authority IDs are ignored.

Discovery is a small versioned UDP protocol, not gRPC broadcast. Requests carry
an authority ID and random nonce; replies echo those and advertise the gRPC port.
No credentials or configuration are announced. gRPC authenticates the agent and
binds its token to its target; no client-supplied target identity is trusted.
Pull/report currently resynchronize every three seconds when connected; a watch
stream is not implemented and notifications are not required for correctness.

The supplied Compose is for a trusted development bridge and explicitly enables
plaintext in the demo agent. For other networks, publish only the necessary ports
with a deployment override and configure server TLS certificate/key paths through
`STATIONDECK_CONTROL_TLS_CERT` and `STATIONDECK_CONTROL_TLS_KEY`. Agent bootstrap
supports a CA file and TLS server name (needed when discovering an IP). TLS is
required unless `allow_plaintext` is explicitly enabled. No authentication is
established merely by discovering a name or matching an authority ID.

Broadcast stays within the connected network. Separate Docker projects must
share a suitable network, or use an explicitly reachable endpoint. The frontend
HTTP port 18081, StationD status port 50051 and Icecast admin port 50061 are distinct
from this configuration authority's gRPC port 50070.

## Plugin extension and real daemon adapters

The catalog is extensible without editing the authoring module. Each plugin schema
has an ID, explicit version, required fields, dependencies and application mode
(`hot`, `reload`, `restart`). Current field types: string (up to 4096 bytes), boolean
and bounded integer. Unknown fields, unsupported versions, missing dependencies,
duplicated instances and unauthorized plugins are rejected. Incompatible schema
changes require a new schema version. Both backend and agent validate documents.

The built-in runtime implements **demo.settings only**. Other plugins require a
trusted local application adapter; publishing a document does not install a plugin
or automatically configure StationD/Icecast. Their actual schemas and adapters must
be provisioned. Existing plugin schemas are not auto-imported yet; optional/nested
fields and secret editing are not implemented. Never place secret values in the
current authoring catalog: retain local secret references in real daemon adapters.

Agent bootstrap `apply_program` can name an absolute executable already present
in the agent container. It receives exactly one argument: the private candidate
bundle JSON path. It must validate local assets/permissions, perform the actual
application and verify the resulting daemon/plugin state before returning zero.
It must be idempotent for `(authority, target, revision)` and attempt restoration
on failure where supported. Unknown outcomes or failed restoration must return
failure; retaining the agent cache does not prove the daemon retained its runtime. The publication cannot choose a program, command
line, filesystem destination or station endpoint. No shell is invoked by the agent.
The direct child is killed after 15 seconds; stdout/stderr are not forwarded.

Real SD and icecastd adapters are now wired through this hook in the optional
real-daemon deployment. See [daemon adapter setup and limits](stationdeck-daemon-adapters.md).
The normal Icecast startup includes its agent. No special overlay is needed for icecastd.
catalog.json remains the minimal demo policy used by native fixtures.

## API and protocol

Author API, all authenticated and target-scoped:

- `GET /api/config/catalog`: permitted target state, schemas and permissions.
- `GET /api/config/targets/:id`: export current draft, publication and history.
- `PUT /api/config/targets/:id/draft`: `base_draft`, `documents`.
- `POST /api/config/targets/:id/publish`: `draft_version`, `base_revision`.

A document carries `plugin`, `instance`, schema `version` and typed `values`.
A published bundle carries authority, target, monotonically increasing revision
and documents. The separate protobuf contract is
`crates/stationdeck-control/proto/config.proto`, package `stationdeck.config.v1`:
`Configuration.Pull` and `Configuration.Report`. It does not change stationd-proto.

## Verification

```sh
make deck-check
make deck-smoke
make deck-config-smoke
```

The configuration recipe uses a unique disposable Compose project, internal
network, separate credentials/state and no host ports. It covers schema/permission
refusals, conflicts, broadcast discovery, real gRPC pull/report, demo consumption,
authority outage, agent restart from cache, authority restart, rollback, ambiguous
authorities, explicit endpoint priority and real WebUI edit/save/publish/cleanup.
It removes only that project's test volumes. The standard browser recipe continues
to cover offline test modules and the existing StationD status gateway.
