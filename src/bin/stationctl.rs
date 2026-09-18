// stationctl — minimal CLI, a direct gRPC client of stationd (no HTTP layer
// in between, per the architecture doc). For now the address is a hardcoded
// default; we'll wire up reading stationd.toml later if the need becomes
// real, rather than doing it ahead of time.

use clap::{Parser, Subcommand};

use stationd::proto::{library, plugin, schedule, station};

use station::station_client::StationClient;
use station::{PlaylistAddRequest, PlaylistListRequest, PlaylistSyncRequest, QuitRequest, StatusRequest};
use schedule::schedule_service_client::ScheduleServiceClient;
use schedule::{ApplyGridRequest, ExportGridRequest, GridFile, PreviewRequest, ResolveNextRequest, SetClockRequest};
use library::library_service_client::LibraryServiceClient;
use library::{ListMediaRequest, ScanRequest};
use plugin::plugin_service_client::PluginServiceClient;
use plugin::{plugin_control_request::Action as PluginAction, PluginControlRequest, PluginListRequest};

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
    /// Media library operations
    #[command(subcommand)]
    Library(LibraryCommand),
    /// Plugin operations
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Manual clock (testing)
    #[command(subcommand)]
    Clock(ClockCommand),
}

#[derive(Subcommand, Debug)]
enum ClockCommand {
    /// Show the effective clock
    Show,
    /// Freeze the clock at a civil local time: "HH:MM" (today) or
    /// "YYYY-MM-DD HH:MM"
    Set { when: String },
    /// Return to real time
    Reset,
}

#[derive(Subcommand, Debug)]
enum PluginCommand {
    /// List all declared plugins and their state
    List,
    /// Activate a stopped or failed plugin (on_load)
    Start { name: String },
    /// Deactivate a loaded plugin (on_unload)
    Stop { name: String },
    /// Stop then start, same artefact
    Restart { name: String },
    /// Reload the artefact from disk (== restart in native)
    Reload { name: String },
}

