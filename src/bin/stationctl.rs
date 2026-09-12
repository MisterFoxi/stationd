// stationctl — minimal CLI, a direct gRPC client of stationd (no HTTP layer
// in between, per the architecture doc). For now the address is a hardcoded
// default; we'll wire up reading stationd.toml later if the need becomes
// real, rather than doing it ahead of time.

use clap::{Parser, Subcommand};

pub mod station {
    tonic::include_proto!("station");
}

use station::station_client::StationClient;
use station::{PlaylistAddRequest, PlaylistSyncRequest, QuitRequest, StatusRequest};

use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "stationctl", version, about = "Minimal CLI to control stationd")]
struct Args {
    /// gRPC address of stationd
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    addr: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show stationd's status (name, uptime, pid)
    Status,
    /// Ask stationd to shut down cleanly
    Quit,
    /// Playlist operations
    #[command(subcommand)]
    Playlist(PlaylistCommand),
}

#[derive(Subcommand, Debug)]
enum PlaylistCommand {
    /// Register a playlist TOML file: stationd validates it, assigns a
    /// UUID, updates its view, and the file is rewritten in place with the id.
    Add {
        /// Path to the playlist .toml file
        path: PathBuf,
    },
    /// Reconcile every *.toml under stationd's playlist root (recursive)
    /// into its view. Best-effort: bad files are reported, not fatal.
    Sync,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut client = StationClient::connect(args.addr).await?;

    match args.command {
        Command::Status => {
            let reply = client.status(StatusRequest {}).await?.into_inner();
            println!("station:  {}", reply.station_name);
            println!("uptime:   {}s", reply.uptime_seconds);
            println!("pid:      {}", reply.pid);
        }
        Command::Quit => {
            client.quit(QuitRequest {}).await?;
            println!("shutdown requested");
        }
        Command::Playlist(PlaylistCommand::Add { path }) => {
            // Syntactic pre-check only: is this readable, well-formed TOML?
            // The business validation happens in stationd. Failing fast here
            // avoids a pointless round-trip on an obviously broken file.
            let content = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
            content
                .parse::<toml::Table>()
                .map_err(|e| anyhow::anyhow!("{} is not well-formed TOML: {e}", path.display()))?;

            let reply = client
                .playlist_add(PlaylistAddRequest {
                    toml_content: content,
                })
                .await?
                .into_inner();

            // stationd is authoritative on the content; write back what it
            // returned (the id-injected, losslessly-rewritten TOML).
            std::fs::write(&path, &reply.toml_content)
                .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;

            println!("added:  {}", path.display());
            println!("id:     {}", reply.id);
        }
        Command::Playlist(PlaylistCommand::Sync) => {
            // No path argument: stationd scans its own configured playlist
            // root. The report is best-effort — successes counted, failures
            // listed loudly (no-silent-failure).
            let reply = client
                .playlist_sync(PlaylistSyncRequest {})
                .await?
                .into_inner();

            println!("synced: {} playlist(s)", reply.added);
            if reply.errors.is_empty() {
                println!("errors: none");
            } else {
                println!("errors: {}", reply.errors.len());
                for e in &reply.errors {
                    println!("  - {}: {}", e.path, e.message);
                }
                // Non-zero exit so scripts / CI notice something was rejected.
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
