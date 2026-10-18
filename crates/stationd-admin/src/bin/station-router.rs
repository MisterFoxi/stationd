// tonic uses Status as its transport error.
#![allow(clippy::result_large_err)]
use anyhow::Result;
use clap::Parser;
use stationd_admin::{
    authorized, read_secret,
    router::{Router, RouterConfig},
    rpc::stream_router_server::StreamRouterServer,
};
use std::path::PathBuf;
use tonic::{transport::Server, Status};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:50062")]
    bind: std::net::SocketAddr,
    #[arg(long, default_value = "/etc/stationd/router.toml")]
    config: PathBuf,
    #[arg(long, default_value = "/run/secrets")]
    secrets_dir: PathBuf,
    #[arg(long, default_value = "/var/lib/station-router/selection.json")]
    state: PathBuf,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let _announcer = stationdeck_discovery::start_from_env().await?;
    let mut config = RouterConfig::load(&args.config)?;
    if let Ok(origin) = std::env::var("STATIONDECK_ICECAST_URL") {
        if !origin.trim().is_empty() {
            stationd_admin::discovery::http_origin(origin.trim())?;
            config.icecast_url = origin.trim().to_owned();
        }
    }
    let (router, mut changes) = Router::new(config, &args.state)?;
    let router_token = read_secret(&args.secrets_dir.join("router-api-token"))?;
    let worker_router = router.clone();
    let worker = tokio::spawn(async move {
        worker_router
            .run(&mut changes, &args.secrets_dir, args.state)
            .await
    });
    let expected = format!("Bearer {router_token}");
    let service = StreamRouterServer::with_interceptor(router, move |req: tonic::Request<()>| {
        let token = req
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !authorized(&expected, token) {
            return Err(Status::unauthenticated("router token required"));
        }
        Ok(req)
    });
    let server = Server::builder()
        .add_service(service)
        .serve_with_shutdown(args.bind, async {
            #[cfg(unix)]
            if let Ok(mut stop) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! { _ = stop.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
                return;
            }
            let _ = tokio::signal::ctrl_c().await;
        });
    // Failure of the data plane must stop the container, not leave a healthy control plane.
    tokio::select! {
        result = worker => { result??; anyhow::bail!("router worker stopped"); },
        result = server => { result?; },
    }
    Ok(())
}
