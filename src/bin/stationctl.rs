// stationctl — direct gRPC client of stationd. An explicit --addr wins;
// otherwise use server.grpc_bind from the local stationd.toml.

use clap::{Parser, Subcommand};

#[path = "stationctl/address.rs"]
mod address;

#[path = "stationctl/listeners.rs"]
mod listeners;

use stationd::proto::{broadcast, events, icecast, library, liquidsoap, live, onair, playlist, plugin, schedule, station, stats};
use events::event_service_client::EventServiceClient;

use station::station_client::StationClient;
use station::{QuitRequest, ShutdownRequest, StatusRequest};
use playlist::playlist_service_client::PlaylistServiceClient;
use schedule::schedule_service_client::ScheduleServiceClient;
use schedule::{ApplyGridRequest, CheckCoverageRequest, EnqueueRequest, ExportGridRequest, GridFile, PreviewRequest, ResolveNextRequest, SetClockRequest};
use library::library_service_client::LibraryServiceClient;
use library::{ListGenresRequest, ListMediaRequest, ScanRequest};
use plugin::plugin_service_client::PluginServiceClient;
use plugin::{
    plugin_control_request::Action as PluginAction, plugin_db_value::Kind as DbKind,
    PluginControlRequest, PluginDbInfoRequest, PluginDbQueryRequest, PluginDbResetRequest,
    PluginListRequest,
};
use broadcast::broadcast_service_client::BroadcastServiceClient;
use liquidsoap::liquidsoap_service_client::LiquidsoapServiceClient;
use liquidsoap::{GetStatusRequest as LsStatusRequest, RenderScriptRequest};
use icecast::icecast_service_client::IcecastServiceClient;
use icecast::{GetStatusRequest as IcecastStatusRequest, RenderConfigRequest as IcecastRenderRequest};
use live::live_service_client::LiveServiceClient;
use stats::stats_service_client::StatsServiceClient;
use onair::on_air_service_client::OnAirServiceClient;
use stats::{plays_request::By as PlaysBy, PlaysRequest};
use live::{CloseRequest as LiveCloseRequest, GetStatusRequest as LiveStatusRequest, HashPasswordRequest, KickRequest, OpenRequest as LiveOpenRequest};
use broadcast::{
    control_request::Action as BroadcastAction, push_override_request, ClearOverridesRequest,
    ControlRequest, GetStateRequest, ListOverridesRequest, PushOverrideRequest,
    SampleListenersRequest, SkipRequest,
};

use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "stationctl", version, about = "Minimal CLI to control stationd")]
struct Args {
    /// gRPC address (default: local stationd.toml server.grpc_bind, else loopback)
    #[arg(long)]
    addr: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show stationd's status (name, uptime, pid)
    Status,
    /// Ask stationd to exit cleanly — its supervisor starts it again (a
    /// restart). To stop it for good: `station stop`
    Quit,
    /// Playlist operations
    #[command(subcommand)]
    Playlist(PlaylistCommand),
    /// Grid / scheduler operations
    #[command(subcommand)]
    Schedule(ScheduleCommand),
    /// Runtime queue operations (audience requests / DJ injection)
    #[command(subcommand)]
    Queue(QueueCommand),
    /// Media library operations
    #[command(subcommand)]
    Library(LibraryCommand),
    /// Plugin operations
    #[command(subcommand)]
    Plugin(PluginCommand),
    /// Listener counts by geographic region (listener-stats plugin)
    #[command(subcommand)]
    Listeners(listeners::ListenersCommand),
    /// Manual clock (testing)
    #[command(subcommand)]
    Clock(ClockCommand),
    /// Station control: state, stop / start (stationd itself), pause / resume,
    /// stop-when-idle / wake
    #[command(subcommand)]
    Station(StationCommand),
    /// Override queue: content pushed ahead of the grid
    #[command(subcommand)]
    Override(OverrideCommand),
    /// Test injection (a listener sample is overwritten by the next Icecast sample)
    #[command(subcommand)]
    Debug(DebugCommand),
    /// Liquidsoap wiring: generated script, bridge status
    #[command(subcommand)]
    Ls(LsCommand),
    /// Icecast as stationd reads it: audience, health of our mounts
    #[command(subcommand)]
    Icecast(IcecastCommand),
    /// Live DJ (harbor, [live]): who is on air, end a live, ad-hoc openings
    #[command(subcommand)]
    Live(LiveCommand),
    /// DJ file helpers ([live] djs_path)
    #[command(subcommand)]
    Dj(DjCommand),
    /// Broadcast statistics: plays grouped by playlist, rule, media…
    /// (`aired` = really started by Liquidsoap, `picked` = chosen by stationd)
    /// The air: what plays, what is prepared, what should follow (a
    /// simulation, `~`), what played, which playlist leads and which follow
    Onair {
        #[command(subcommand)]
        what: Option<OnairCommand>,
        /// Tracks to come, the prepared one included (max 30)
        #[arg(long, default_value_t = 10)]
        upcoming: u32,
        /// Last tracks played (max 100)
        #[arg(long, default_value_t = 10)]
        history: u32,
        /// Playlists to come (max 20)
        #[arg(long, default_value_t = 5)]
        playlists: u32,
        /// Keep printing a new snapshot at every change (Ctrl+C to stop)
        #[arg(long)]
        follow: bool,
    },
    /// The station journal: typed facts (state, grid, scan, tags, plugins…)
    /// and every warn / error line, oldest first
    Events {
        /// How many past events (max 2000)
        #[arg(long, default_value_t = 50)]
        last: u32,
        /// Keep printing events as they happen (Ctrl+C to stop)
        #[arg(long)]
        follow: bool,
        /// Only this level and above
        #[arg(long, value_enum)]
        level: Option<EventLevel>,
    },
    Stats {
        /// Window up to now: 30m, 24h, 7d…
        #[arg(long, default_value = "24h")]
        since: String,
        /// Grouping
        #[arg(long, value_enum, default_value = "playlist")]
        by: StatsBy,
        /// Max lines (0 = all)
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Only this key of the grouping (e.g. one media path with --by media)
        #[arg(long, default_value = "")]
        key: String,
    },
}

