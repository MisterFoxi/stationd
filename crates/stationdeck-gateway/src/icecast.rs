//! Bounded adapter to the existing Icecast administration service.
use stationdeck_services::icecast::{IcecastStatus, StreamStatus};
use std::time::Duration;
use tonic::transport::{ClientTlsConfig, Endpoint};
#[allow(clippy::result_large_err)]
mod rpc {
    tonic::include_proto!("stationd.admin.icecast.v1");
}
#[derive(Clone)]
pub(crate) struct IcecastTarget {
    endpoint: Endpoint,
    token: String,
}
impl IcecastTarget {
    pub(crate) fn new(address: &str, token: &str) -> Result<Self, &'static str> {
        Self::with_tls_name(address, token, None)
    }
    pub(crate) fn with_tls_name(
        address: &str,
        token: &str,
        tls_name: Option<&str>,
    ) -> Result<Self, &'static str> {
        if token.is_empty() || token.len() > 512 || token.chars().any(char::is_control) {
            return Err("Invalid Icecast administration credential.");
        }
        let mut endpoint = Endpoint::from_shared(address.to_owned())
            .map_err(|_| "Invalid Icecast administration endpoint.")?;
        let uri = endpoint.uri();
        if !matches!(uri.scheme_str(), Some("http" | "https"))
            || uri.authority().is_none_or(|a| a.as_str().contains('@'))
            || uri.path() != "/"
            || uri.query().is_some()
        {
            return Err("Invalid Icecast administration endpoint.");
        }
        if uri.scheme_str() == Some("https") {
            let mut tls = ClientTlsConfig::new().with_native_roots();
            if let Some(name) = tls_name {
                tls = tls.domain_name(name);
            }
            endpoint = endpoint
                .tls_config(tls)
                .map_err(|_| "Invalid Icecast TLS configuration.")?;
        }
        Ok(Self {
            endpoint: endpoint
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(6)),
            token: token.into(),
        })
    }
    pub(crate) async fn read(&self) -> Result<IcecastStatus, (&'static str, &'static str)> {
        let channel = self.endpoint.connect().await.map_err(|_| {
            (
                "unavailable",
                "Icecast administration could not be reached.",
            )
        })?;
        let mut request = tonic::Request::new(rpc::Empty {});
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().map_err(|_| {
                (
                    "configuration",
                    "Icecast administration credentials are invalid.",
                )
            })?,
        );
        request.set_timeout(Duration::from_secs(6));
        let reply = rpc::icecast_admin_client::IcecastAdminClient::new(channel)
            .max_decoding_message_size(256 * 1024)
            .get_status(request)
            .await
            .map_err(|error| match error.code() {
                tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => (
                    "permission_denied",
                    "Icecast administration rejected the server-side credentials.",
                ),
                tonic::Code::DeadlineExceeded => ("timeout", "Icecast stream status timed out."),
                _ => ("unavailable", "Icecast stream status is unavailable."),
            })?
            .into_inner();
        let clean = |s: String, max| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect::<String>()
        };
        if reply.mounts.len() > 1024 {
            return Err(("invalid_response", "Icecast returned too many streams."));
        }
        let mut streams: Vec<_> = reply
            .mounts
            .into_iter()
            .map(|m| StreamStatus {
                mount: clean(m.mount, 256),
                connected: m.connected,
                listeners: m.listeners,
                title: clean(m.title, 1024),
                content_type: clean(m.content_type, 128),
            })
            .collect();
        streams.sort_by(|a, b| a.mount.cmp(&b.mount));
        Ok(IcecastStatus {
            server_id: clean(reply.server_id, 256),
            streams,
        })
    }
    pub(crate) async fn control(
        &self,
        command: stationdeck_services::icecast::IcecastCommand,
    ) -> Result<stationdeck_services::icecast::IcecastResult, (&'static str, &'static str)> {
        use stationdeck_services::icecast::{
            IcecastCommand as C, IcecastResult as R, Listener, RouteStatus,
        };
        command.validate().map_err(|e| ("invalid_request", e))?;
        let channel = self.endpoint.connect().await.map_err(|_| {
            (
                "unavailable",
                "Icecast administration could not be reached.",
            )
        })?;
        let mut client = rpc::icecast_admin_client::IcecastAdminClient::new(channel)
            .max_decoding_message_size(512 * 1024);
        // Each RPC has its own request type; credentials never leave this adapter.
        macro_rules! req {
            ($body:expr) => {{
                let mut r = tonic::Request::new($body);
                r.metadata_mut().insert(
                    "authorization",
                    format!("Bearer {}", self.token)
                        .parse()
                        .map_err(|_| ("configuration", "Invalid administration credential."))?,
                );
                r.set_timeout(Duration::from_secs(6));
                r
            }};
        }

        let clean = |s: String, max: usize| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(max)
                .collect::<String>()
        };
        let changes = command.changes_stream();
        let router = matches!(&command, C::Route | C::Switch { .. });
        let migration_command = matches!(
            &command,
            C::MigrationPlan { .. }
                | C::MigrationRun { .. }
                | C::MigrationReturn { .. }
                | C::MigrationCancel { .. }
                | C::Migrations
        );
        let error = |e: tonic::Status| {
            if migration_command {
                match e.code() {
                tonic::Code::FailedPrecondition => ("migration_refused","Migration refused. Check stream availability, overlapping cohorts, journal state and Icecast restarts. Refresh the journal."),
                tonic::Code::ResourceExhausted => ("migration_limit","Migration or journal limit reached."),
                _ => operation_error(e.code(),changes,router),
            }
            } else {
                operation_error(e.code(), changes, router)
            }
        };
        match command {
            C::MigrationPlan {
                mount,
                destination,
                continuous,
            } => {
                let operation = client
                    .plan_listener_migration(req!(rpc::MigrationPlanRequest {
                        mount,
                        destination,
                        continuous
                    }))
                    .await
                    .map_err(error)?
                    .into_inner();
                Ok(R::Migration {
                    operation: migration(operation)?,
                })
            }
            C::MigrationRun { id } | C::MigrationReturn { id } | C::MigrationCancel { id } => {
                let request = req!(rpc::MigrationRequest { id });
                let operation = match command {
                    C::MigrationReturn { .. } => client.return_listener_migration(request).await,
                    C::MigrationCancel { .. } => client.cancel_listener_migration(request).await,
                    _ => client.run_listener_migration(request).await,
                }
                .map_err(error)?
                .into_inner();
                Ok(R::Migration {
                    operation: migration(operation)?,
                })
            }
            C::Migrations => {
                let operations = client
                    .list_listener_migrations(req!(rpc::Empty {}))
                    .await
                    .map_err(error)?
                    .into_inner()
                    .operations;
                if operations.len() > 100 {
                    return Err(("invalid_response", "Too many migration operations."));
                }
                Ok(R::Migrations {
                    operations: operations
                        .into_iter()
                        .map(migration)
                        .collect::<Result<_, _>>()?,
                })
            }
            C::Route | C::Switch { .. } => {
                let reply = match command {
                    C::Switch { input } => {
                        client
                            .switch_route(req!(rpc::SwitchRequest { input }))
                            .await
                    }
                    _ => client.get_route(req!(rpc::Empty {})).await,
                }
                .map_err(error)?
                .into_inner();
                if reply.inputs.len() > 128 || reply.input_streams.len() > 128 {
                    return Err(("invalid_response", "Too many router inputs."));
                }
                Ok(R::Route {
                    route: RouteStatus {
                        output_mount: clean(reply.output_mount, 256),
                        input: clean(reply.input, 128),
                        connected: reply.connected,
                        inputs: reply.inputs.into_iter().map(|s| clean(s, 128)).collect(),
                        problem: clean(reply.problem, 512),
                        input_streams: reply
                            .input_streams
                            .into_iter()
                            .map(|input| stationdeck_services::icecast::RouteInput {
                                id: clean(input.id, 128),
                                mount: clean(input.mount, 256),
                            })
                            .collect(),
                    },
                })
            }
            C::Listeners { mount } => {
                let reply = client
                    .list_listeners(req!(rpc::MountRequest {
                        mount: mount.clone()
                    }))
                    .await
                    .map_err(error)?
                    .into_inner();
                if reply.listeners.len() > 2048 {
                    return Err((
                        "invalid_response",
                        "Too many listeners; use the administration CLI.",
                    ));
                }
                Ok(R::Listeners {
                    mount,
                    listeners: reply
                        .listeners
                        .into_iter()
                        .map(|l| Listener {
                            id: l.id,
                            ip: clean(l.ip, 128),
                            user_agent: clean(l.user_agent, 256),
                            connected_seconds: l.connected_seconds,
                        })
                        .collect(),
                })
            }
            C::Metadata { mount, title } => {
                client
                    .set_metadata(req!(rpc::MetadataRequest { mount, title }))
                    .await
                    .map_err(error)?;
                Ok(R::Done {
                    message: "Metadata updated. Refresh streams to verify.".into(),
                })
            }
            C::KickSource { mount } => {
                client
                    .kick_source(req!(rpc::MountRequest { mount }))
                    .await
                    .map_err(error)?;
                Ok(R::Done {
                    message: "Source disconnected. Its encoder may reconnect automatically.".into(),
                })
            }
            C::KickListener { mount, id } => {
                client
                    .kick_listener(req!(rpc::KickListenerRequest { mount, id }))
                    .await
                    .map_err(error)?;
                Ok(R::Done {
                    message: "Listener disconnected. Refresh listeners to verify.".into(),
                })
            }
        }
    }
}

