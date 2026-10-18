use stationdeck_gateway::Gateway;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

fn initialize_token(path: &Path) -> Result<(), &'static str> {
    if path.exists() {
        let existing =
            fs::read_to_string(path).map_err(|_| "Cannot read the existing gateway token.")?;
        Gateway::new(existing.trim(), None)?;
        return Ok(());
    }
    let mut random = [0_u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|_| "Cannot initialize gateway credentials.")?;
    let token = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "Cannot create gateway credentials.")?;
    file.write_all(token.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| "Cannot persist gateway credentials.")
}

#[tokio::main]
async fn main() -> Result<(), &'static str> {
    let token_file = std::env::var("STATIONDECK_ACCESS_TOKEN_FILE")
        .unwrap_or_else(|_| "/secrets/access-token".into());
    if std::env::args().nth(1).as_deref() == Some("init-token") {
        return initialize_token(Path::new(&token_file));
    }
    let token =
        fs::read_to_string(&token_file).map_err(|_| "Gateway access token is unavailable.")?;
    let address = std::env::var("STATIONDECK_GRPC_ADDR")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut gateway = Gateway::new(token.trim(), address.as_deref())?;
    if let Ok(path) = std::env::var("STATIONDECK_SERVICE_CATALOG") {
        gateway = gateway.with_services(
            stationdeck_discovery::Policy::read(Path::new(&path)).map_err(|e| e.message())?,
        )?;
    }
    if let Some(address) = std::env::var("STATIONDECK_ICECAST_GRPC_ADDR")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let path = std::env::var("STATIONDECK_ICECAST_TOKEN_FILE")
            .unwrap_or_else(|_| "/run/secrets/icecast-admin-api-token".into());
        let admin_token = fs::read_to_string(path)
            .map_err(|_| "Icecast administration credential is unavailable.")?;
        gateway = gateway.with_icecast(address.trim(), admin_token.trim())?;
    }
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .map_err(|_| "Cannot bind the gateway listener.")?;
    axum::serve(listener, gateway.router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| "Gateway server failed.")
}