#[derive(Subcommand, Debug)]
enum OnairCommand {
    /// Tracks really aired, most recent first (from the broadcast log)
    History {
        /// Only tracks aired strictly before this instant (epoch UTC, seconds)
        #[arg(long)]
        before: Option<i64>,
        /// How many (0 = 50, max 500)
        #[arg(long, default_value_t = 30)]
        limit: u32,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum EventLevel {
    Info,
    Warn,
    Error,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum StatsBy {
    /// The rule's / override's playlist (a group counts as the group)
    Playlist,
    /// The leaf playlist that produced the file (a group's member)
    Leaf,
    /// Grid rule
    Rule,
    /// AtClockHard, Every, BaseRotation, Override…
    Origin,
    Media,
    Artist,
}

#[derive(Subcommand, Debug)]
enum LiveCommand {
    /// The DJ on air, the last live, openings, urgent rights, refused DJs
    Status,
    /// End the live now: the DJ is disconnected; the way it came in closes
    /// (slot / opening: until its end; urgent right: [live] urgent_cooldown)
    Kick,
    /// Let a DJ connect now, outside the grid, for a while (persisted)
    Open {
        /// DJ id (DJ file)
        dj: String,
        /// How long the opening lasts: 30m, 2h, 1d… (1m to 7d)
        #[arg(long = "for", value_name = "DURATION")]
        duration: String,
    },
    /// End a DJ's opening now (a DJ already on air stays: use kick)
    Close {
        /// DJ id (DJ file)
        dj: String,
    },
}

#[derive(Subcommand, Debug)]
enum DjCommand {
    /// Hash a password for the DJ file (`password_hash`). The password is read
    /// from standard input (not echoed on a terminal), never from the command line.
    Hash,
}

#[derive(Subcommand, Debug)]
enum IcecastCommand {
    /// Last read of /admin/stats: audience, source, bitrate, listeners, title
    Status,
    /// Print the generated icecast.xml ([icecast.server], as written at start-up)
    Render,
}

#[derive(Subcommand, Debug)]
enum LsCommand {
    /// Print the generated Liquidsoap script (as written at stationd start-up)
    Render,
    /// Bridge status: last pull from Liquidsoap, what is really on air
    Status,
}

#[derive(Subcommand, Debug)]
enum StationCommand {
    /// Show the broadcast state and the last listener sample (stationd
    /// stopped by the operator: says so, exit code 3)
    State {
        /// stationd's working directory, where data/stationd.stopped lives
        /// (default: $STATIOND_ROOT, else the current directory)
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Stop stationd itself: the current track plays to its end, then the
    /// background noise; its supervisor (s6) does not start it again — not
    /// even after a restart — until `station start`. Refused while a DJ is on
    /// air, unless --force (the DJ is disconnected first)
    Stop {
        #[arg(long)]
        force: bool,
    },
    /// Start stationd again after `station stop` (in the container: removes
    /// data/stationd.stopped, restarts the s6 service, waits until it answers)
    Start {
        /// stationd's working directory (default: $STATIOND_ROOT, else the
        /// current directory)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Seconds to wait for stationd to answer
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Pause now: current track frozen, background noise on air (Liquidsoap)
    Pause,
    /// Resume: the frozen track plays on; from sleep, the slot of now (also
    /// cancels an armed stop-when-idle)
    Resume,
    /// Skip to the next track now
    #[command(alias = "skip")]
    Next,
    /// Arm the idle sleep: at the next track boundary with zero listeners
    StopWhenIdle,
    /// Wake a sleeping station (no-op in any other state: never un-pauses)
    Wake,
}

#[derive(Subcommand, Debug)]
enum OverrideCommand {
    /// Push a media or a playlist ahead of the grid
    Push {
        /// Media path under the media root (need not be indexed; checked on disk
        /// when it airs)
        #[arg(long, conflicts_with = "playlist", required_unless_present = "playlist")]
        media: Option<String>,
        /// Playlist ref, resolved when it airs
        #[arg(long)]
        playlist: Option<String>,
        /// Cut the current track now (degraded to soft without Liquidsoap, or
        /// while the station is paused/sleeping)
        #[arg(long)]
        hard: bool,
        /// Staleness window from now, e.g. 30s, 5m, 2h (default: never stale)
        #[arg(long)]
        expiry: Option<String>,
        /// Playlist only: number of tracks it holds the air. Default: a
        /// sequence/shuffle group plays its whole cycle (from the top), any
        /// other playlist 1 track; on such a group it is a cap
        #[arg(long)]
        tracks: Option<u32>,
    },
    /// List pending overrides, in play order
    List,
    /// Remove one override (--id) or all
    Clear {
        #[arg(long)]
        id: Option<u64>,
    },
}

#[derive(Subcommand, Debug)]
enum DebugCommand {
    /// Inject a listener sample (emits ListenersSampled; overwritten by the next Icecast sample)
    Listeners { count: u32 },
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
    /// The plugin's own database (capability `db`)
    Db {
        name: String,
        #[command(subcommand)]
        cmd: PluginDbCommand,
    },
}

#[derive(Subcommand, Debug)]
enum PluginDbCommand {
    /// File, size, schema version, tables and bounds
    Info,
    /// Run one read-only SQL statement (separate read-only connection)
    Query { sql: String },
    /// Delete the database (plugin must be stopped; its next start recreates
    /// it and re-applies its migrations)
    Reset {
        /// Confirm the deletion (irreversible)
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
enum LibraryCommand {
    /// Move media into genre/style folders using the AI genre (Electronic---House)
    Reorganize {
        /// Preview moves and conflicts without changing disk or index
        #[arg(long)]
        dry_run: bool,
    },
    /// Scan the configured media root and reconcile the index. Heavy work runs
    /// off the async runtime server-side; a skipped audio file is reported,
    /// not fatal (the scan itself succeeds).
    Scan {
        /// Show the progress on stderr while it runs
        #[arg(long)]
        progress: bool,
        /// Force la ré-analyse offline (Essentia) de tous les fichiers
        #[arg(long)]
        reanalyze: bool,
    },
    /// Where the scan is (phase, files read / found) and how the last one ended
    ScanStatus {
        /// Keep printing each change (Ctrl+C to stop)
        #[arg(long)]
        follow: bool,
    },
    /// Values per origin among the available media: the file's genre, then
    /// each custom-tags source (Type…), with counts, spellings, media without
    Values,
    /// Rename a value of one origin across the library (merged into `to` if
    /// it exists), file by file; --dry-run shows the files and the playlists
    /// whose genre filters name it
    Rename {
        from: String,
        to: String,
        /// The custom-tags source (e.g. Type); default: the file's genre
        #[arg(long, default_value = "")]
        origin: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the standard tags of a media file, read from the file itself,
    /// and their revision (for `library tag --revision`).
    Tags {
        /// Media path, relative to the media root (e.g. `Musique/a.mp3`)
        media: String,
    },
    /// Write standard tags INTO a media file (ID3v2: mp3, wav, aiff); the
    /// index is refreshed. Only the given fields change; an empty value
    /// (`--title ""`, `--year 0`) removes the field.
    Tag {
        /// Media path, relative to the media root
        media: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        artist: Option<String>,
        #[arg(long)]
        album: Option<String>,
        #[arg(long)]
        year: Option<u32>,
        /// Genres of the file (TCON): repeat for several; replaces them all
        #[arg(long = "genre", value_name = "GENRE")]
        genres: Vec<String>,
        /// Remove every genre of the file (TCON)
        #[arg(long, conflicts_with = "genres")]
        no_genre: bool,
        /// A user tag turned into genres by custom-tags, e.g. `Type=talks,news`
        /// (replaces its values; `Type=` removes it). Repeatable
        #[arg(long = "source", value_name = "NAME=V1,V2")]
        sources: Vec<String>,
        /// BPM (TBPM); 0 removes it
        #[arg(long)]
        bpm: Option<u32>,
        /// Manual tempo label (wins over the one derived from the BPM); "" removes it
        #[arg(long)]
        tempo: Option<String>,
        /// Manual creation date, RFC 3339 (wins over the derived one); "" removes it
        #[arg(long)]
        creation: Option<String>,
        /// Refuse if the tags changed since this revision (from `library
        /// tags`); without it, the tags are read just before writing
        #[arg(long)]
        revision: Option<String>,
    },
    /// List the media index. Available files only, unless --all also shows
    /// vanished-but-known files (available = 0).
    List {
        /// Include vanished-but-known files.
        #[arg(long)]
        all: bool,
        /// Keep only media carrying this genre (case-insensitive). Repeatable:
        /// several --genre match ANY of them.
        #[arg(long = "genre", value_name = "GENRE")]
        genres: Vec<String>,
        /// Group the listing by genre (a multi-genre media appears under each;
        /// media without genre under "(no genre)").
        #[arg(long)]
        by_genre: bool,
    },
    /// Genre inventory: media count per genre (case-insensitive), plus the
    /// number of media without genre.
    Genres {
        /// Include vanished-but-known files.
        #[arg(long)]
        all: bool,
    },
    /// Forget the media that vanished from disk (kept as unavailable by the
    /// scans). The play history keeps their path and artist.
    Prune {
        /// Only those last seen more than this long ago (e.g. 30d, 12h)
        #[arg(long)]
        older_than: Option<String>,
    },
    /// Search the index, one page at a time: words (title, artist, album,
    /// path; case-insensitive, accented capitals included), filters, stable sort.
    Search {
        /// Words that must all appear
        #[arg(default_value = "")]
        query: String,
        /// Keep media carrying this genre (repeatable: any of them)
        #[arg(long = "genre", value_name = "GENRE")]
        genres: Vec<String>,
        /// Folder (path prefix), e.g. `Musique/Rock`
        #[arg(long)]
        folder: Option<String>,
        /// Media missing this metadata (repeatable: missing all of them):
        /// title, artist, album, year, genre
        #[arg(long = "missing", value_name = "FIELD")]
        missing: Vec<String>,
        /// Age of the creation date: `<10d` = created less than ten days ago,
        /// `>=30d` = at least thirty days ago (repeatable: all of them).
        /// Units: s, m, h, d. Media without a creation date never match
        #[arg(long = "age", value_name = "OP DURATION", allow_hyphen_values = true)]
        age: Vec<String>,
        /// BPM filter on the analysed value: `>120`, `<=130`, `=128`
        /// (repeatable: all must hold). Media not analysed never match.
        #[arg(long = "bpm", value_name = "OP BPM", allow_hyphen_values = true)]
        bpm: Vec<String>,
        /// Keep media whose Essentia genre contains this (repeatable: any of them)
        #[arg(long = "genre-ai", value_name = "SUBSTR")]
        genre_ai: Vec<String>,
        /// Keep media whose mood contains this (repeatable: any of them)
        #[arg(long = "mood", value_name = "SUBSTR")]
        mood: Vec<String>,
        /// Sort key: path, title, artist, album, year, duration, bpm
        #[arg(long, default_value = "path")]
        sort: String,
        /// Reverse order
        #[arg(long)]
        desc: bool,
        /// Page size (default 50, max 500)
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Resume after this cursor (printed at the end of a page)
        #[arg(long)]
        cursor: Option<String>,
        /// Include vanished-but-known files.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ScheduleCommand {
    /// List the grid rules without advancing playback state.
    List {
        /// List this grid file of the node instead of the applied grid.
        #[arg(long)]
        grid: Option<String>,
    },
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
    /// Validate and install a grid.toml: it becomes the ACTIVE grid file of
    /// the node (written by stationd, replaced) and is applied — the rule
    /// index is rebuilt (family A), the durable playback state kept (family
    /// B). Rejects atomically.
    Apply {
        /// Path to the grid .toml file
        path: PathBuf,
    },
    /// The grid files of the node ([grid] path), the active one marked.
    Grids,
    /// Print a grid file of the node (default: the active grid) with its revision.
    Show {
        /// Grid name (e.g. `grid`, `ete.toml`). Omitted → the active grid.
        name: Option<String>,
        /// Write it to this file (to edit it, then `schedule save`).
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Write a grid file of the node from a local TOML (stationd validates,
    /// writes, and applies it if it is the active grid).
    Save {
        /// Grid name (e.g. `grid`, `ete.toml`).
        name: String,
        /// The TOML to write.
        path: PathBuf,
        /// Revision the edit started from (`schedule show`); omitted = create.
        #[arg(long, conflicts_with = "force")]
        revision: Option<String>,
        /// Replace the file whatever its revision.
        #[arg(long)]
        force: bool,
    },
    /// Make a grid file the active grid: validated, applied, remembered.
    Activate {
        /// Grid name (e.g. `ete`).
        name: String,
    },
    /// Re-read the active grid file and apply it (after editing it by hand).
    Reload,
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
        /// Project this grid file of the node instead of the applied grid.
        #[arg(long, conflicts_with = "draft")]
        grid: Option<String>,
        /// Project this local TOML (nothing applied).
        #[arg(long)]
        draft: Option<PathBuf>,
    },
    /// Sizing check: does the grid have enough media? For each rule, size the
    /// pool of the playlist it references and grade it (OK / ⚠ thin / ✗
    /// insufficient). Read-only — no playout. Exits non-zero if any rule is ✗.
    Check {
        /// Only check these rule ids (repeatable). Omitted → the whole grid.
        #[arg(long = "rule")]
        rules: Vec<String>,
        /// Check this grid file of the node instead of the applied grid.
        #[arg(long, conflicts_with = "draft")]
        grid: Option<String>,
        /// Check this local TOML (nothing applied).
        #[arg(long)]
        draft: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum QueueCommand {
    /// Push a media into a queue playlist's runtime buffer (audience request /
    /// DJ injection). Refused — and exits non-zero — if the queue is at its
    /// max_len.
    Push {
        /// The queue playlist ref.
        reference: String,
        /// The media rel_path to enqueue.
        media: String,
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
    /// Make the view exactly the playlist root: sync (add / update) plus drop
    /// the playlists whose file is gone. One still referenced by a grid rule
    /// or a group is kept and reported.
    Reload,
    /// Print (or write) the TOML stationd holds for one playlist (what it
    /// applied), or with --file the playlist's file and its revision.
    Export {
        /// Playlist ref (e.g. `emission/intro`) or UUID
        reference: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// The file under the playlist root (comments included) instead of
        /// what is applied; its revision goes to stderr.
        #[arg(long)]
        file: bool,
    },
    /// Delete a playlist: its file under the playlist root and its entry in
    /// the view. Refused while a grid rule or a group references it.
    Remove {
        /// Playlist ref (e.g. `emission/intro`) or UUID
        reference: String,
        /// Confirm the deletion (the file is deleted)
        #[arg(long)]
        yes: bool,
        /// Refuse if the file changed since this revision (from `export --file`)
        #[arg(long)]
        revision: Option<String>,
    },
    /// Check a playlist TOML without applying or writing anything: every
    /// problem, tied to its field. Exit 1 if there is an error.
    Validate {
        /// Local .toml file to check
        path: PathBuf,
        /// Ref it would be saved as (resolves `./` member refs, detects cycles)
        #[arg(long = "as")]
        as_ref: Option<String>,
    },
    /// Evaluate the pool of a playlist TOML without applying it: media
    /// count, duration, a sample; per member for a group.
    Preview {
        /// Local .toml file to evaluate
        path: PathBuf,
        /// Ref it would be saved as (base of `./` member refs)
        #[arg(long = "as")]
        as_ref: Option<String>,
        /// Sample size (default 20, max 100)
        #[arg(long, default_value_t = 20)]
        sample: u32,
    },
    /// Save a playlist into stationd's playlist root: stationd validates,
    /// writes the file (atomically) and applies it. Creates it if it does not
    /// exist; replacing an existing file needs its revision (--revision, from
    /// `export --file`) or --force.
    Save {
        /// Target ref under the playlist root (e.g. `emission/intro`)
        reference: String,
        /// Local .toml file to send
        path: PathBuf,
        /// Revision the change starts from (the file's, from `export --file`)
        #[arg(long)]
        revision: Option<String>,
        /// Replace the current file whatever its revision
        #[arg(long, conflicts_with = "revision")]
        force: bool,
    },
    /// The playlists that can air a media: static ones listing it, dynamic
    /// ones whose filters keep it (groups: see their members).
    Containing {
        /// Media path, relative to the media root (e.g. `Musique/a.mp3`)
        media: String,
    },
}

fn print_tags(t: &library::MediaTags) {
    let v = |s: &str| if s.is_empty() { "-".to_string() } else { s.to_string() };
    println!("title:    {}", v(&t.title));
    println!("artist:   {}", v(&t.artist));
    println!("album:    {}", v(&t.album));
    println!("year:     {}", if t.year == 0 { "-".to_string() } else { t.year.to_string() });
    println!("genres:   {}", v(&t.genres.join(", ")));
    for src in &t.sources {
        println!("{:<9} {}", format!("{}:", src.name), v(&src.values.join(", ")));
    }
    println!("bpm:      {}", if t.bpm == 0 { "-".to_string() } else { t.bpm.to_string() });
    println!("tempo:    {}{}", v(&t.tempo), if t.tempo_manual.is_empty() { String::new() } else { format!("  (manual: {})", t.tempo_manual) });
    println!("creation: {}{}", v(&t.creation), if t.creation_manual.is_empty() { String::new() } else { format!("  (manual: {})", t.creation_manual) });
    if !t.tempo_choices.is_empty() {
        println!("tempo labels: {}", t.tempo_choices.join(", "));
    }
    println!("revision: {}", t.revision);
}

/// One line per playlist (`playlist list` / `containing`), plus who
/// references it.
fn print_summary(p: &playlist::PlaylistSummary) {
    let handle = if p.rel_path.is_empty() { "(no path)" } else { &p.rel_path };
    let state = if p.enabled { "enabled" } else { "disabled" };
    println!("{handle}  [{}]  {}  ({state})  {}", p.mode, p.name, p.id);
    let by: Vec<String> = p
        .rules
        .iter()
        .map(|r| format!("grid rule `{r}`"))
        .chain(p.groups.iter().map(|g| format!("group `{g}`")))
        .collect();
    if !by.is_empty() {
        println!("    used by: {}", by.join(", "));
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let root = match &args.command {
        Command::Station(StationCommand::Start { root, .. })
        | Command::Station(StationCommand::State { root }) => stationd_root(root.as_ref()),
        _ => stationd_root(None),
    };
    let addr = address::resolve(args.addr.as_deref(), &root)?;
    // The two commands that must work while stationd is down.
    match &args.command {
        Command::Station(StationCommand::Start { root, timeout }) => {
            return station_start(&addr, &stationd_root(root.as_ref()), *timeout).await;
        }
        Command::Station(StationCommand::State { root }) => {
            let mut bc = match BroadcastServiceClient::connect(addr.clone()).await {
                Ok(bc) => bc,
                Err(e) => return offline_state(&stationd_root(root.as_ref()), &addr, e),
            };
            let s = bc.get_state(GetStateRequest {}).await?.into_inner();
            print_broadcast_status(&s);
            return Ok(());
        }
        _ => {}
    }
    let mut client = StationClient::connect(addr.clone()).await?;

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
        Command::Playlist(cmd) => playlist_command(&addr, cmd).await?,
        Command::Schedule(ScheduleCommand::List { grid }) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched
                .list_rules(schedule::ListRulesRequest { grid: grid.unwrap_or_default(), draft_toml: String::new() })
                .await?
                .into_inner();
            if reply.rules.is_empty() {
                println!("(no rules in the view)");
            }
            for rule in reply.rules {
                println!("{rule:#?}");
            }
        }
        Command::Schedule(ScheduleCommand::Next { at }) => {
            // The scheduler lives behind its own service on the same server.
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
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
            if !reply.override_source.is_empty() {
                println!("override by:   {}", reply.override_source);
            }
            if !reply.halted_state.is_empty() {
                println!("halted:        {} (nothing airs)", reply.halted_state);
            }
        }
        Command::Schedule(ScheduleCommand::Validate { path }) => {
            let content = read_grid_toml(&path)?;
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched
                .validate_grid(ApplyGridRequest {
                    files: vec![GridFile {
                        path: path.display().to_string(),
                        toml: content,
                    }],
                })
                .await?
                .into_inner();
            if !reply.ok {
                println!("invalid: {}", path.display());
                print_grid_diagnostics(&reply.diagnostics);
                std::process::exit(1);
            }
            println!("valid:  {}", path.display());
        }
        Command::Schedule(ScheduleCommand::Apply { path }) => {
            let content = read_grid_toml(&path)?;
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched
                .apply_grid(ApplyGridRequest {
                    files: vec![GridFile {
                        path: path.display().to_string(),
                        toml: content,
                    }],
                })
                .await?
                .into_inner();
            if !reply.ok {
                println!("rejected: {} (nothing written, nothing applied)", path.display());
                print_grid_diagnostics(&reply.diagnostics);
                std::process::exit(1);
            }
            println!("applied: {}", path.display());
            if !reply.grid.is_empty() {
                println!("written: grid file {} (the active grid)", reply.grid);
            }
            println!("rules:   {}", reply.applied_rule_ids.len());
            for id in &reply.applied_rule_ids {
                println!("  - {id}");
            }
        }
        Command::Schedule(ScheduleCommand::Export { rules, out }) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
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
        Command::Schedule(ScheduleCommand::Grids) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched.list_grids(schedule::ListGridsRequest {}).await?.into_inner();
            if reply.grids.is_empty() {
                println!("(no grid file)");
            }
            for g in &reply.grids {
                let mark = if g.active { "*" } else { " " };
                let rules = match (g.revision.is_empty(), g.rules) {
                    (true, _) => "no file (the grid last applied stays)".to_string(),
                    (false, Some(n)) => format!("{n} rule(s)"),
                    (false, None) => "unreadable".to_string(),
                };
                let rev: String = g.revision.trim_start_matches("sha256:").chars().take(12).collect();
                println!("{mark} {:<24} {rules:<12} {rev}", g.name);
                if let Some(p) = &g.problem {
                    let at = if p.field_path.is_empty() { String::new() } else { format!("{}: ", p.field_path) };
                    println!("    problem: {at}{}", p.message);
                }
            }
            println!();
            println!("active: {}", reply.active);
        }
        Command::Schedule(ScheduleCommand::Show { name, file }) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let g = sched
                .get_grid(schedule::GetGridRequest { name: name.unwrap_or_default() })
                .await?
                .into_inner();
            if !g.exists {
                anyhow::bail!("grid `{}` has no file yet: create it with `schedule save {} <file>`", g.name, g.name);
            }
            match file {
                Some(path) => {
                    std::fs::write(&path, &g.toml)
                        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
                    println!("grid:     {}{}", g.name, if g.active { " (active)" } else { "" });
                    println!("written:  {}", path.display());
                    println!("revision: {}", g.revision);
                }
                None => {
                    eprintln!("# grid {}{}, revision {}", g.name, if g.active { " (active)" } else { "" }, g.revision);
                    print!("{}", g.toml);
                }
            }
            if g.differs_from_applied {
                eprintln!("note: this file is not what is applied (edited since, or invalid): `schedule reload` applies it");
            }
        }
        Command::Schedule(ScheduleCommand::Save { name, path, revision, force }) => {
            let toml = read_grid_toml(&path)?;
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let mut req = schedule::SaveGridRequest {
                name: name.clone(),
                toml,
                expected_revision: revision.unwrap_or_default(),
            };
            let mut r = sched.save_grid(req.clone()).await?.into_inner();
            if r.conflict && force && !r.revision.is_empty() {
                req.expected_revision = r.revision.clone();
                r = sched.save_grid(req).await?.into_inner();
            }
            if r.conflict {
                if r.revision.is_empty() {
                    anyhow::bail!("grid `{name}` has no file any more: save it without --revision to create it");
                }
                anyhow::bail!(
                    "grid `{name}` exists (revision {}): pass --revision <rev> (from `schedule show {name} --file`) \
                     or --force to replace it",
                    r.revision
                );
            }
            if !r.ok {
                println!("not saved:");
                print_grid_diagnostics(&r.diagnostics);
                std::process::exit(1);
            }
            println!("{} {name}", if r.created { "created:" } else { "saved:  " });
            println!("revision: {}", r.revision);
            println!("{}", if r.applied { "applied: it is the active grid" } else { "not applied: not the active grid (`schedule activate`)" });
        }
        Command::Schedule(ScheduleCommand::Activate { name }) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let r = sched.activate_grid(schedule::ActivateGridRequest { name: name.clone() }).await?.into_inner();
            if !r.ok {
                println!("refused: {name} has problems; the active grid is still {}", r.name);
                print_grid_diagnostics(&r.diagnostics);
                std::process::exit(1);
            }
            println!("active:  {}", r.name);
            println!("rules:   {}", r.rules);
        }
        Command::Schedule(ScheduleCommand::Reload) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let r = sched.reload_grid(schedule::ReloadGridRequest {}).await?.into_inner();
            if r.missing {
                println!("no file for the active grid {}: the grid last applied stays on air", r.name);
                std::process::exit(1);
            }
            if !r.ok {
                println!("refused: {} has problems; the grid last applied stays on air", r.name);
                print_grid_diagnostics(&r.diagnostics);
                std::process::exit(1);
            }
            println!("reloaded: {}", r.name);
            println!("rules:    {}", r.rules);
        }
        Command::Schedule(ScheduleCommand::Preview { at, window, grid, draft }) => {
            let draft_toml = match &draft {
                Some(p) => read_grid_toml(p)?,
                None => String::new(),
            };
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched
                .preview(PreviewRequest {
                    from: at.map(|seconds| ::prost_types::Timestamp { seconds, nanos: 0 }),
                    window: Some(::prost_types::Duration { seconds: window, nanos: 0 }),
                    grid: grid.unwrap_or_default(),
                    draft_toml,
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
                let strat = if o.strategy.is_empty() {
                    String::new()
                } else {
                    format!("  ({})", o.strategy)
                };
                let pool = fmt_pool(o.selected_count, o.total_duration.as_ref());
                println!("{utc:>11}  {}  {origin:<13}  {pl}{rule}{strat}  {pool}", o.at_local);

                // Group decomposition under the segment. A `take` member has no
                // TS (track durations are unknown); a `runtime` member of a
                // `sequence` shows its relative offset from the group start; a
                // `shuffle` shows budgets only (order is drawn at runtime).
                let n = o.members.len();
                for (i, m) in o.members.iter().enumerate() {
                    let branch = if i + 1 == n { '\u{2514}' } else { '\u{251c}' };
                    let quota = match &m.quota {
                        Some(schedule::group_member::Quota::Take(t)) => format!("take {t}"),
                        Some(schedule::group_member::Quota::TakeRandom(r)) => format!("take random {}..{}", r.min, r.max),
                        Some(schedule::group_member::Quota::Runtime(d)) => {
                            format!("runtime {}", fmt_dur(d.seconds))
                        }
                        None => String::new(),
                    };
                    let at = match &m.offset {
                        Some(d) => format!("   {}", fmt_offset(d.seconds)),
                        None => String::new(),
                    };
                    let pool = fmt_pool(m.selected_count, m.total_duration.as_ref());
                    println!("               {branch} {:<16} {quota}{at}  {pool}", m.r#ref);
                }
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
            // Live DJ connection windows: laid over the programme, listed apart.
            if !reply.live.is_empty() {
                println!();
                println!("live (DJ connection windows):");
                for w in reply.live {
                    let opens = if w.open_before {
                        format!("(open before) {}", w.opens_local)
                    } else {
                        w.opens_local
                    };
                    let closes = if w.closes_at.is_some() { w.closes_local } else { "(still open)".into() };
                    println!("  - {:<12} {opens} → {closes}  [{}]", w.dj, w.rule_id);
                }
            }
        }
        Command::Schedule(ScheduleCommand::Check { rules, grid, draft }) => {
            let draft_toml = match &draft {
                Some(p) => read_grid_toml(p)?,
                None => String::new(),
            };
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched
                .check_coverage(CheckCoverageRequest { rule_ids: rules, grid: grid.unwrap_or_default(), draft_toml })
                .await?
                .into_inner();
            if reply.entries.is_empty() {
                println!("(no rules in the grid)");
            }
            for e in &reply.entries {
                let v = schedule::Verdict::try_from(e.verdict).unwrap_or_default();
                let glyph = verdict_glyph(v);
                let kind = e.kind.as_str();
                let pl = if e.playlist_ref.is_empty() {
                    "(none)"
                } else {
                    e.playlist_ref.as_str()
                };
                let rule = if e.rule_id.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", e.rule_id)
                };
                let count = e
                    .selected_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into());
                let dur = e
                    .total_duration
                    .as_ref()
                    .map(|d| fmt_hms_secs(d.seconds))
                    .unwrap_or_else(|| "?".into());
                let detail = e.detail.as_str();
                println!("{glyph:<3} {kind:<13} {pl}{rule}  {count} média(s), {dur}  — {detail}");

                // Group members, indented — locates the weak link in a group.
                let n = e.members.len();
                for (i, m) in e.members.iter().enumerate() {
                    let branch = if i + 1 == n { '\u{2514}' } else { '\u{251c}' };
                    let mv = schedule::Verdict::try_from(m.verdict).unwrap_or_default();
                    let mg = verdict_glyph(mv);
                    let mc = m
                        .selected_count
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".into());
                    let md = m
                        .total_duration
                        .as_ref()
                        .map(|d| fmt_hms_secs(d.seconds))
                        .unwrap_or_else(|| "?".into());
                    println!(
                        "     {branch} {mg:<3} {:<16} {mc} média(s), {md}  — {}",
                        m.r#ref, m.detail
                    );
                }
            }
            let worst = schedule::Verdict::try_from(reply.worst).unwrap_or_default();
            println!();
            println!("verdict grille: {} {}", verdict_glyph(worst), worst.as_str_name());
            // Non-zero only on ✗ (INSUFFICIENT); ⚠ THIN still airs, so it does
            // not fail a CI gate on its own.
            if worst == schedule::Verdict::Insufficient {
                std::process::exit(1);
            }
        }
        Command::Queue(QueueCommand::Push { reference, media }) => {
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let reply = sched
                .enqueue(EnqueueRequest {
                    playlist_ref: reference,
                    media_path: media,
                })
                .await?
                .into_inner();
            if reply.accepted {
                println!("queued (buffer len {})", reply.len);
            } else {
                println!("refused: queue at max_len (buffer len {})", reply.len);
                std::process::exit(1);
            }
        }
        Command::Library(LibraryCommand::Reorganize { dry_run }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let report = lib.reorganize(library::ReorganizeRequest { dry_run }).await?.into_inner();
            for f in &report.files {
                println!("{}: {} -> {}{}", f.status, f.from, f.to,
                    if f.detail.is_empty() { String::new() } else { format!(" ({})", f.detail) });
            }
            println!("planned: {}, moved: {}, unchanged: {}, skipped: {}, failed: {}",
                report.planned, report.moved, report.unchanged, report.skipped, report.failed);
            anyhow::ensure!(report.failed == 0, "{} media could not be reorganized", report.failed);
        }
        Command::Library(LibraryCommand::Prune { older_than }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let r = lib
                .prune(library::PruneRequest { older_than: older_than.unwrap_or_default() })
                .await?
                .into_inner();
            println!("forgotten: {} vanished media", r.removed);
        }
        Command::Library(LibraryCommand::Tags { media }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let t = lib.get_tags(library::GetTagsRequest { rel_path: media }).await?.into_inner();
            print_tags(&t);
        }
        Command::Library(LibraryCommand::Tag {
            media, title, artist, album, year, genres, no_genre, sources, bpm, tempo, creation, revision,
        }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let revision = match revision {
                Some(r) => r,
                None => lib.get_tags(library::GetTagsRequest { rel_path: media.clone() }).await?.into_inner().revision,
            };
            let genres = (no_genre || !genres.is_empty()).then_some(library::StringList { values: genres });
            let sources = sources
                .iter()
                .map(|s| {
                    let (name, values) = s.split_once('=').ok_or_else(|| anyhow::anyhow!("--source `{s}`: expected NAME=V1,V2"))?;
                    Ok(library::TagValues {
                        name: name.trim().to_string(),
                        values: values.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect(),
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let r = lib
                .set_tags(library::SetTagsRequest {
                    rel_path: media.clone(),
                    revision,
                    title,
                    artist,
                    album,
                    year,
                    genres,
                    sources,
                    bpm,
                    tempo_manual: tempo,
                    creation_manual: creation,
                })
                .await?
                .into_inner();
            if r.conflict {
                eprintln!("conflict: the tags of {media} changed since that revision; nothing written. Now:");
                if let Some(t) = &r.tags {
                    print_tags(t);
                }
                std::process::exit(1);
            }
            println!("written: {media}");
            if let Some(t) = &r.tags {
                print_tags(t);
            }
            if let Some(m) = &r.media {
                println!("index genres: {}", if m.genres.is_empty() { "-".into() } else { m.genres.join(", ") });
            }
        }
        Command::Library(LibraryCommand::Scan { progress, reanalyze }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let watcher = if progress {
                let mut w = lib.clone();
                Some(tokio::spawn(async move {
                    let Ok(r) = w.watch_scan(library::WatchScanRequest {}).await else { return };
                    let mut st = r.into_inner();
                    while let Ok(Some(s)) = st.message().await {
                        eprint!("\r{:<60}", scan_line(&s));
                    }
                }))
            } else {
                None
            };
            let reply = lib.scan(ScanRequest { reanalyze }).await?.into_inner();
            if let Some(w) = watcher {
                w.abort();
                eprintln!();
            }
            println!("found:       {}", reply.found);
            println!("skipped:     {}", reply.skipped);
            println!("present:     {}", reply.present);
            println!("vanished:    {} (known before, not seen by this scan)", reply.vanished);
            if reply.unavailable > 0 {
                println!(
                    "unavailable: {} in all (vanished files kept in the index; `library prune` forgets them)",
                    reply.unavailable
                );
            } else {
                println!("unavailable: 0");
            }
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
        Command::Library(LibraryCommand::Search { query, genres, folder, missing, age, bpm, genre_ai, mood, sort, desc, limit, cursor, all }) => {
            use library::search_media_request::Field;
            let field = |s: &str| -> anyhow::Result<Field> {
                Ok(match s.to_ascii_lowercase().as_str() {
                    "path" => Field::Path,
                    "title" => Field::Title,
                    "artist" => Field::Artist,
                    "album" => Field::Album,
                    "year" => Field::Year,
                    "duration" => Field::Duration,
                    "bpm" => Field::Bpm,
                    "genre" => Field::Genre,
                    other => anyhow::bail!("unknown field `{other}` (path, title, artist, album, year, duration, bpm, genre)"),
                })
            };
            let sort = field(&sort)?;
            if sort == Field::Genre {
                anyhow::bail!("`genre` is not a sort key (path, title, artist, album, year, duration, bpm)");
            }
            let missing = missing.iter().map(|m| field(m).map(|f| f as i32)).collect::<anyhow::Result<Vec<_>>>()?;
            // `>120`, `<=130`, `=128` → BpmFilter { op, value }.
            let bpm = bpm
                .iter()
                .map(|s| {
                    let s = s.trim();
                    let n = s.chars().take_while(|c| matches!(c, '<' | '>' | '=' | '!')).count();
                    let (op, val) = s.split_at(n);
                    let value: f64 = val.trim().parse().map_err(|_| {
                        anyhow::anyhow!("bpm `{s}`: expected a number after the operator (e.g. >120)")
                    })?;
                    Ok(library::search_media_request::BpmFilter { op: op.to_string(), value })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let r = lib
                .search_media(library::SearchMediaRequest {
                    query,
                    genres,
                    folder: folder.unwrap_or_default(),
                    directory: None,
                    include_unavailable: all,
                    missing,
                    sort: sort as i32,
                    descending: desc,
                    limit,
                    cursor: cursor.unwrap_or_default(),
                    age: age.iter().map(String::as_str).map(age_filter).collect::<anyhow::Result<Vec<_>>>()?,
                    bpm,
                    genre_ai,
                    mood,
                })
                .await?
                .into_inner();
            for m in &r.media {
                println!("{}", fmt_media_line(m));
            }
            println!("({} shown, {} matching)", r.media.len(), r.total);
            if !r.next_cursor.is_empty() {
                println!("next page: --cursor {}", r.next_cursor);
            }
        }
        Command::Library(LibraryCommand::List { all, genres, by_genre }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let filtered = !genres.is_empty();
            let reply = lib
                .list_media(ListMediaRequest { only_available: !all, genres })
                .await?
                .into_inner();
            if reply.media.is_empty() {
                if filtered {
                    println!("(no media with these genres)");
                } else {
                    println!("(no media in the index)");
                }
            } else if by_genre {
                // Presentation-only grouping of what the server returned; the
                // case fold matches the server's (trim + Unicode lowercase).
                use std::collections::BTreeMap;
                let mut groups: BTreeMap<String, (String, Vec<&library::Media>)> = BTreeMap::new();
                let mut untagged: Vec<&library::Media> = Vec::new();
                for m in &reply.media {
                    if m.genres.is_empty() {
                        untagged.push(m);
                    }
                    for g in &m.genres {
                        groups
                            .entry(g.trim().to_lowercase())
                            .or_insert_with(|| (g.clone(), Vec::new()))
                            .1
                            .push(m);
                    }
                }
                for (label, media) in groups.values() {
                    println!("{label} ({})", media.len());
                    for m in media {
                        println!("  {}", fmt_media_line(m));
                    }
                }
                if !untagged.is_empty() {
                    println!("(no genre) ({})", untagged.len());
                    for m in &untagged {
                        println!("  {}", fmt_media_line(m));
                    }
                }
            } else {
                for m in &reply.media {
                    println!("{}", fmt_media_line(m));
                }
            }
        }
        Command::Library(LibraryCommand::ScanStatus { follow }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let mut st = lib.watch_scan(library::WatchScanRequest {}).await?.into_inner();
            while let Some(s) = st.message().await? {
                println!("{}", scan_line(&s));
                if let Some(e) = &s.last {
                    if !follow || s.phase == library::scan_status::Phase::Idle as i32 {
                        let when = epoch_utc(e.finished_at);
                        if e.ok {
                            println!(
                                "last scan:   ended {when}: found {}, skipped {}, present {}, vanished {}, unavailable {}",
                                e.found, e.skipped, e.present, e.vanished, e.unavailable
                            );
                        } else {
                            println!("last scan:   FAILED {when}: {}", e.error);
                        }
                    }
                }
                if !follow {
                    break;
                }
            }
        }
        Command::Library(LibraryCommand::Values) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let reply = lib.list_tag_values(library::ListTagValuesRequest {}).await?.into_inner();
            for o in &reply.origins {
                let name = if o.origin.is_empty() { "genre (file, TCON)".to_string() } else { format!("{} (TXXX source)", o.origin) };
                println!("{name}");
                for v in &o.values {
                    let variants = if v.spellings.len() > 1 {
                        format!("  [spellings: {}]", v.spellings.join(" | "))
                    } else {
                        String::new()
                    };
                    println!("  {:>5}  {}{variants}", v.count, v.value);
                }
                println!("  {:>5}  (none)", o.without);
            }
        }
        Command::Library(LibraryCommand::Rename { from, to, origin, dry_run }) => {
            use library::rename_tag_value_event::Event;
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let mut st = lib
                .rename_tag_value(library::RenameTagValueRequest { origin, from, to, dry_run })
                .await?
                .into_inner();
            while let Some(ev) = st.message().await? {
                match ev.event {
                    Some(Event::Preview(p)) => {
                        println!("files:     {}", p.files.len());
                        for f in &p.files {
                            println!("  {f}");
                        }
                        if p.merges {
                            println!("merge:     the new value already exists: merged into it");
                        }
                        if p.playlists.is_empty() {
                            println!("playlists: none filters on it");
                        } else {
                            println!("playlists whose genre filter names it (they will select differently):");
                            for n in &p.playlists {
                                println!("  {n}");
                            }
                        }
                    }
                    Some(Event::Started(n)) => println!("{n} file(s) to change"),
                    Some(Event::File(f)) if f.error.is_empty() => println!("  ok      {}", f.rel_path),
                    Some(Event::File(f)) => println!("  FAILED  {}: {}", f.rel_path, f.error),
                    Some(Event::Done(d)) => {
                        println!("changed: {}, unchanged: {}, failed: {}", d.changed, d.unchanged, d.failed);
                        anyhow::ensure!(d.failed == 0, "{} file(s) not renamed", d.failed);
                    }
                    None => {}
                }
            }
        }
        Command::Library(LibraryCommand::Genres { all }) => {
            let mut lib = LibraryServiceClient::connect(addr.clone()).await?;
            let reply = lib
                .list_genres(ListGenresRequest { only_available: !all })
                .await?
                .into_inner();
            if reply.genres.is_empty() && reply.untagged == 0 {
                println!("(no media in the index)");
            }
            for g in &reply.genres {
                let variants = if g.spellings.len() > 1 {
                    format!("  [spellings: {}]", g.spellings.join(" | "))
                } else {
                    String::new()
                };
                println!("{:>5}  {}{variants}", g.count, g.genre);
            }
            if reply.untagged > 0 {
                println!("{:>5}  (no genre)", reply.untagged);
            }
            if !reply.genre_ai.is_empty() {
                println!("\nGenre IA (racine):");
                for b in &reply.genre_ai {
                    println!("{:>5}  {}", b.count, b.label);
                }
            }
            if !reply.mood.is_empty() {
                println!("\nMood:");
                for b in &reply.mood {
                    println!("{:>5}  {}", b.count, b.label);
                }
            }
        }
        Command::Listeners(command) => listeners::run(&addr, command).await?,
        Command::Plugin(PluginCommand::List) => {
            let mut cli = PluginServiceClient::connect(addr.clone()).await?;
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
                let caps = if p.capabilities.is_empty() {
                    String::new()
                } else {
                    format!("  caps=[{}]", p.capabilities.join(","))
                };
                println!(
                    "{:<16} {:<12} {:<9} order={:<3} failures={}{caps}{reason}",
                    p.name, p.state, en, p.order, p.failures
                );
                if let Some(notice) = &p.operator_notice {
                    if notice.code == plugin::operator_notice::Code::AutoSleep as i32 {
                        match notice.max_connection_age {
                            Some(age) => println!("  automatic sleep active: all connections >= {age}s"),
                            None => println!("  automatic sleep active: zero listeners"),
                        }
                    }
                }
            }
        }
        Command::Plugin(PluginCommand::Db { name, cmd }) => {
            let mut cli = PluginServiceClient::connect(addr.clone()).await?;
            match cmd {
                PluginDbCommand::Info => {
                    let i = cli
                        .db_info(PluginDbInfoRequest { name: name.clone() })
                        .await?
                        .into_inner();
                    println!("path     {}", i.path);
                    if !i.exists {
                        println!("         (no file yet: plugin never started, or reset)");
                    } else {
                        println!("size     {} bytes", i.size_bytes);
                        println!("schema   version {}", i.schema_version);
                        if i.tables.is_empty() {
                            println!("tables   (none)");
                        }
                        for (n, t) in i.tables.iter().enumerate() {
                            let head = if n == 0 { "tables  " } else { "        " };
                            println!("{head} {:<24} {} row(s)", t.name, t.rows);
                        }
                    }
                    println!(
                        "limits   max_size_mb={} query_timeout_ms={} max_rows={}",
                        i.max_size_mb, i.query_timeout_ms, i.max_rows
                    );
                }
                PluginDbCommand::Query { sql } => {
                    let r = cli
                        .db_query(PluginDbQueryRequest { name: name.clone(), sql: sql.clone() })
                        .await?
                        .into_inner();
                    let cells: Vec<Vec<String>> = r
                        .rows
                        .iter()
                        .map(|row| {
                            row.values
                                .iter()
                                .map(|v| match &v.kind {
                                    None | Some(DbKind::Null(_)) => "NULL".to_string(),
                                    Some(DbKind::Integer(i)) => i.to_string(),
                                    Some(DbKind::Real(f)) => f.to_string(),
                                    Some(DbKind::Text(t)) => t.clone(),
                                    Some(DbKind::Blob(b)) => format!("<blob {} bytes>", b.len()),
                                })
                                .collect()
                        })
                        .collect();
                    let mut widths: Vec<usize> = r.columns.iter().map(|c| c.chars().count()).collect();
                    for row in &cells {
                        for (w, c) in widths.iter_mut().zip(row) {
                            *w = (*w).max(c.chars().count());
                        }
                    }
                    let line = |row: &[String]| {
                        row.iter()
                            .zip(&widths)
                            .map(|(c, w)| format!("{c:<w$}"))
                            .collect::<Vec<_>>()
                            .join("  ")
                            .trim_end()
                            .to_string()
                    };
                    println!("{}", line(&r.columns));
                    for row in &cells {
                        println!("{}", line(row));
                    }
                    println!("({} row(s))", cells.len());
                }
                PluginDbCommand::Reset { yes } => {
                    if !yes {
                        anyhow::bail!(
                            "this deletes the database of plugin `{name}` for good: re-run with --yes"
                        );
                    }
                    let r = cli
                        .db_reset(PluginDbResetRequest { name: name.clone() })
                        .await?
                        .into_inner();
                    if r.removed {
                        println!("{name}: database deleted");
                    } else {
                        println!("{name}: no database to delete");
                    }
                }
            }
        }
        Command::Plugin(cmd) => {
            let (name, action) = match &cmd {
                PluginCommand::Start { name } => (name, PluginAction::Start),
                PluginCommand::Stop { name } => (name, PluginAction::Stop),
                PluginCommand::Restart { name } => (name, PluginAction::Restart),
                PluginCommand::Reload { name } => (name, PluginAction::Reload),
                PluginCommand::List | PluginCommand::Db { .. } => unreachable!("handled above"),
            };
            let mut cli = PluginServiceClient::connect(addr.clone()).await?;
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
            let mut sched = ScheduleServiceClient::connect(addr.clone()).await?;
            let status = sched.set_clock(req).await?.into_inner();
            let state = if status.frozen { "FROZEN" } else { "real time" };
            println!("clock: {state} \u{2014} {}", status.effective_local);
        }
        Command::Station(StationCommand::State { .. } | StationCommand::Start { .. }) => {
            unreachable!("handled before connecting")
        }
        Command::Station(StationCommand::Stop { force }) => {
            // A DJ on air without --force, an unwritable marker: refused
            // (non-zero exit), stationd keeps running.
            let r = client.shutdown(ShutdownRequest { force }).await?.into_inner();
            if r.kicked {
                println!("live DJ disconnected");
            }
            if r.parked {
                println!("air: the current track plays to its end, then the background noise");
            } else {
                println!("air: Liquidsoap NOT parked — the safety fallback will air");
            }
            println!("stationd stopped (marker {}): not restarted until `stationctl station start`", r.marker);
        }
        Command::Station(StationCommand::Next) => {
            let mut bc = BroadcastServiceClient::connect(addr.clone()).await?;
            bc.skip(SkipRequest {}).await?;
            println!("skipped: the next track is starting");
        }
        Command::Station(cmd) => {
            let action = match cmd {
                StationCommand::Pause => BroadcastAction::Pause,
                StationCommand::Resume => BroadcastAction::Resume,
                StationCommand::StopWhenIdle => BroadcastAction::StopWhenIdle,
                StationCommand::Wake => BroadcastAction::Wake,
                StationCommand::State { .. }
                | StationCommand::Start { .. }
                | StationCommand::Stop { .. }
                | StationCommand::Next => unreachable!("handled above"),
            };
            let mut bc = BroadcastServiceClient::connect(addr.clone()).await?;
            // A meaningless transition comes back as failed_precondition → `?`
            // exits non-zero with the reason.
            let r = bc
                .control(ControlRequest { action: action as i32 })
                .await?
                .into_inner();
            if r.changed {
                println!("{} \u{2192} {}", state_name(r.from), state_name(r.to));
            } else {
                println!("already {}", state_name(r.to));
            }
        }
        Command::Override(OverrideCommand::Push { media, playlist, hard, expiry, tracks }) => {
            use push_override_request::{Content, Mode};
            let content = match (media, playlist) {
                (Some(m), _) => Content::MediaPath(m),
                (None, Some(p)) => Content::PlaylistRef(p),
                (None, None) => unreachable!("clap requires --media or --playlist"),
            };
            let mut bc = BroadcastServiceClient::connect(addr.clone()).await?;
            let r = bc
                .push_override(PushOverrideRequest {
                    content: Some(content),
                    mode: (if hard { Mode::Hard } else { Mode::Soft }) as i32,
                    expiry: expiry.unwrap_or_default(),
                    tracks: tracks.unwrap_or(0),
                })
                .await?
                .into_inner();
            println!("queued: override #{} ({} pending)", r.id, r.pending);
            if r.degraded {
                println!("note:   hard degraded to soft (no Liquidsoap, or station paused/stopped): airs at the next track boundary");
            }
        }
        Command::Override(OverrideCommand::List) => {
            let mut bc = BroadcastServiceClient::connect(addr.clone()).await?;
            let r = bc.list_overrides(ListOverridesRequest {}).await?.into_inner();
            if r.overrides.is_empty() {
                println!("(no pending override)");
            }
            for o in &r.overrides {
                let what = if o.media_path.is_empty() {
                    format!("playlist {}", o.playlist_ref)
                } else {
                    format!("media {}", o.media_path)
                };
                let exp = if o.expires_at == 0 {
                    "never stale".to_string()
                } else {
                    format!("expires at {}", o.expires_at)
                };
                // 0 = auto: to the end of a group's cycle, else 1 track.
                let left = if o.remaining == 0 { "auto".to_string() } else { o.remaining.to_string() };
                println!(
                    "#{:<4} {what}  mode={}  left={left}  by={}  ({exp})",
                    o.id, o.mode, o.source
                );
            }
        }
        Command::Override(OverrideCommand::Clear { id }) => {
            let mut bc = BroadcastServiceClient::connect(addr.clone()).await?;
            let r = bc
                .clear_overrides(ClearOverridesRequest { id: id.unwrap_or(0) })
                .await?
                .into_inner();
            println!("removed: {}", r.removed);
        }
        Command::Ls(LsCommand::Render) => {
            let mut ls = LiquidsoapServiceClient::connect(addr.clone()).await?;
            let r = ls.render_script(RenderScriptRequest {}).await?.into_inner();
            eprintln!("# written to {}", r.path);
            print!("{}", r.script);
        }
        Command::Icecast(IcecastCommand::Render) => {
            let mut ic = IcecastServiceClient::connect(addr.clone()).await?;
            let r = ic.render_config(IcecastRenderRequest {}).await?.into_inner();
            eprintln!("# written to {} ({}) — Icecast's user must be in this group", r.path, r.access);
            print!("{}", r.xml);
        }
        Command::Live(LiveCommand::Status) => {
            let mut lv = LiveServiceClient::connect(addr.clone()).await?;
            let s = lv.get_status(LiveStatusRequest {}).await?.into_inner();
            print_live_status(&s);
        }
        Command::Live(LiveCommand::Kick) => {
            let mut lv = LiveServiceClient::connect(addr.clone()).await?;
            let r = lv.kick(KickRequest {}).await?.into_inner();
            println!("live ended: {} disconnected, its way in closed (see `live status`)", r.dj);
        }
        Command::Live(LiveCommand::Open { dj, duration }) => {
            let mut lv = LiveServiceClient::connect(addr.clone()).await?;
            let r = lv.open(LiveOpenRequest { dj, duration }).await?.into_inner();
            if let Some(o) = r.opening {
                println!("opening: {} may connect now; it ends {}", o.dj, left(o.until));
            }
        }
        Command::Live(LiveCommand::Close { dj }) => {
            let mut lv = LiveServiceClient::connect(addr.clone()).await?;
            let r = lv.close(LiveCloseRequest { dj }).await?.into_inner();
            if let Some(o) = r.opening {
                println!("opening of {} closed (opened {})", o.dj, ago(o.opened_at));
            }
        }
        Command::Onair { what: Some(OnairCommand::History { before, limit }), .. } => {
            let mut cli = OnAirServiceClient::connect(addr.clone()).await?;
            let r = cli
                .history(onair::HistoryRequest { before, limit })
                .await?
                .into_inner();
            if r.tracks.is_empty() {
                println!("(nothing aired)");
            }
            let tz = jiff::tz::TimeZone::system();
            for t in &r.tracks {
                println!("{}", onair_history_line(t, &tz, true));
            }
        }
        Command::Onair { what: None, upcoming, history, playlists, follow } => {
            let mut cli = OnAirServiceClient::connect(addr.clone()).await?;
            let mut stream = cli
                .watch(onair::WatchRequest {
                    upcoming,
                    history,
                    playlists_ahead: playlists,
                })
                .await?
                .into_inner();
            let mut first = true;
            while let Some(snap) = stream.message().await? {
                if !first {
                    println!("\n{}", "─".repeat(72));
                }
                first = false;
                print_onair(&snap);
                if !follow {
                    break;
                }
            }
        }
        Command::Events { last, follow, level } => {
            let tz = match StationClient::connect(addr.clone()).await {
                Ok(mut c) => c.status(StatusRequest {}).await.map(|r| onair_tz(&r.into_inner().timezone)).ok(),
                Err(_) => None,
            }
            .unwrap_or_else(jiff::tz::TimeZone::system);
            let min = match level {
                None | Some(EventLevel::Info) => 1,
                Some(EventLevel::Warn) => 2,
                Some(EventLevel::Error) => 3,
            };
            let mut cli = EventServiceClient::connect(addr.clone()).await?;
            let mut st = cli.watch(events::WatchEventsRequest { backlog: last.max(1), follow }).await?.into_inner();
            while let Some(e) = st.message().await? {
                if e.level >= min {
                    println!("{}", event_line(&e, &tz));
                }
            }
        }
        Command::Stats { since, by, limit, key } => {
            let by = match by {
                StatsBy::Playlist => PlaysBy::Playlist,
                StatsBy::Leaf => PlaysBy::Leaf,
                StatsBy::Rule => PlaysBy::Rule,
                StatsBy::Origin => PlaysBy::Origin,
                StatsBy::Media => PlaysBy::Media,
                StatsBy::Artist => PlaysBy::Artist,
            };
            let mut cli = StatsServiceClient::connect(addr.clone()).await?;
            let r = cli
                .plays(PlaysRequest { since: since.clone(), by: by as i32, limit, key })
                .await?
                .into_inner();
            let local = |t: i64| {
                jiff::Timestamp::from_second(t)
                    .map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%d/%m %H:%M").to_string())
                    .unwrap_or_else(|_| t.to_string())
            };
            println!(
                "{} → {}  ({} aired / {} picked)",
                local(r.from),
                local(r.to),
                r.total_aired,
                r.total_picked
            );
            if r.rows.is_empty() {
                println!("(nothing played in this window)");
            }
            let width = r.rows.iter().map(|x| x.key.chars().count().max(9)).max().unwrap_or(9);
            println!("{:>6} {:>7}  {:<width$}  last", "aired", "picked", "");
            for x in &r.rows {
                let key = if x.key.is_empty() { "(unknown)" } else { x.key.as_str() };
                println!("{:>6} {:>7}  {:<width$}  {}", x.aired, x.picked, key, local(x.last_at));
            }
        }
        Command::Dj(DjCommand::Hash) => {
            let password = read_password("DJ password: ")?;
            let mut lv = LiveServiceClient::connect(addr.clone()).await?;
            let r = lv.hash_password(HashPasswordRequest { password }).await?.into_inner();
            println!("password_hash = \"{}\"", r.hash);
        }
        Command::Icecast(IcecastCommand::Status) => {
            let mut ic = IcecastServiceClient::connect(addr.clone()).await?;
            let s = ic.get_status(IcecastStatusRequest {}).await?.into_inner();
            print_icecast_status(&s);
        }
        Command::Ls(LsCommand::Status) => {
            let mut ls = LiquidsoapServiceClient::connect(addr.clone()).await?;
            let s = ls.get_status(LsStatusRequest {}).await?.into_inner();
            print_ls_status(&s);
        }
        Command::Debug(DebugCommand::Listeners { count }) => {
            let mut bc = BroadcastServiceClient::connect(addr.clone()).await?;
            let s = bc
                .sample_listeners(SampleListenersRequest { count })
                .await?
                .into_inner();
            print_broadcast_status(&s);
        }
    }

    Ok(())
}

fn print_ls_status(s: &liquidsoap::LiquidsoapStatus) {
    if !s.enabled {
        println!("liquidsoap: not configured (no [liquidsoap] section) — nothing airs");
        return;
    }
    let when = |t: i64| {
        if t == 0 {
            "never".to_string()
        } else {
            let ago = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64 - t)
                .unwrap_or(0);
            // Past a minute, hh:mm:ss reads better than a raw second count.
            let ago = if ago < 60 {
                format!("{ago}s")
            } else {
                format!("{:02}:{:02}:{:02}", ago / 3600, ago % 3600 / 60, ago % 60)
            };
            format!("{ago} ago (epoch {t})")
        }
    };
    let with_playlist = |media: &str, playlist: &str| {
        if playlist.is_empty() {
            media.to_string()
        } else {
            format!("{media}  [{playlist}]")
        }
    };
    println!("bridge:     {}", s.http_bind);
    println!("script:     {}", s.script_path);
    println!("pulls:      {} (last {})", s.pulls, when(s.last_pull_at));
    // A `file` reply is the prefetched next track, shown as `next:` below;
    // only a halted / none reply is worth showing as such.
    if !s.last_reply.is_empty() && s.last_reply != "file" {
        println!("last reply: {} ({})", s.last_reply, s.last_detail);
    }
    if s.on_air_kind.is_empty() {
        println!("on air:     (nothing reported by Liquidsoap yet)");
    } else {
        let what = match s.on_air_kind.as_str() {
            "track" => with_playlist(&s.on_air_media, &s.on_air_playlist),
            "live" => format!("live DJ {}", if s.on_air_media.is_empty() { "?" } else { &s.on_air_media }),
            other => other.to_string(),
        };
        println!("on air:     {what} — since {}", when(s.on_air_since));
    }
    if s.next_media.is_empty() {
        println!("next:       (nothing queued in Liquidsoap)");
    } else {
        println!("next:       {}", with_playlist(&s.next_media, &s.next_playlist));
    }
    match s.air_state.as_str() {
        "playing" | "" => println!("tracks:     {} started", s.tracks_started),
        "stopping" => println!(
            "tracks:     stopping — at the end of the current track ({} started)",
            s.tracks_started
        ),
        other => println!("tracks:     {other} ({} started)", s.tracks_started),
    }
    let control = if !s.control_error.is_empty() {
        format!("ERROR {} ({})", s.control_error, when(s.control_error_at))
    } else if !s.control_last_ok.is_empty() {
        format!("ok — last `{}` {}", s.control_last_ok, when(s.control_last_ok_at))
    } else {
        "not contacted yet".to_string()
    };
    println!("control:    {}  [{}]", control, s.control_socket);
}

/// `12s ago (epoch …)` / `hh:mm:ss ago (epoch …)`; 0 = never.
fn ago(t: i64) -> String {
    if t == 0 {
        return "never".to_string();
    }
    let ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 - t)
        .unwrap_or(0);
    let ago = if ago < 60 {
        format!("{ago}s")
    } else {
        format!("{:02}:{:02}:{:02}", ago / 3600, ago % 3600 / 60, ago % 60)
    };
    format!("{ago} ago (epoch {t})")
}

/// `in hh:mm:ss (epoch …)`: time left until `t`.
fn left(t: i64) -> String {
    let left = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| t - d.as_secs() as i64)
        .unwrap_or(0)
        .max(0);
    format!("in {:02}:{:02}:{:02} (epoch {t})", left / 3600, left % 3600 / 60, left % 60)
}

/// Read one line from standard input, without echo on a terminal.
fn read_password(prompt: &str) -> anyhow::Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    let tty = stdin.is_terminal();
    let mut saved: Option<libc::termios> = None;
    if tty {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        // SAFETY: plain termios calls on fd 0, restored below.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) == 0 {
                saved = Some(t);
                t.c_lflag &= !libc::ECHO;
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
        }
    }
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if let Some(t) = saved {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &t);
        }
        eprintln!();
    }
    read?;
    let pw = line.trim_end_matches(['\n', '\r']).to_string();
    anyhow::ensure!(!pw.is_empty(), "empty password");
    Ok(pw)
}

fn print_live_status(s: &live::LiveStatus) {
    if !s.enabled {
        println!("live:      not configured (no [live] section) — no harbor");
        return;
    }
    println!("harbor:    port {} mount {} — silence cut after {} s", s.harbor_port, s.mount, s.silence_timeout_s);
    if s.djs_error.is_empty() {
        println!("DJ file:   {} ({} DJ)", s.djs_path, s.djs_count);
    } else {
        println!("DJ file:   UNUSABLE — every login refused: {}", s.djs_error);
    }
    if s.urgent_djs.is_empty() {
        println!("urgent:    nobody holds the permanent right");
    } else {
        println!(
            "urgent:    {} (permanent right; closed {} s after a cut)",
            s.urgent_djs.join(", "),
            s.urgent_cooldown_s
        );
    }
    match &s.on_air {
        Some(a) => {
            let way = match a.access.as_str() {
                "slot" => format!("slot {}", if a.rule_id.is_empty() { "?" } else { &a.rule_id }),
                "open" => "ad-hoc opening".to_string(),
                "urgent" => "urgent right".to_string(),
                _ => "?".to_string(),
            };
            println!(
                "on air:    {} — since {} — {} — from {}",
                a.dj,
                ago(a.since),
                way,
                if a.address.is_empty() { "?" } else { &a.address }
            )
        }
        None => println!("on air:    nobody (the programme airs)"),
    }
    for o in &s.openings {
        println!(
            "opening:   {} — ends {}{}",
            o.dj,
            left(o.until),
            if o.cut { " — cut: refused until its end" } else { "" }
        );
    }
    for c in &s.cooldowns {
        println!("urgent closed: {} (cut) — reopens {}", c.dj, left(c.until));
    }
    if let Some(l) = &s.last {
        println!("last live: {} — ended {} ({})", l.dj, ago(s.last_ended_at), s.last_reason);
    }
    for r in &s.refused {
        println!("refused:   {} until the end of {}", r.dj, r.occurrence);
    }
    if !s.last_refusal_dj.is_empty() {
        println!(
            "last refusal: {} — {} ({})",
            s.last_refusal_dj,
            s.last_refusal_reason,
            ago(s.last_refusal_at)
        );
    }
}

fn print_icecast_status(s: &icecast::IcecastStatus) {
    if !s.enabled {
        println!("icecast:   not configured (no [icecast] section) — audience never sampled");
        return;
    }
    let server = if s.server_id.is_empty() {
        s.server.clone()
    } else {
        format!("{} ({})", s.server, s.server_id)
    };
    println!("icecast:   {server} — read every {} s", s.poll_interval_s);
    println!("last read: {}", ago(s.last_ok_at));
    if !s.problem.is_empty() {
        println!("problem:   {} ({})", s.problem, ago(s.problem_at));
    }
    match s.audience {
        Some(n) => println!("audience:  {n}"),
        None => println!("audience:  unknown — a draining station keeps playing"),
    }
    if s.last_ok_at == 0 {
        return;
    }
    // The last read failed: what follows is the last successful one.
    if s.problem_at > s.last_ok_at {
        println!("(mounts below as of the last successful read, {})", ago(s.last_ok_at));
    }
    for m in &s.mounts {
        println!("{}", m.mount);
        if !m.present {
            println!("  source:    absent from Icecast");
            continue;
        }
        if m.connected {
            let mut who = vec![m.source_ip.as_str(), m.user_agent.as_str()];
            who.retain(|w| !w.is_empty());
            let who = if who.is_empty() { String::new() } else { format!(" — {}", who.join(", ")) };
            println!("  source:    connected since {}{who}", m.stream_start);
        } else {
            println!("  source:    NOT connected (mount known to Icecast, nobody feeding it)");
        }
        let nominal = if m.bitrate.is_empty() { "?".to_string() } else { format!("{} kbit/s", m.bitrate) };
        let read = match m.read_kbps {
            Some(r) => format!("~{r:.0} kbit/s received (last {} s)", m.read_window_s),
            None => "received: measured after 30 s".to_string(),
        };
        let ctype = if m.content_type.is_empty() { String::new() } else { format!("  [{}]", m.content_type) };
        println!("  bitrate:   {nominal} announced, {read}{ctype}");
        let listeners = m.listeners.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let peak = m.listener_peak.map(|n| format!(" (peak {n})")).unwrap_or_default();
        println!("  listeners: {listeners}{peak}");
        if !m.title.is_empty() {
            println!("  title:     {}", m.title);
        }
    }
}

/// The s6 service directory of stationd in the container (s6-overlay v3).
const S6_SERVICE: &str = "/run/service/stationd";

/// Exit code of `station state` when stationd is stopped by the operator.
const EXIT_OPERATOR_STOPPED: i32 = 3;

/// stationd's working directory: `--root`, else `$STATIOND_ROOT`, else `.`.
fn stationd_root(root: Option<&PathBuf>) -> PathBuf {
    root.cloned()
        .or_else(|| std::env::var_os("STATIOND_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

// ----- journal / scan --------------------------------------------------------

/// One journal event, in English (D12: stationctl renders the codes itself).
fn event_line(e: &events::Event, tz: &jiff::tz::TimeZone) -> String {
    use events::event::{Code, Component, Level};
    let when = jiff::Timestamp::from_millisecond(e.at_ms)
        .map(|t| t.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let level = Level::try_from(e.level).map(|l| l.as_str_name()).unwrap_or("?");
    let component = Component::try_from(e.component).map(|c| c.as_str_name().to_lowercase()).unwrap_or_default();
    let param = |n: &str| e.params.iter().find(|p| p.name == n).map(|p| p.value.as_str()).unwrap_or("");
    let rest = |skip: &[&str]| {
        e.params
            .iter()
            .filter(|p| !skip.contains(&p.name.as_str()))
            .map(|p| format!("{}={}", p.name, p.value))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let epoch_param = |name: &str| param(name).parse::<i64>().ok()
        .and_then(|at| jiff::Timestamp::from_second(at).ok())
        .map(|at| at.to_zoned(tz.clone()).strftime("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    let what = match Code::try_from(e.code).unwrap_or(Code::Unspecified) {
        Code::PluginModeEnabled if param("kind") == "auto_sleep" => {
            let age = param("max_connection_age");
            if age.is_empty() {
                format!("{}: automatic sleep active (zero listeners)", param("plugin"))
            } else {
                format!("{}: automatic sleep active (all connections >= {age}s)", param("plugin"))
            }
        },
        Code::ConnectionStarted => format!("connection_started mount={} id={} start≈{} age={}s",
            param("mount"), param("id"), epoch_param("started_at"), param("connected_seconds")),
        Code::ConnectionEnded => format!("connection_ended mount={} id={} start≈{} last_seen={} duration≥{}s end_observed={}",
            param("mount"), param("id"), epoch_param("started_at"), epoch_param("last_seen_at"),
            param("connected_seconds"), epoch_param("observed_at")),
        Code::Log => format!("{}  {}", param("message"), rest(&["message", "target"])),
        c => format!("{}  {}", c.as_str_name().to_lowercase(), rest(&[])),
    };
    format!("{when}  {level:<5}  {component:<10}  {}", what.trim_end())
}

fn scan_line(s: &library::ScanStatus) -> String {
    use library::scan_status::Phase;
    let phase = Phase::try_from(s.phase).map(|p| p.as_str_name().to_lowercase()).unwrap_or_default();
    match Phase::try_from(s.phase).unwrap_or(Phase::Unspecified) {
        Phase::Idle | Phase::Unspecified => "scan:        idle".to_string(),
        Phase::Reading => format!("scan:        reading {}/{}", s.done, s.total),
        Phase::Analyzing => format!("scan:        estimating BPM {}/{}", s.done, s.total),
        _ => format!("scan:        {phase} ({} files)", s.total),
    }
}

fn epoch_utc(epoch: i64) -> String {
    jiff::Timestamp::from_second(epoch).map(|t| t.strftime("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_default()
}

// ----- onair ---------------------------------------------------------------

fn onair_tz(name: &str) -> jiff::tz::TimeZone {
    jiff::tz::TimeZone::get(name).unwrap_or_else(|_| jiff::tz::TimeZone::system())
}

fn hm(epoch: i64, tz: &jiff::tz::TimeZone, secs: bool) -> String {
    let fmt = if secs { "%H:%M:%S" } else { "%H:%M" };
    jiff::Timestamp::from_second(epoch)
        .map(|t| t.to_zoned(tz.clone()).strftime(fmt).to_string())
        .unwrap_or_else(|_| epoch.to_string())
}

fn mmss(ms: Option<u64>) -> String {
    match ms {
        Some(ms) => {
            let s = ms / 1000;
            if s >= 3600 {
                format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
            } else {
                format!("{}:{:02}", s / 60, s % 60)
            }
        }
        None => "—".into(),
    }
}

/// `Artist — Title`; the file name when the tags are missing (said so).
fn onair_label(t: &onair::Track) -> String {
    let file = t.rel_path.rsplit('/').next().unwrap_or(&t.rel_path);
    match (t.artist.is_empty(), t.title.is_empty()) {
        (false, false) => format!("{} — {}", t.artist, t.title),
        (true, false) => t.title.clone(),
        _ if t.stream => format!("relay {}", t.rel_path),
        _ => format!("{file} (no title)"),
    }
}

fn onair_from(t: &onair::Track) -> String {
    let mut parts = Vec::new();
    if !t.playlist_ref.is_empty() {
        parts.push(t.playlist_ref.clone());
    }
    if !t.leaf_ref.is_empty() && t.leaf_ref != t.playlist_ref {
        parts.push(format!("› {}", t.leaf_ref));
    }
    if !t.origin.is_empty() {
        let rule = if t.rule_id.is_empty() { String::new() } else { format!(" {}", t.rule_id) };
        parts.push(format!("[{}{rule}]", t.origin));
    }
    if !t.override_source.is_empty() {
        parts.push(format!("by {}", t.override_source));
    }
    parts.join(" ")
}

fn onair_history_line(t: &onair::Track, tz: &jiff::tz::TimeZone, secs: bool) -> String {
    use onair::track::Outcome;
    let when = t.started_at.map(|e| hm(e, tz, secs)).unwrap_or_else(|| "—".into());
    let end = match Outcome::try_from(t.outcome).unwrap_or(Outcome::Unspecified) {
        Outcome::Aired => "aired",
        Outcome::Cut => "CUT",
        Outcome::Unknown => "end unknown",
        Outcome::Unspecified => "",
    };
    format!("  {when}  {:<44} {:>7}  {:<11} {}", onair_label(t), mmss(t.duration_ms), end, onair_from(t))
}

fn onair_when(at: Option<i64>, tz: &jiff::tz::TimeZone) -> String {
    at.map(|e| hm(e, tz, false)).unwrap_or_else(|| "?".into())
}

/// An on-air note (opcode + parameters), worded for the CLI.
fn onair_note(n: &onair::Note, tz: &jiff::tz::TimeZone) -> String {
    use onair::note::Code as C;
    match C::try_from(n.code) {
        Ok(C::StationPaused) => "station paused: nothing follows until it resumes".into(),
        Ok(C::StationSleeping) => "station asleep: nothing follows until it wakes".into(),
        Ok(C::SleepAtTrackEnd) => "falls asleep at the end of this track (0 listeners)".into(),
        Ok(C::SleepArmed) => "sleep armed: stops at a track boundary if the sleep conditions still hold".into(),
        Ok(C::LiveOnAir) => format!("DJ {} on air: what follows depends on the end of the live", n.dj),
        Ok(C::NoLiquidsoap) => "no [liquidsoap]: nothing airs; this is what the grid would pick".into(),
        Ok(C::Simulated) => "simulated: what the station will play if nothing changes meanwhile (an override, a live, a new grid, a rescan can change it)".into(),
        Ok(C::PoolEmpty) => "nothing left to air in the grid at that point: dead air (Liquidsoap fills it)".into(),
        Ok(C::Fallback) => "FALLBACK: no rule covers that moment".into(),
        Ok(C::StreamUnknownDuration) => format!("relay {}: unknown duration, no estimated time beyond", n.media),
        Ok(C::UnknownDuration) => format!("{}: unknown duration (not indexed), no estimated time beyond", n.media),
        Ok(C::SimulationFailed) => format!("simulation failed: {}", n.reason),
        Ok(C::PluginFilterFailed) => format!("plugin `{}` failed in the simulation: {}", n.plugin, n.reason),
        Ok(C::GridProjectionFailed) => format!("grid projection failed: {}", n.reason),
        Ok(C::HistoryUnreadable) => format!("history unreadable: {}", n.reason),
        Ok(C::RendezvousWillNotCut) => format!(
            "rendez-vous `{}` ({}) will NOT cut at {}: its playlist will produce nothing",
            n.rule, n.playlist, onair_when(n.at, tz)
        ),
        Ok(C::SourceWillBeEmpty) => format!(
            "rule `{}` ({}) will produce nothing at {}: empty pool or emptied by the constraints",
            n.rule, n.playlist, onair_when(n.at, tz)
        ),
        Ok(C::RendezvousNotCut) => format!(
            "rendez-vous `{}` ({}) did NOT cut at {}: its playlist produced nothing",
            n.rule, n.playlist, onair_when(n.at, tz)
        ),
        Ok(C::SourceWasEmpty) => format!(
            "rule `{}` ({}) produced nothing ({}×, last at {}): the grid fell through",
            n.rule, n.playlist, n.count, onair_when(n.at, tz)
        ),
        Ok(C::Unspecified) | Err(_) => format!("unknown note (code {})", n.code),
    }
}

/// `<10d` → operator `<`, duration `10d` (the duration is checked by stationd).
fn age_filter(s: &str) -> anyhow::Result<library::search_media_request::AgeFilter> {
    let s = s.trim();
    let op_len = s.bytes().take_while(|b| matches!(b, b'<' | b'>' | b'=')).count();
    let (op, value) = s.split_at(op_len);
    if !matches!(op, "<" | "<=" | ">" | ">=") || value.trim().is_empty() {
        anyhow::bail!("--age {s:?}: want an operator and a duration, e.g. <10d, >=30d");
    }
    Ok(library::search_media_request::AgeFilter { op: op.into(), value: value.trim().into() })
}

fn print_onair(s: &onair::OnAirSnapshot) {
    let tz = onair_tz(&s.timezone);
    let listeners = s.listeners.map_or("listeners —".to_string(), |n| format!("listeners {n}"));
    let mut head = vec![s.state.clone(), listeners];
    if s.pending_overrides > 0 {
        head.push(format!("{} pending override(s)", s.pending_overrides));
    }
    if !s.live_dj.is_empty() {
        head.push(format!("LIVE {}", s.live_dj));
    }
    if !s.liquidsoap {
        head.push("no [liquidsoap]: nothing airs".into());
    }
    println!("{}   (#{} at {} {})", head.join(" · "), s.revision, hm(s.observed_at, &tz, true), s.timezone);

    println!("\nON AIR");
    match (&s.on_air, s.on_air_kind.as_str()) {
        (Some(t), _) => {
            let since = t.started_at.map(|e| hm(e, &tz, true)).unwrap_or_else(|| "—".into());
            let elapsed = t.started_at.map(|e| (s.observed_at - e).max(0) as u64 * 1000);
            println!(
                "  {since}  {:<44} {} / {}  {}",
                onair_label(t),
                mmss(elapsed),
                mmss(t.duration_ms),
                onair_from(t)
            );
        }
        (None, "") => println!("  (nothing reported by Liquidsoap)"),
        (None, kind) => println!("  {kind}"),
    }
    if let Some(p) = &s.current_playlist {
        let rule = if p.rule_id.is_empty() { String::new() } else { format!(", rule {}", p.rule_id) };
        println!("  playlist: {} ({}{rule})", p.playlist_ref, p.origin);
    }

    println!("\nNEXT");
    if let Some(t) = &s.prefetched {
        println!("  prepared  {:<44} {:>7}  {}", onair_label(t), mmss(t.duration_ms), onair_from(t));
    }
    for t in &s.upcoming {
        let when = t.estimated_at.map(|e| format!("~{}", hm(e, &tz, false))).unwrap_or_else(|| "~ —".into());
        let cut = t.cut_at.map(|e| format!("  (cut at {})", hm(e, &tz, false))).unwrap_or_default();
        println!("  {when:<9} {:<44} {:>7}  {}{cut}", onair_label(t), mmss(t.duration_ms), onair_from(t));
    }
    if s.prefetched.is_none() && s.upcoming.is_empty() {
        println!("  (nothing)");
    }

    if !s.next_playlists.is_empty() || !s.indicative.is_empty() {
        println!("\nPLAYLISTS AHEAD");
        for p in &s.next_playlists {
            let when = p.from.map(|e| hm(e, &tz, false)).unwrap_or_else(|| "—".into());
            use onair::playlist_slot::Issue;
            let issue = match Issue::try_from(p.issue).unwrap_or(Issue::None) {
                Issue::None => "",
                Issue::PoolEmpty => "  !! EMPTY POOL",
                Issue::NothingPlayable => "  !! NOTHING PLAYABLE",
            };
            println!("  {when}  {:<24} [{} {}]{issue}", p.playlist_ref, p.origin, p.rule_id);
        }
        for p in &s.indicative {
            println!("  (by track count)  {:<24} [{}]", p.playlist_ref, p.rule_id);
        }
    }

    println!("\nPLAYED");
    if s.history.is_empty() {
        println!("  (nothing yet)");
    }
    for t in &s.history {
        println!("{}", onair_history_line(t, &tz, false));
    }

    if !s.notes.is_empty() {
        println!("\nNOTES");
        for n in &s.notes {
            println!("  - {}", onair_note(n, &tz));
        }
    }
}

fn local_time(epoch: i64) -> String {
    jiff::Timestamp::from_second(epoch)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%Y-%m-%d %H:%M:%S %Z").to_string())
        .unwrap_or_else(|_| format!("epoch {epoch}"))
}

/// `station state` with stationd unreachable: the marker tells a stop by the
/// operator (exit 3) from an outage (error).
fn offline_state(root: &std::path::Path, addr: &str, err: tonic::transport::Error) -> anyhow::Result<()> {
    let marker = stationd::operator_stop::marker_under(root);
    match stationd::operator_stop::read_marker(&marker) {
        Some(at) => {
            let since = at.map(|a| format!(" since {}", local_time(a))).unwrap_or_default();
            println!("state:     STOPPED by the operator{since} ({})", marker.display());
            println!("restart:   stationctl station start");
            std::process::exit(EXIT_OPERATOR_STOPPED);
        }
        None => anyhow::bail!(
            "stationd unreachable at {addr} ({err}) and no stop marker at {}: not an operator stop",
            marker.display()
        ),
    }
}

/// `station start`: remove the marker, restart the parked s6 service, wait
/// until it is up, then show the state. Local by nature — stationd is not
/// there to answer — the only command that is not a gRPC call.
async fn station_start(addr: &str, root: &std::path::Path, timeout_s: u64) -> anyhow::Result<()> {
    use std::process::Command as Proc;
    let marker = stationd::operator_stop::marker_under(root);
    if !std::path::Path::new(S6_SERVICE).is_dir() {
        anyhow::bail!(
            "no s6 service {S6_SERVICE} here: run it in the container \
             (`docker compose exec station stationctl station start`); \
             outside s6, launch stationd directly — it lifts the stop itself"
        );
    }
    let was_stopped = stationd::operator_stop::read_marker(&marker).is_some();
    stationd::operator_stop::remove_marker(&marker)
        .map_err(|e| anyhow::anyhow!("cannot remove {}: {e}", marker.display()))?;
    if !was_stopped {
        // Not stopped: a running stationd is left alone.
        if let Ok(mut bc) = BroadcastServiceClient::connect(addr.to_string()).await {
            println!("stationd is already running (no stop marker)");
            print_broadcast_status(&bc.get_state(GetStateRequest {}).await?.into_inner());
            return Ok(());
        }
    }
    // -r + -wR: restart and wait until restarted AND ready (the parked
    // service was already « up and ready »: a plain wait would return at once).
    println!("marker removed, restarting the s6 service: waiting for stationd…");
    let status = Proc::new("s6-svc")
        .args(["-T", &(timeout_s * 1000).to_string(), "-wR", "-r", S6_SERVICE])
        .status();
    if !matches!(&status, Ok(s) if s.success()) {
        // Put the stop back when the service could not even be told: the
        // state must not lie. (A timeout leaves it starting: no marker.)
        let told = std::path::Path::new(S6_SERVICE).join("supervise/control");
        let writable = {
            use std::os::unix::fs::OpenOptionsExt;
            // Non-blocking: a fifo without reader must not hang the CLI.
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&told)
                .is_ok()
        };
        if was_stopped && !writable {
            let _ = stationd::operator_stop::write_marker(
                &marker,
                stationd::resolver::Epoch(jiff::Timestamp::now().as_second()),
                "cli (start refused)",
            );
            anyhow::bail!(
                "cannot control {S6_SERVICE} ({status:?}): run as root, or check the service \
                 permissions (s6-svperms in the stationd run script) — still stopped"
            );
        }
        anyhow::bail!("stationd not ready after {timeout_s} s ({status:?}): see the container logs");
    }
    let mut bc = BroadcastServiceClient::connect(addr.to_string()).await?;
    print_broadcast_status(&bc.get_state(GetStateRequest {}).await?.into_inner());
    Ok(())
}

fn state_name(state: i32) -> &'static str {
    broadcast::State::try_from(state)
        .map(|s| s.as_str_name())
        .unwrap_or("UNKNOWN")
}

fn print_broadcast_status(s: &broadcast::BroadcastStatus) {
    println!("state:     {}", state_name(s.state));
    match s.listeners {
        Some(n) => println!("listeners: {n}"),
        None => println!("listeners: unknown (never sampled, or Icecast unreadable — see `icecast status`)"),
    }
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

/// Count and duration have independent presence (e.g. a remote member with
/// runtime). Keep milliseconds: a short sting must not be printed as zero.
/// One media line for `library list`: duration, path, "artist — title",
/// genres, availability flag.
fn fmt_media_line(m: &library::Media) -> String {
    let secs = m.duration_ms / 1000;
    let dur = format!("{}:{:02}", secs / 60, secs % 60);
    let flag = if m.available { "" } else { "  (unavailable)" };
    let who = match (m.artist.is_empty(), m.title.is_empty()) {
        (false, false) => format!("{} \u{2014} {}", m.artist, m.title),
        (true, false) => m.title.clone(),
        _ => "(no tags)".to_string(),
    };
    let genres = if m.genres.is_empty() {
        "\u{2014}".to_string()
    } else {
        m.genres.join(", ")
    };
    // Analyse offline, quand présente : BPM · genre IA · mood.
    let mut ai = Vec::new();
    if m.bpm > 0.0 {
        ai.push(format!("{} bpm", m.bpm.round() as i64));
    }
    if !m.genre_ai.is_empty() {
        ai.push(m.genre_ai.clone());
    }
    if !m.mood.is_empty() {
        ai.push(m.mood.clone());
    }
    let ai = if ai.is_empty() { String::new() } else { format!("  {{{}}}", ai.join(" · ")) };
    format!("{dur:>7}  {}  {who}  [{genres}]{ai}{flag}", m.rel_path)
}

fn fmt_pool(count: Option<u64>, duration: Option<&prost_types::Duration>) -> String {
    let count = count.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into());
    let duration = duration.map(|d| {
        let (h, m, s) = (d.seconds / 3600, (d.seconds % 3600) / 60, d.seconds % 60);
        if d.nanos == 0 {
            format!("{h:02}:{m:02}:{s:02}")
        } else {
            format!("{h:02}:{m:02}:{s:02}.{:03}", d.nanos / 1_000_000)
        }
    }).unwrap_or_else(|| "unknown".into());
    format!("pool: {count} media, duration {duration}")
}

/// Compact duration render for the preview's group members: largest unit first,
/// seconds dropped once minutes/hours are present. 1200 → "20m", 3900 → "1h05m".
fn fmt_dur(secs: i64) -> String {
    if secs <= 0 {
        return "0s".to_string();
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        // Zero-pad the minutes only when an hour precedes them ("1h05m").
        if h > 0 {
            out.push_str(&format!("{m:02}m"));
        } else {
            out.push_str(&format!("{m}m"));
        }
    }
    if s > 0 && h == 0 && m == 0 {
        out.push_str(&format!("{s}s"));
    }
    if out.is_empty() {
        out.push_str("0s");
    }
    out
}

/// A relative start offset for a sequence's runtime member: "+0", "+20m", …
fn fmt_offset(secs: i64) -> String {
    if secs == 0 {
        "+0".to_string()
    } else {
        format!("+{}", fmt_dur(secs))
    }
}

/// Seconds → "HH:MM:SS" for the coverage check's pool durations.
fn fmt_hms_secs(secs: i64) -> String {
    let s = secs.max(0);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// Verdict glyph for the coverage table: OK / ⚠ thin / ✗ insufficient.
fn verdict_glyph(v: schedule::Verdict) -> &'static str {
    match v {
        schedule::Verdict::Ok => "OK",
        schedule::Verdict::Thin => "\u{26a0}",         // ⚠
        schedule::Verdict::Insufficient => "\u{2717}", // ✗
        _ => "?",
    }
}

// ---------------------------------------------------------------------------
// playlist
// ---------------------------------------------------------------------------

fn severity_label(d: &playlist::Diagnostic) -> &'static str {
    match playlist::diagnostic::Severity::try_from(d.severity) {
        Ok(playlist::diagnostic::Severity::Warning) => "warning",
        _ => "error",
    }
}

/// `error   selection.order: `order = shuffle` is not valid… (got: shuffle; expected: fifo, lifo)`
fn print_diagnostics(ds: &[playlist::Diagnostic]) {
    for d in ds {
        let field = if d.field_path.is_empty() { "(file)" } else { d.field_path.as_str() };
        let mut extra = Vec::new();
        if !d.rejected.is_empty() {
            extra.push(format!("got: {}", d.rejected));
        }
        if !d.expected.is_empty() {
            extra.push(format!("expected: {}", d.expected));
        }
        let extra = if extra.is_empty() { String::new() } else { format!(" ({})", extra.join("; ")) };
        println!("  {:<7} {field}: {}{extra}", severity_label(d), d.message);
    }
}

fn print_grid_diagnostics(ds: &[schedule::GridDiagnostic]) {
    for d in ds {
        let field = if d.field_path.is_empty() { "(file)" } else { d.field_path.as_str() };
        let rule = if d.rule_id.is_empty() { String::new() } else { format!(" [{}]", d.rule_id) };
        let mut extra = Vec::new();
        if !d.rejected.is_empty() {
            extra.push(format!("got: {}", d.rejected));
        }
        if !d.expected.is_empty() {
            extra.push(format!("expected: {}", d.expected));
        }
        let extra = if extra.is_empty() { String::new() } else { format!(" ({})", extra.join("; ")) };
        println!("  error   {field}{rule}: {}{extra}", d.message);
    }
}

fn read_toml(path: &std::path::Path) -> anyhow::Result<String> {
    std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))
}

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}h{:02}m{:02}s", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

async fn playlist_command(addr: &str, cmd: PlaylistCommand) -> anyhow::Result<()> {
    use playlist::*;
    let mut pl = PlaylistServiceClient::connect(addr.to_string()).await?;
    match cmd {
        PlaylistCommand::Add { path } => {
            // Syntactic pre-check only: is this readable, well-formed TOML?
            // The business validation happens in stationd. Failing fast here
            // avoids a pointless round-trip on an obviously broken file.
            let content = read_toml(&path)?;
            content
                .parse::<toml::Table>()
                .map_err(|e| anyhow::anyhow!("{} is not well-formed TOML: {e}", path.display()))?;
            let reply = pl.add(AddRequest { toml: content }).await?.into_inner();
            // stationd is authoritative on the content; write back what it
            // returned (the id-injected, losslessly-rewritten TOML) into the
            // file the user named (not stationd's playlist root: use `save`).
            std::fs::write(&path, &reply.toml).map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
            println!("added:  {}", path.display());
            println!("id:     {}", reply.id);
        }
        PlaylistCommand::Sync => {
            // No path argument: stationd scans its own configured playlist
            // root. The report is best-effort — successes counted, failures
            // listed loudly (no-silent-failure).
            let reply = pl.sync(SyncRequest {}).await?.into_inner();
            println!("synced: {} playlist(s)", reply.added);
            if reply.errors.is_empty() {
                println!("errors: none");
            } else {
                println!("errors: {}", reply.errors.len());
                print_file_errors(&reply.errors);
                // Non-zero exit so scripts / CI notice something was rejected.
                std::process::exit(1);
            }
        }
        PlaylistCommand::List => {
            let reply = pl.list(ListRequest {}).await?.into_inner();
            if reply.playlists.is_empty() {
                println!("(no playlists in the view)");
            }
            for p in &reply.playlists {
                print_summary(p);
            }
        }
        PlaylistCommand::Containing { media } => {
            let reply = pl.containing(ContainingRequest { media_path: media.clone() }).await?.into_inner();
            if reply.playlists.is_empty() {
                println!("(no playlist can air {media})");
            }
            for p in &reply.playlists {
                print_summary(p);
            }
        }
        PlaylistCommand::Reload => {
            let reply = pl.reload(ReloadRequest {}).await?.into_inner();
            println!("synced:  {} playlist(s)", reply.added);
            if reply.removed.is_empty() {
                println!("removed: none");
            } else {
                println!("removed: {} (file gone)", reply.removed.len());
                for r in &reply.removed {
                    println!("  - {r}");
                }
            }
            if reply.errors.is_empty() {
                println!("errors:  none");
            } else {
                println!("errors:  {}", reply.errors.len());
                print_file_errors(&reply.errors);
                std::process::exit(1);
            }
        }
        PlaylistCommand::Export { reference, out, file } => {
            let reply = pl.export(ExportRequest { reference }).await?.into_inner();
            let handle = if reply.rel_path.is_empty() { reply.id.clone() } else { reply.rel_path.clone() };
            let content = if file {
                if reply.file.is_empty() {
                    anyhow::bail!("playlist `{handle}` has no file under the playlist root (added with `playlist add`)");
                }
                eprintln!("file:     {}", reply.file);
                eprintln!("revision: {}", reply.revision);
                &reply.file_toml
            } else {
                if reply.file_differs {
                    eprintln!(
                        "note: {} differs from what is applied (edited since, or invalid): `export --file` shows it",
                        reply.file
                    );
                }
                &reply.applied_toml
            };
            match out {
                Some(path) => {
                    std::fs::write(&path, content).map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
                    println!("exported {handle} → {}", path.display());
                }
                None => print!("{content}"),
            }
        }
        PlaylistCommand::Remove { reference, yes, revision } => {
            if !yes {
                anyhow::bail!(
                    "this deletes playlist `{reference}` (its file and its entry): re-run with --yes \
                     (`playlist export {reference} --out <file>` keeps a copy)"
                );
            }
            let reply = pl
                .remove(RemoveRequest { reference, expected_revision: revision.unwrap_or_default() })
                .await?
                .into_inner();
            let handle = if reply.rel_path.is_empty() { "(no path)" } else { &reply.rel_path };
            println!("removed: {handle}  {}", reply.id);
            if reply.file.is_empty() {
                println!("file:    none on disk");
            } else {
                println!("file:    {} (deleted)", reply.file);
            }
        }
        PlaylistCommand::Validate { path, as_ref } => {
            let toml = read_toml(&path)?;
            let r = pl.validate(ValidateRequest { toml, reference: as_ref.unwrap_or_default() }).await?.into_inner();
            if r.diagnostics.is_empty() {
                println!("valid");
            } else {
                println!("{}", if r.ok { "valid, with warnings:" } else { "invalid:" });
                print_diagnostics(&r.diagnostics);
            }
            if !r.ok {
                std::process::exit(1);
            }
        }
        PlaylistCommand::Preview { path, as_ref, sample } => {
            let toml = read_toml(&path)?;
            let r = pl
                .preview_pool(PreviewPoolRequest { toml, reference: as_ref.unwrap_or_default(), sample })
                .await?
                .into_inner();
            if !r.ok {
                println!("invalid:");
                print_diagnostics(&r.diagnostics);
                std::process::exit(1);
            }
            match (r.count, r.duration_ms) {
                (Some(c), Some(d)) => println!("pool:     {c} media, {}", fmt_ms(d)),
                (Some(c), None) => println!("pool:     {c} media, duration unknown"),
                _ => println!("pool:     not measurable (remote / queue: no indexed media)"),
            }
            if let Some(a) = r.artists {
                println!("artists:  {a}");
            }
            for m in &r.members {
                let what = match (&m.error, m.count, m.duration_ms) {
                    (e, _, _) if !e.is_empty() => format!("!! {e}"),
                    (_, Some(c), Some(d)) => format!("{c} media, {}", fmt_ms(d)),
                    (_, Some(c), None) => format!("{c} media"),
                    _ => "not measurable".into(),
                };
                let resolved = if m.resolved.is_empty() || m.resolved == m.r#ref { String::new() } else { format!(" → {}", m.resolved) };
                println!("  member {}{resolved}: {what}", m.r#ref);
            }
            for m in &r.sample {
                let label = match (m.artist.is_empty(), m.title.is_empty()) {
                    (false, false) => format!("{} — {}", m.artist, m.title),
                    (true, false) => m.title.clone(),
                    _ => m.rel_path.clone(),
                };
                println!("  {:>7}  {label}  ({})", fmt_ms(m.duration_ms), m.rel_path);
            }
            if !r.diagnostics.is_empty() {
                print_diagnostics(&r.diagnostics);
            }
        }
        PlaylistCommand::Save { reference, path, revision, force } => {
            let toml = read_toml(&path)?;
            let mut req = SaveRequest { reference: reference.clone(), toml, expected_revision: revision.unwrap_or_default() };
            let mut r = pl.save(req.clone()).await?.into_inner();
            if r.conflict && force && !r.revision.is_empty() {
                // --force: take the current revision and try once more (a
                // second conflict means someone is writing right now).
                req.expected_revision = r.revision.clone();
                r = pl.save(req).await?.into_inner();
            }
            if r.conflict {
                if r.revision.is_empty() {
                    anyhow::bail!("playlist `{reference}` has no file any more: save it without --revision to create it");
                }
                anyhow::bail!(
                    "playlist `{reference}` exists ({}, revision {}): pass --revision <rev> (from `playlist export {reference} --file`) \
                     or --force to replace it",
                    r.file,
                    r.revision
                );
            }
            if !r.ok {
                println!("not saved:");
                print_diagnostics(&r.diagnostics);
                std::process::exit(1);
            }
            println!("{} {}", if r.created { "created:" } else { "saved:  " }, r.file);
            println!("id:       {}", r.id);
            println!("revision: {}", r.revision);
            if !r.diagnostics.is_empty() {
                print_diagnostics(&r.diagnostics);
            }
        }
    }
    Ok(())
}

fn print_file_errors(errors: &[playlist::FileError]) {
    for e in errors {
        if e.diagnostics.is_empty() {
            println!("  - {}: {}", e.path, e.message);
        } else {
            println!("  - {}:", e.path);
            print_diagnostics(&e.diagnostics);
        }
    }
}

#[cfg(test)]
mod preview_pool_tests {
    use super::fmt_pool;
    use prost_types::Duration;

    #[test]
    fn pool_display_keeps_unknown_fields_independent_and_preserves_milliseconds() {
        assert_eq!(fmt_pool(None, None), "pool: unknown media, duration unknown");
        assert_eq!(fmt_pool(None, Some(&Duration { seconds: 1200, nanos: 0 })),
            "pool: unknown media, duration 00:20:00");
        assert_eq!(fmt_pool(Some(0), Some(&Duration::default())),
            "pool: 0 media, duration 00:00:00");
        assert_eq!(fmt_pool(Some(1), Some(&Duration { seconds: 0, nanos: 250_000_000 })),
            "pool: 1 media, duration 00:00:00.250");
    }
}
