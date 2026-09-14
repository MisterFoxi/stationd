// stationctl — minimal CLI, a direct gRPC client of stationd (no HTTP layer
// in between, per the architecture doc). For now the address is a hardcoded
// default; we'll wire up reading stationd.toml later if the need becomes
// real, rather than doing it ahead of time.

use clap::{Parser, Subcommand};

use stationd::proto::{schedule, station};

use station::station_client::StationClient;
use station::{PlaylistAddRequest, PlaylistListRequest, PlaylistSyncRequest, QuitRequest, StatusRequest};
use schedule::schedule_service_client::ScheduleServiceClient;
use schedule::{ApplyGridRequest, ExportGridRequest, GridFile, PreviewRequest, ResolveNextRequest};

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
    /// Grid / scheduler operations
    #[command(subcommand)]
    Schedule(ScheduleCommand),
}

#[derive(Subcommand, Debug)]
enum ScheduleCommand {
    /// List the grid rules without advancing playback state.
    List,
    /// Resolve which source the grid would pull now (or at a given instant).
    /// This is the live resolver path, so it persists side effects (a consumed
    /// AtClock mark, an Every reset) exactly as a real track boundary would.
    Next {
        /// Evaluate at this instant (epoch seconds, UTC) instead of now.
        /// RFC3339 parsing can come later; epoch keeps the client dependency-free.
        #[arg(long)]
        at: Option<i64>,
    },
    /// Validate a grid.toml without installing it (parse + kind gating +
    /// ref resolution). Writes nothing; a rejected grid exits non-zero.
    Validate {
        /// Path to the grid .toml file
        path: PathBuf,
    },
    /// Validate and install a grid.toml: rebuilds the rule index (family A),
    /// preserving the durable playback state (family B). Rejects atomically.
    Apply {
        /// Path to the grid .toml file
        path: PathBuf,
    },
    /// Export the current grid back to TOML (stdout, or a file with --out).
    Export {
        /// Only export these rule ids (repeatable). Omitted → the whole grid.
        #[arg(long = "rule")]
        rules: Vec<String>,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Project the grid over a window without waiting for the wall clock.
    /// Each occurrence is shown in epoch UTC and station-local time (the
    /// anti-DST view). Playback-driven `every` rules are not projected.
    Preview {
        /// Start instant (epoch seconds, UTC). Default: now (server-side).
        #[arg(long)]
        at: Option<i64>,
        /// Window length in seconds. Default: 24h.
        #[arg(long, default_value_t = 86_400)]
        window: i64,
    },
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
    /// List the playlists currently in stationd's view.
    List,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut client = StationClient::connect(args.addr.clone()).await?;

    match args.command {
        Command::Status => {
            let reply = client.status(StatusRequest {}).await?.into_inner();
            println!("station:  {}", reply.station_name);
            println!("timezone: {}", reply.timezone);
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
        Command::Playlist(PlaylistCommand::List) => {
            let reply = client
                .playlist_list(PlaylistListRequest {})
                .await?
                .into_inner();

            if reply.playlists.is_empty() {
                println!("(no playlists in the view)");
            } else {
                for p in &reply.playlists {
                    let handle = if p.rel_path.is_empty() {
                        "(no path)"
                    } else {
                        &p.rel_path
                    };
                    let state = if p.enabled { "enabled" } else { "disabled" };
                    println!("{handle}  [{}]  {}  ({state})  {}", p.mode, p.name, p.id);
                }
            }
        }
        Command::Schedule(ScheduleCommand::List) => {
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            let reply = sched.list_rules(schedule::ListRulesRequest {}).await?.into_inner();
            if reply.rules.is_empty() {
                println!("(no rules in the view)");
            }
            for rule in reply.rules {
                println!("{rule:#?}");
            }
        }
        Command::Schedule(ScheduleCommand::Next { at }) => {
            // The scheduler lives behind its own service on the same server.
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            let now = at.map(|seconds| ::prost_types::Timestamp { seconds, nanos: 0 });
            let reply = sched
                .resolve_next(ResolveNextRequest { now })
                .await?
                .into_inner();

            let origin = schedule::decision::Origin::try_from(reply.origin)
                .map(|o| o.as_str_name())
                .unwrap_or("UNKNOWN");
            println!("origin:        {origin}");
            println!(
                "playlist_ref:  {}",
                if reply.playlist_ref.is_empty() { "(fallback)" } else { &reply.playlist_ref }
            );
            if !reply.rule_id.is_empty() {
                println!("rule:          {}", reply.rule_id);
            }
        }
        Command::Schedule(ScheduleCommand::Validate { path }) => {
            let content = read_grid_toml(&path)?;
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            // A rejected grid comes back as a gRPC error (invalid_argument);
            // `?` surfaces the joined diagnostics and exits non-zero.
            sched
                .validate_grid(ApplyGridRequest {
                    files: vec![GridFile {
                        path: path.display().to_string(),
                        toml: content,
                    }],
                })
                .await?;
            println!("valid:  {}", path.display());
        }
        Command::Schedule(ScheduleCommand::Apply { path }) => {
            let content = read_grid_toml(&path)?;
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            let reply = sched
                .apply_grid(ApplyGridRequest {
                    files: vec![GridFile {
                        path: path.display().to_string(),
                        toml: content,
                    }],
                })
                .await?
                .into_inner();
            println!("applied: {}", path.display());
            println!("rules:   {}", reply.applied_rule_ids.len());
            for id in &reply.applied_rule_ids {
                println!("  - {id}");
            }
        }
        Command::Schedule(ScheduleCommand::Export { rules, out }) => {
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            let reply = sched
                .export_grid(ExportGridRequest { rule_ids: rules })
                .await?
                .into_inner();
            // The server returns a single grid.toml; join defensively in case
            // that ever changes.
            let toml = reply
                .files
                .into_iter()
                .map(|f| f.toml)
                .collect::<Vec<_>>()
                .join("\n");
            match out {
                Some(path) => {
                    std::fs::write(&path, &toml)
                        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
                    println!("exported: {}", path.display());
                }
                None => print!("{toml}"),
            }
        }
        Command::Schedule(ScheduleCommand::Preview { at, window }) => {
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            let reply = sched
                .preview(PreviewRequest {
                    from: at.map(|seconds| ::prost_types::Timestamp { seconds, nanos: 0 }),
                    window: Some(::prost_types::Duration { seconds: window, nanos: 0 }),
                })
                .await?
                .into_inner();
            if reply.occurrences.is_empty() {
                println!("(no occurrences in the window)");
            }
            for o in reply.occurrences {
                let origin = schedule::decision::Origin::try_from(o.origin)
                    .map(|x| x.as_str_name())
                    .unwrap_or("UNKNOWN");
                let utc = o.at_utc.map(|t| t.seconds).unwrap_or(0);
                let pl = if o.playlist_ref.is_empty() {
                    "(fallback)".to_string()
                } else {
                    o.playlist_ref
                };
                let rule = if o.rule_id.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", o.rule_id)
                };
                println!("{utc:>11}  {}  {origin:<13}  {pl}{rule}", o.at_local);
            }
        }
    }

    Ok(())
}

/// Read a grid TOML file and fail fast if it isn't well-formed TOML. Business
/// validation (kinds, refs) happens in stationd; this only avoids a pointless
/// round-trip on an obviously broken file, mirroring `playlist add`.
fn read_grid_toml(path: &std::path::Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    content
        .parse::<toml::Table>()
        .map_err(|e| anyhow::anyhow!("{} is not well-formed TOML: {e}", path.display()))?;
    Ok(content)
}
