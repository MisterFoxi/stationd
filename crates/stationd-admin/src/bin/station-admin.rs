// tonic 0.12 uses Status as its transport error, including generated RPC code.
#![allow(clippy::result_large_err)]
use anyhow::Result;
use clap::Parser;
use stationd_admin::{
    authorized, read_secret, rpc::icecast_admin_server::IcecastAdminServer, Admin,
};
use std::path::PathBuf;
use tonic::{transport::Server, Status};

#[derive(Parser)]
#[command(about = "V1 central administration daemon (Icecast first)")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:50061")]
    bind: std::net::SocketAddr,
    #[arg(long, default_value = "/etc/stationd/icecast.toml")]
    config: PathBuf,
    #[arg(long, default_value = "/run/secrets")]
    secrets_dir: PathBuf,
    #[arg(long, default_value = "http://icecast:8000")]
    icecast_url: String,
}

async fn shutdown_signal() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let admin = Admin::load(&args.config, &args.secrets_dir, &args.icecast_url)?;
    let token = read_secret(&args.secrets_dir.join("admin-api-token"))?;
    let expected = format!("Bearer {token}");
    let service = IcecastAdminServer::with_interceptor(admin, move |req: tonic::Request<()>| {
        let received = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !authorized(&expected, received) {
            return Err(Status::unauthenticated("admin token required"));
        }
        Ok(req)
    });
    eprintln!("station-admin listening on {}", args.bind);
    Server::builder()
        .add_service(service)
        .serve_with_shutdown(args.bind, shutdown_signal())
        .await?;
    Ok(())
}
