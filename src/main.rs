use std::path::PathBuf;

use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::oneshot;
use tonic::transport::Server;
use tracing::{info, warn};

use stationd::grpc::station::station_server::StationServer;
use stationd::{config, db, grpc};

/// stationd — the webradio's core daemon.
/// Owns the station's state, drives Liquidsoap/Icecast (single writer),
/// and serves a gRPC contract for `api` and the CLI (`stationctl`).
#[derive(Parser, Debug)]
#[command(name = "stationd", version, about)]
struct Args {
    /// Path to the TOML config file
    #[arg(short, long, default_value = "stationd.toml")]
    config: PathBuf,
}

// `#[tokio::main]` replaces the previous synchronous `fn main()`: we now
// need an async runtime to run the gRPC server and wait for a shutdown
// signal at the same time (`tokio::select!` below).
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let cfg = config::Config::load(&args.config)?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cfg.logging.level.clone().into()),
        )
        .init();

    info!(station = %cfg.station.name, "stationd starting");
    info!(db_path = ?cfg.database.path, "SQLite database");
    info!(media_path = ?cfg.media.library_path, "media library");
    info!(playlist_path = ?cfg.playlist.path, "playlist directory (source of truth)");

    if !cfg.media.library_path.exists() {
        warn!(
            path = ?cfg.media.library_path,
            "media directory does not exist yet (not a problem for now)"
        );
    }

    if !cfg.playlist.path.exists() {
        warn!(
            path = ?cfg.playlist.path,
            "playlist directory does not exist yet (not a problem for now)"
        );
    }

    let db_pool = db::init(&cfg.database.path).await?;
    info!(
        connections = db_pool.size(),
        "SQLite database ready (migrations applied)"
    );

    // TODO: Liquidsoap/Icecast control — deliberately absent at this stage

    let addr = cfg.server.grpc_bind.parse()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = grpc::StationService::new(
        cfg.station.name.clone(),
        db_pool.clone(),
        cfg.playlist.path.clone(),
        shutdown_tx,
    );

    info!(%addr, "gRPC server listening (status, quit)");

    // Three ways to shut down cleanly: via `stationctl quit` (shutdown_rx,
    // triggered by the service's `quit` handler), or via a signal — Ctrl+C
    // (SIGINT) when interactive, or SIGTERM (what `systemctl stop` sends by
    // default; without this handler, systemd would wait out its timeout and
    // then kill the process forcefully instead of a clean shutdown).
    // All three converge on the same shutdown — `serve_with_shutdown` waits
    // for this future to resolve before tearing down the server.
    let mut sigterm = signal(SignalKind::terminate())?;
    let shutdown_signal = async move {
        tokio::select! {
            _ = shutdown_rx => {
                info!("shutdown requested via the `quit` command");
            }
            _ = tokio::signal::ctrl_c() => {
                info!("shutdown requested (Ctrl+C / SIGINT)");
            }
            _ = sigterm.recv() => {
                info!("shutdown requested (SIGTERM, e.g. systemctl stop)");
            }
        }
    };

    Server::builder()
        .add_service(StationServer::new(service))
        .serve_with_shutdown(addr, shutdown_signal)
        .await?;

    info!("stationd shut down cleanly");

    Ok(())
}
