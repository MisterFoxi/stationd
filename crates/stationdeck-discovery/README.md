# stationdeck-discovery

Service-neutral signed discovery and versioned interface selection. No Icecast,
StationD, egui, tonic or business DTO dependency is present in this package.

An embedded service calls `start_from_env().await?` once at startup and retains
the returned task handle for its lifetime. A non-Rust service can run the companion
`stationdeck-discovery` executable in its own network namespace. Configure:

- STATIONDECK_SERVICE_DESCRIPTOR: descriptor JSON path.
- STATIONDECK_DISCOVERY_KEY_FILE: its private instance-specific signing key.
- STATIONDECK_SERVICE_DISCOVERY_BIND: optional IPv4 UDP bind, default :50072.

A descriptor identifies an authority and instance, lease duration and interfaces.
Each interface supplies id, SemVer, transport, scheme, port and TLS indication.
The same descriptor can list gRPC, HTTP and other interfaces on different ports.
Adding an interface does not require changing the announcement or selection code.

Clients load `Policy` with explicit trust and interface bindings, then call
`resolve(interface_id).await`. Explicit endpoints take priority; discovered origins
are authenticated before any API credential can be sent. API permissions remain
separate. The policy does not discover or create its own trust keys or credentials.

See [protocol and deployment status](../../Doc/stationdeck-service-discovery.md).
Tests are included but were not executed by the implementation agent.
