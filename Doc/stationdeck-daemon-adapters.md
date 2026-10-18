# Real daemon configuration adapters (development)

Wired on 2026-10-18. Agent-based Icecast startup is now the default; runtime readback was verified. Full test suites remain with the operator. StationD 0.5
source and protobuf contracts are unchanged. The optional demo remains available alongside the real targets.

## Build and start on stationdev1

From `/data/dev`, with all builds and services in containers:

```sh
make deck-daemon-build
make deck
make icecast-up
make deck-sd-agent STATIONDECK_SD_ENDPOINT=your-stationd.example:50051
```

Replace the last address with the existing SD gRPC listener. No endpoint is guessed
and StationD itself is not launched or rewritten. Its listener-stats plugin must
already exist and be loaded. A renamed plugin can be mapped through the trusted
`daemon_plugin` in `config/stationdeck/sd-adapter.json`.

The normal StationDeck Compose initializes catalog-daemons.json, the icecastd
credential and the shared discovery network. The normal Icecast Compose/image
always starts its agent. No additional overlay or agent command is required for
icecastd; deck-daemons remains a shortcut for starting both stacks.

Start StationDeck first to initialize the local credential/network. No configuration
is published automatically. On first installation Icecast waits for a valid
publication and remains unready; publish through the available WebUI. On restart
the agent restores its validated cache before contacting StationDeck, so a backend
outage does not discard the last valid configuration. The effective TOML and the
local bootstrap cannot independently start Icecast. Local TOML supplies only the
fixed mounts, secret references and other non-authored bootstrap settings.
Existing credentials, cache, effective volume and router selection are preserved.
Development defaults assume project names stationdeck and stationd-v1-icecast.

## Authoring

In the WebUI, enable Configuration authoring and connect with the editor token
from `make deck-author-token` in your own terminal. Choose a target, add exactly
one document, set instance to **default**, edit, save, then publish.

| Target | Schema | Fields | Application |
| --- | --- | --- | --- |
| sd-listener-stats-1 | sd.listener-stats v1 | retention_days, 1..365 | Existing SD preview/save/reload RPCs |
| icecastd-1 | icecast.settings v1 | location, hostname, admin_email, max_clients (1..32768) | Validated XML, process restart, native stats verification |

Example document arrays: `config/stationdeck/example-sd-documents.json` and
`config/stationdeck/example-icecast-documents.json`. Icecast identity strings must
be nonempty, free of control characters, and at most 1024 UTF-8 bytes. Native
validation can reject a publication; desired and applied revisions stay distinct.

This first mapping configures listener-stats, not every installed plugin. Other
scalar plugins can reuse the adapter after provisioning a schema, target, private
credential, local mapping and agent service. Each SD target handles one plugin
so bundles cannot partially commit several plugins. Optional resets, nested
values, secret editing and automatic schema import remain to implement.
A publication never installs or starts a plugin.

## SD semantics

The adapter validates against its local catalog and selects the daemon/plugin
from local policy. GetConfig supplies the exact file/schema revisions. The
adapter checks types/bounds and refuses secret edits, then previews, saves with
SAVE_RELOAD and reads back the revision/values. Only confirmed running values
produce success. Unchanged values with pending=false do not trigger reload.

Concurrent edits cause conflicts. A confirmed reload failure attempts guarded
restoration of the edited fields. Restoration can fail; a stopped/failed plugin
is never silently started. Inspect SD plugin state after failure. A timed-out RPC
has an unknown outcome, reconciled on the next pull. This is not a network
transaction or a guarantee that a failed plugin remains running. The last valid
agent cache is retained until confirmed success.

STATIONDECK_SD_ENDPOINT selects SD. STATIONDECK_CONFIG_ENDPOINT selects the
StationDeck authority, with priority over broadcast discovery. No publication
can choose the SD endpoint. Local SD policy supports HTTPS, native/custom CA,
TLS server name and an optional bearer-token file mounted separately. Private
HTTP is explicitly enabled in the development policy. No SD credential is
embedded. The status gateway separately uses STATIONDECK_GRPC_ADDR.

## Icecast semantics

The managed container supervises both agent and Icecast, with a private Unix
socket for application and no Docker socket. UID/GID 1000 can read its private
agent token. Only the four authorized root scalars are overlaid on the local
TOML. Mounts, secret references, GeoIP, proxies, port and other settings stay local.

station-icecast-config validates/renders the candidate and local assets before
the running process is stopped. XML containing secrets stays private in /tmp
(tmpfs). The new process consumes that exact XML; authenticated native stats
verify hostname/location/admin email. The client limit is rendered into the XML.
Effective TOML is committed only after startup verification. Failure attempts to
restart the previous XML and restore its TOML; failed rollback exits the container
for Docker to restart. No HUP support is assumed. Restart disconnects listeners
and broadcasters; they need their reconnect policy.

Admin reads the secret-free effective TOML through its read-only /managed mount
for GetConfig/show/revision. Existing mount/credential policy stays local. The
host icecast-apply workflow refuses an active StationDeck-managed container.

Both real adapters reconcile on every pull, even for the same desired revision.
Icecast no-ops check health without restarting. Agent restart restores the last
valid cached bundle before contacting the authority. Latest attempt and last
confirmed applied revision are separate; a failed attempt is never current success.

## Lifecycle and state

The shared stationdeck-configuration Docker bridge connects control, SD agent
and icecastd for broadcast discovery. Configuration gRPC/UDP have no host ports.
An external authority needs separately provisioned connectivity and TLS. Each
agent sees only its own credential via a named-volume subpath.

Normal lifecycle commands keep the agent; no special managed overlay is needed:

```sh
make icecast-up
make icecast-live-up
make icecast-switch-up
make deck-daemons-down
```

Sources A/B restart to reconnect after an Icecast restart. Router state is retained.
The old compose.stationdeck.yaml is a no-op compatibility alias; base-only startup
also uses the agent. The old separate Dockerfile.icecast-agent was removed.
An outdated standalone image cannot silently fall back: the normal Compose
explicitly requires the supervisor entrypoint and fails if that image lacks it.

compose.standalone-test.yaml explicitly selects the old local-file entrypoint for
isolated legacy recipes only. It is never included by normal lifecycle commands.
The geography recipe opts into this compatibility fixture deliberately.

Existing distribution/backup recipes do not package the new StationDeck volumes.
Back up authority, agent and effective volumes separately before migration.
This wiring currently targets the source-based development deployment.

## Validation handoff

The normal Icecast and WebUI images were built in containers. Default Compose
and Make startup and real revision/value readback were checked. No full test
suite was run: the operator owns test execution. A focused regression recipe is
available as tests/stationdeck-icecast-startup.py: without a publication/cache,
neither a bootstrap nor an old effective TOML may start the Icecast process.
The existing demo recipes do not fully validate the real daemon adapters.