// Never forward arbitrary upstream diagnostics. Reads cannot have an unknown
// mutation outcome; router failures should identify the missing dependency.
fn operation_error(code: tonic::Code, changes: bool, router: bool) -> (&'static str, &'static str) {
    match code {
        tonic::Code::InvalidArgument | tonic::Code::NotFound => (
            "invalid_target", "The target is not configured or the request was rejected.",
        ),
        tonic::Code::PermissionDenied | tonic::Code::Unauthenticated => (
            "permission_denied", "Icecast administration denied this operation.",
        ),
        tonic::Code::Unimplemented | tonic::Code::FailedPrecondition => (
            "unavailable", "This operation is unavailable for the current stream or router.",
        ),
        _ if router && !changes => (
            "router_unavailable", "Stream router unavailable. Check that the router service is running, then load the route again.",
        ),
        _ if !changes => (
            "unavailable", "Listener list unavailable. Check Icecast administration and the selected mount, then refresh listeners.",
        ),
        _ if router => (
            "router_unavailable", "Stream router unavailable or switching failed. The outcome may be unknown; reload the route before retrying.",
        ),
        _ => (
            "unavailable", "Operation failed or its outcome is unknown. Refresh before retrying.",
        ),
    }
}

fn migration(
    op: rpc::ListenerMigration,
) -> Result<stationdeck_services::icecast::ListenerMigration, (&'static str, &'static str)> {
    if op.id == 0
        || op.total > 2048
        || [op.moved, op.returned, op.missing, op.uncertain]
            .iter()
            .map(|n| *n as u64)
            .sum::<u64>()
            > op.total as u64
        || !matches!(
            op.state.as_str(),
            "planned" | "moving" | "away" | "returning" | "complete" | "interrupted" | "expired"
        )
    {
        return Err(("invalid_response", "Invalid migration journal response."));
    }
    let clean = |s: String, max| {
        s.chars()
            .filter(|c| !c.is_control())
            .take(max)
            .collect::<String>()
    };
    Ok(stationdeck_services::icecast::ListenerMigration {
        id: op.id,
        continuous: op.continuous,
        mount: clean(op.mount, 256),
        destination: clean(op.destination, 256),
        state: op.state,
        total: op.total,
        moved: op.moved,
        returned: op.returned,
        missing: op.missing,
        uncertain: op.uncertain,
        created_at: op.created_at,
        note: clean(op.note, 512),
    })
}

#[cfg(test)]
mod control_error_tests {
    use super::*;
    #[test]
    fn unavailable_reads_identify_the_dependency_without_claiming_a_mutation() {
        let (code, message) = operation_error(tonic::Code::Unavailable, false, true);
        assert_eq!(code, "router_unavailable");
        assert!(message.contains("router service is running"));
        assert!(!message.contains("unknown"));
        let (_, message) = operation_error(tonic::Code::DeadlineExceeded, false, false);
        assert!(message.contains("Listener list"));
        assert!(!message.contains("unknown"));
    }
    #[test]
    fn failed_mutations_keep_the_uncertain_outcome_explicit() {
        for router in [false, true] {
            let (_, message) = operation_error(tonic::Code::Unavailable, true, router);
            assert!(message.contains("unknown"));
        }
        assert_eq!(
            operation_error(tonic::Code::Unauthenticated, false, true).0,
            "permission_denied"
        );
    }
}
