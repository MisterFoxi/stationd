use std::path::PathBuf;

use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::oneshot;
use tonic::transport::Server;
use tracing::{info, warn};

use stationd::grpc::station::station_server::StationServer;
use stationd::schedule_grpc::schedule::schedule_service_server::ScheduleServiceServer;
use stationd::library_grpc::library::library_service_server::LibraryServiceServer;
use stationd::plugin_grpc::plugin::plugin_service_server::PluginServiceServer;
use stationd::broadcast_grpc::broadcast::broadcast_service_server::BroadcastServiceServer;
use stationd::broadcast_grpc::BroadcastGrpc;
use stationd::ls_grpc::liquidsoap::liquidsoap_service_server::LiquidsoapServiceServer;
use stationd::ls_grpc::LsGrpc;
use stationd::icecast_grpc::icecast::icecast_service_server::IcecastServiceServer;
use stationd::icecast_grpc::IcecastGrpc;
use stationd::grid_engine::GridEngine;
use stationd::station_control::StationControl;
use stationd::library_grpc::LibraryGrpc;
use stationd::plugin_grpc::PluginGrpc;
use stationd::schedule_grpc::ScheduleGrpc;
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
    info!(station_timezone = %cfg.station.timezone, "timezone");

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

    // Station runtime control: broadcast state (restored from the last run —
    // an operator's stop stays a stop), override queue, manual clock. Shared
    // by the grid engine, the plugin host surface and `stationctl station|
    // override|debug`.
    let control = StationControl::load(db_pool.clone()).await?;
    info!(state = control.state().as_str(), "broadcast control ready");

    // Plugin system: single owning actor over the declared plugins. Loads the
    // enabled ones now (a failure is recorded, not fatal), each with its host
    // surface scoped to its declared capabilities. The grid engine and the
    // station control emit events to it (best-effort); it is also driven via
    // `stationctl plugin list|start|stop|restart|reload`.
    let plugins = stationd::plugin::spawn_with(cfg.plugins.clone(), Some(control.clone()));
    control.attach_plugins(plugins.clone());
    let plugin_service = PluginGrpc::new(plugins.clone());
    let mut broadcast_service = BroadcastGrpc::new(control.clone());

    // Grid engine: the live resolver over the SQLite-backed grid, in the
    // station timezone. `sync_grid` reconciles the Every counter rows for the
    // current grid (catch-up on start-up); it never resets an existing counter.
    let engine = GridEngine::new(db_pool.clone(), cfg.station.timezone.clone())
        .with_control(control.clone())
        .with_plugins(plugins.clone())
        .with_media_root(cfg.media.library_path.clone());
    engine.sync_grid().await?;

    // Liquidsoap wiring (optional `[liquidsoap]`): write the generated script
    // (only when it changed — Liquidsoap runs under its own unit and must be
    // restarted to pick it up) and serve the loopback bridge it pulls from.
    // A bind failure is fatal: a configured station that cannot air must not
    // pretend to run.
    let (ls_service, ls_task) = match &cfg.liquidsoap {
        None => {
            info!("no [liquidsoap] section: nothing airs (scheduling only)");
            (LsGrpc::disabled(), None)
        }
        Some(ls_cfg) => {
            let script = stationd::ls_script::render(ls_cfg, &cfg.station.name);
            if stationd::ls_script::write_if_changed(&ls_cfg.script_path, &script)? {
                warn!(path = ?ls_cfg.script_path, "Liquidsoap script (re)written: restart Liquidsoap to apply it");
            } else {
                info!(path = ?ls_cfg.script_path, "Liquidsoap script unchanged");
            }
            for (what, p) in [("fallback_path", &ls_cfg.fallback_path), ("halted_path", &ls_cfg.halted_path)] {
                if !p.exists() {
                    warn!(path = ?p, "[liquidsoap] {what} does not exist: Liquidsoap will refuse to start");
                }
            }
            let bridge = stationd::ls_bridge::LsBridge::new(engine.clone(), &cfg.media.library_path)?;
            // Control socket: pause/resume follow the broadcast state machine
            // (whoever changes it); skip is a direct RPC.
            let ls_control = stationd::ls_control::LsControl::new(std::path::absolute(&ls_cfg.control_socket)?);
            let (air_tx, air_rx) = tokio::sync::mpsc::unbounded_channel();
            control.attach_air(air_tx.clone());
            stationd::ls_control::spawn_air_sync(air_rx, ls_control.clone(), bridge.clone(), control.state());
            // AtClock hard: a timer cuts the rendez-vous in at the mark.
            stationd::ls_control::spawn_at_clock_ticker(engine.clone(), air_tx);
            broadcast_service = broadcast_service.with_liquidsoap(ls_control.clone());
            let router = stationd::ls_bridge::router(bridge.clone(), &ls_cfg.api_token);
            let listener = tokio::net::TcpListener::bind(ls_cfg.http_addr()).await?;
            info!(addr = %ls_cfg.http_bind, "Liquidsoap bridge listening (loopback)");
            let task = tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!(error = %e, "Liquidsoap bridge stopped");
                }
            });
            (LsGrpc::new(ls_cfg.clone(), script, bridge, ls_control), Some(task))
        }
    };

    // Icecast (optional `[icecast]`, requires `[liquidsoap]` — checked at
    // load): sample the audience of our mounts from the admin API. A failed
    // read makes the audience unknown, never zero (a draining station keeps
    // playing). `stationctl debug listeners` still injects, until the next
    // sample overwrites it.
    let icecast_monitor = stationd::icecast::IcecastMonitor::default();
    let (icecast_service, icecast_task) = match (&cfg.icecast, &cfg.liquidsoap) {
        (Some(ic), Some(ls_cfg)) => {
            for o in ic.foreign_outputs(ls_cfg) {
                warn!(
                    mount = %o.mount,
                    output = %format!("{}:{}", o.host, o.port),
                    admin_url = %ic.admin_url,
                    "[icecast] output on another host:port than admin_url: is it the same Icecast?"
                );
            }
            let client = stationd::icecast::IcecastClient::new(ic).map_err(anyhow::Error::msg)?;
            let mounts: Vec<String> = ls_cfg.outputs.iter().map(|o| o.mount.clone()).collect();
            info!(icecast = client.authority(), ?mounts, every_s = ic.poll_interval, "Icecast audience sampling");
            let mut service = IcecastGrpc::new(
                icecast_monitor.clone(),
                client.authority().to_string(),
                ic.poll_interval as u32,
                mounts.clone(),
            );
            // `[icecast.server]`: stationd owns Icecast's config. Written only
            // when it changed; Icecast runs under its own unit and must be
            // restarted to pick it up (stationd never launches it).
            if let Some(srv) = &ic.server {
                let xml = stationd::icecast_xml::render(ic, srv, ls_cfg, &cfg.station.name);
                if stationd::icecast_xml::write_if_changed(&srv.config_path, &xml)? {
                    warn!(path = ?srv.config_path, "Icecast config (re)written: restart Icecast to apply it");
                } else {
                    info!(path = ?srv.config_path, "Icecast config unchanged");
                }
                // Holds the passwords (0640): Icecast reads it through the
                // group. Set at every start; failure = no start (Icecast
                // could not read its own config).
                if let Some(group) = &srv.file_group {
                    stationd::icecast_xml::set_group(&srv.config_path, group).map_err(anyhow::Error::msg)?;
                }
                let access = stationd::icecast_xml::access(&srv.config_path).map_err(anyhow::Error::msg)?;
                info!(path = ?srv.config_path, %access, "Icecast config access: Icecast's user must be in this group");
                if !srv.share_dir.join("web").is_dir() {
                    warn!(path = ?srv.share_dir, "[icecast.server] share_dir has no web/: is Icecast installed there?");
                }
                service = service.with_config(xml, srv.config_path.to_string_lossy().into_owned());
            }
            let task = stationd::icecast::spawn_sampler(
                client,
                mounts,
                std::time::Duration::from_secs(ic.poll_interval),
                control.clone(),
                icecast_monitor.clone(),
            );
            (service, Some(task))
        }
        _ => {
            info!("no [icecast] section: audience never sampled (stationctl debug listeners only)");
            (IcecastGrpc::disabled(), None)
        }
    };

    let schedule_service = ScheduleGrpc::new(engine);

    // Media library: single owning actor over the `media` view. The heavy scan
    // runs off the async runtime (spawn_blocking); scans are serialised by the
    // actor's command loop. Each scan goes through the plugins' `on_scan`
    // (e.g. `TXXX:Type` → genre) before indexing. Reachable via `stationctl
    // library scan|list`.
    let library = stationd::library_actor::spawn_with(
        db_pool.clone(),
        cfg.media.library_path.clone(),
        Some(plugins),
    );
    let library_service = LibraryGrpc::new(library);

    let addr = cfg.server.grpc_bind.parse()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let service = grpc::StationService::new(
        cfg.station.name.clone(),
        cfg.station.timezone.clone(),
        db_pool.clone(),
        cfg.playlist.path.clone(),
        shutdown_tx,
    );

    info!(%addr, "gRPC server listening (status, quit, schedule, library, plugin, broadcast, liquidsoap, icecast)");

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
        .add_service(ScheduleServiceServer::new(schedule_service))
        .add_service(LibraryServiceServer::new(library_service))
        .add_service(PluginServiceServer::new(plugin_service))
        .add_service(BroadcastServiceServer::new(broadcast_service))
        .add_service(LiquidsoapServiceServer::new(ls_service))
        .add_service(IcecastServiceServer::new(icecast_service))
        .serve_with_shutdown(addr, shutdown_signal)
        .await?;

    // The bridge only answers short requests: stop it with the daemon.
    if let Some(task) = ls_task {
        task.abort();
    }
    if let Some(task) = icecast_task {
        task.abort();
    }

    // The override queue is volatile (in memory): say what is lost, never
    // drop it silently.
    let pending = control.list_overrides().len();
    if pending > 0 {
        warn!(pending, "pending overrides discarded at shutdown (the queue is volatile)");
    }

    info!("stationd shut down cleanly");

    Ok(())
}