#[derive(Subcommand, Debug)]
enum LibraryCommand {
    /// Scan the configured media root and reconcile the index. Heavy work runs
    /// off the async runtime server-side; a skipped audio file is reported,
    /// not fatal (the scan itself succeeds).
    Scan,
    /// List the media index. Available files only, unless --all also shows
    /// vanished-but-known files (available = 0).
    List {
        /// Include vanished-but-known files.
        #[arg(long)]
        all: bool,
    },
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
    /// anti-DST view). An elapsed `every` is projected at its cadence; a
    /// track-counted `every` can't be timed on a clock, so it is listed
    /// separately as indicative.
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
            if !reply.media_path.is_empty() {
                println!("media:         {}", reply.media_path);
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
            // Marks (AtClock/Every) are instants → always shown. Base/DayPart
            // are segments: a base that merely *resumes* after a mark (same
            // playlist as the current segment) is not reprinted, otherwise the
            // floor reappears after every jingle and drowns the timeline. A
            // real change of segment (DayPart start/end, floor→floor swap) is
            // still shown.
            use schedule::decision::Origin as O;
            let mut last_segment: Option<(i32, String)> = None;
            for o in reply.occurrences {
                let parsed = O::try_from(o.origin).ok();
                let is_instant =
                    matches!(parsed, Some(O::AtClockHard | O::AtClockSoft | O::Every));
                if !is_instant {
                    let key = (o.origin, o.playlist_ref.clone());
                    if last_segment.as_ref() == Some(&key) {
                        continue;
                    }
                    last_segment = Some(key);
                }
                let origin = parsed.map(|x| x.as_str_name()).unwrap_or("UNKNOWN");
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
            // Rules taken into account but not placeable on a clock (a
            // track-counted `every`, cadence driven by playback): listed once
            // at the end, apart from the ordered timeline.
            if !reply.indicative.is_empty() {
                println!();
                println!("indicative (playback-driven, not projected):");
                for r in reply.indicative {
                    let pl = if r.playlist_ref.is_empty() {
                        "(fallback)".to_string()
                    } else {
                        r.playlist_ref
                    };
                    let rule = if r.rule_id.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", r.rule_id)
                    };
                    println!("  - EVERY  {pl}{rule}");
                }
            }
        }
        Command::Library(LibraryCommand::Scan) => {
            let mut lib = LibraryServiceClient::connect(args.addr.clone()).await?;
            let reply = lib.scan(ScanRequest {}).await?.into_inner();
            println!("found:       {}", reply.found);
            println!("skipped:     {}", reply.skipped);
            println!("present:     {}", reply.present);
            println!("unavailable: {}", reply.unavailable);
            if !reply.skips.is_empty() {
                println!("skips:");
                for s in &reply.skips {
                    let reason = library::skip::Reason::try_from(s.reason)
                        .map(|r| r.as_str_name())
                        .unwrap_or("UNKNOWN");
                    let detail = if s.detail.is_empty() {
                        String::new()
                    } else {
                        format!("  ({})", s.detail)
                    };
                    println!("  - {reason:<13} {}{detail}", s.path);
                }
            }
            // A skipped audio file is diagnostic, not a failure: exit zero.
        }
        Command::Library(LibraryCommand::List { all }) => {
            let mut lib = LibraryServiceClient::connect(args.addr.clone()).await?;
            let reply = lib
                .list_media(ListMediaRequest { only_available: !all })
                .await?
                .into_inner();
            if reply.media.is_empty() {
                println!("(no media in the index)");
            }
            for m in &reply.media {
                let secs = m.duration_ms / 1000;
                let dur = format!("{}:{:02}", secs / 60, secs % 60);
                let flag = if m.available { "" } else { "  (unavailable)" };
                let who = match (m.artist.is_empty(), m.title.is_empty()) {
                    (false, false) => format!("{} \u{2014} {}", m.artist, m.title),
                    (true, false) => m.title.clone(),
                    _ => "(no tags)".to_string(),
                };
                println!("{dur:>7}  {}  {who}{flag}", m.rel_path);
            }
        }
        Command::Plugin(PluginCommand::List) => {
            let mut cli = PluginServiceClient::connect(args.addr.clone()).await?;
            let reply = cli.list(PluginListRequest {}).await?.into_inner();
            if reply.plugins.is_empty() {
                println!("(no plugins declared)");
            }
            for p in &reply.plugins {
                let en = if p.enabled { "enabled" } else { "disabled" };
                let reason = if p.reason.is_empty() {
                    String::new()
                } else {
                    format!("  \u{2014} {}", p.reason)
                };
                println!(
                    "{:<16} {:<12} {:<9} order={:<3} failures={}{reason}",
                    p.name, p.state, en, p.order, p.failures
                );
            }
        }
        Command::Plugin(cmd) => {
            let (name, action) = match &cmd {
                PluginCommand::Start { name } => (name, PluginAction::Start),
                PluginCommand::Stop { name } => (name, PluginAction::Stop),
                PluginCommand::Restart { name } => (name, PluginAction::Restart),
                PluginCommand::Reload { name } => (name, PluginAction::Reload),
                PluginCommand::List => unreachable!("handled above"),
            };
            let mut cli = PluginServiceClient::connect(args.addr.clone()).await?;
            let reply = cli
                .control(PluginControlRequest {
                    name: name.clone(),
                    action: action as i32,
                })
                .await?
                .into_inner();
            if let Some(p) = reply.plugin {
                let reason = if p.reason.is_empty() {
                    String::new()
                } else {
                    format!("  \u{2014} {}", p.reason)
                };
                println!("{}: {}{reason}", p.name, p.state);
            }
        }
        Command::Clock(cmd) => {
            let req = match &cmd {
                ClockCommand::Show => SetClockRequest { real: false, at: String::new() },
                ClockCommand::Reset => SetClockRequest { real: true, at: String::new() },
                ClockCommand::Set { when } => SetClockRequest { real: false, at: when.clone() },
            };
            let mut sched = ScheduleServiceClient::connect(args.addr.clone()).await?;
            let status = sched.set_clock(req).await?.into_inner();
            let state = if status.frozen { "FROZEN" } else { "real time" };
            println!("clock: {state} \u{2014} {}", status.effective_local);
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
