use stationdeck_control::{
    store::{Bootstrap, Store},
    wire::configuration_server::ConfigurationServer,
};
use std::{fs, path::Path};
fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}
fn initialize(path: &Path) -> Result<(), &'static str> {
    use std::io::Read;
    if path.exists() {
        stationdeck_control::read_token(path)?;
        return Ok(());
    }
    let mut bytes = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|_| "Cannot initialize credentials.")?;
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let parent = path.parent().ok_or("Invalid credential path.")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create credential directory.")?;
    let token = bytes.iter().map(|n| format!("{n:02x}")).collect::<String>();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "Cannot create credential; existing credentials are never replaced.")?;
    file.write_all(token.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "Cannot persist credential.")?;
    fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| "Cannot persist credential directory.")
}
#[tokio::main]
async fn main() -> Result<(), &'static str> {
    match std::env::args().nth(1).as_deref() {
        Some("init") => {
            let catalog: Bootstrap = serde_json::from_slice(
                &fs::read(env(
                    "STATIONDECK_CONTROL_CATALOG",
                    "/bootstrap/catalog.json",
                ))
                .map_err(|_| "Configuration catalog unavailable.")?,
            )
            .map_err(|_| "Invalid configuration catalog.")?;
            catalog.validate()?;
            if let Ok(path) = std::env::var("STATIONDECK_DISCOVERY_KEY_FILE") {
                initialize(Path::new(&path))?;
            }
            for principal in &catalog.operators {
                initialize(&Path::new("/author-secrets").join(&principal.token_file))?;
            }
            for target in &catalog.targets {
                initialize(
                    &Path::new("/agent-secrets")
                        .join(&target.token_file)
                        .join("token"),
                )?;
            }
            return Ok(());
        }
        Some("apply-sd") => {
            let candidate = std::env::args()
                .nth(2)
                .ok_or("Candidate path is required.")?;
            return stationdeck_control::sd_adapter::apply(Path::new(&candidate)).await;
        }
        Some("agent") => {
            let mut config = stationdeck_control::agent::read_config(Path::new(&env(
                "STATIONDECK_AGENT_CONFIG",
                "/bootstrap/agent.json",
            )))?;
            if let Ok(endpoint) = std::env::var("STATIONDECK_CONFIG_ENDPOINT") {
                if !endpoint.trim().is_empty() {
                    config.endpoint = Some(endpoint);
                }
            }
            tokio::select! {r=stationdeck_control::agent::run(config)=>return r,_=tokio::signal::ctrl_c()=>return Ok(())}
        }
        _ => {}
    }
    let _announcer = stationdeck_discovery::start_from_env()
        .await
        .map_err(|e| e.message())?;
    let bootstrap: Bootstrap = serde_json::from_slice(
        &fs::read(env(
            "STATIONDECK_CONTROL_CATALOG",
            "/bootstrap/catalog.json",
        ))
        .map_err(|_| "Configuration catalog unavailable.")?,
    )
    .map_err(|_| "Invalid configuration catalog.")?;
    let store = Store::open(
        bootstrap,
        Path::new(&env("STATIONDECK_CONTROL_STATE", "/data/state.json")),
        Path::new("/secrets"),
    )?;
    let http = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .map_err(|_| "Cannot bind authoring API.")?;
    let grpc_bind = env("STATIONDECK_CONTROL_GRPC_BIND", "0.0.0.0:50070")
        .parse::<std::net::SocketAddr>()
        .map_err(|_| "Invalid gRPC bind address.")?;
    let mut server = tonic::transport::Server::builder();
    match (
        std::env::var("STATIONDECK_CONTROL_TLS_CERT"),
        std::env::var("STATIONDECK_CONTROL_TLS_KEY"),
    ) {
        (Ok(cert), Ok(key)) => {
            let identity = tonic::transport::Identity::from_pem(
                fs::read(cert).map_err(|_| "TLS certificate unavailable.")?,
                fs::read(key).map_err(|_| "TLS key unavailable.")?,
            );
            server = server
                .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
                .map_err(|_| "Invalid TLS identity.")?;
        }
        (Err(_), Err(_)) => {}
        _ => return Err("TLS certificate and key must both be configured."),
    }
    let grpc = server
        .add_service(
            ConfigurationServer::new(stationdeck_control::server::Grpc(store.clone()))
                .max_decoding_message_size(4096)
                .max_encoding_message_size(256 * 1024),
        )
        .serve(grpc_bind);
    let discovery = stationdeck_control::discovery::serve(
        "0.0.0.0:50071",
        store.bootstrap.authority.clone(),
        grpc_bind.port(),
    );
    let web = axum::serve(http, stationdeck_control::server::router(store)).into_future();
    use std::future::IntoFuture;
    tokio::select! {
        _=web=>Err("Authoring API stopped."),
        _=grpc=>Err("Configuration gRPC server stopped."),
        _=discovery=>Err("Configuration discovery stopped."),
        _=tokio::signal::ctrl_c()=>Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn initialization_preserves_existing_credentials_and_refuses_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent/token");
        initialize(&path).unwrap();
        let first = fs::read(&path).unwrap();
        initialize(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);
        assert_eq!(first.len(), 64);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(&path, b"invalid").unwrap();
        assert!(initialize(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid");
    }
}
