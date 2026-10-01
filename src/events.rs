//! The station's event journal (`EventService.Watch`, `stationctl events`,
//! the TUI's Système screen).
//!
//! A bounded, in-memory record of what happened, in order: typed facts
//! (state changes, grid applied, scan, tags written, plugin quarantined…)
//! and every `warn` / `error` the daemon logs. It is a view for the operator,
//! never a source of truth: nothing reads it back to decide anything, and it
//! starts empty at each start (the journal of the host keeps the full logs).
//!
//! D12 — stationd sends no display text: a typed fact is a [`Code`] with
//! named parameters, each client translates it. The one exception is
//! [`Code::Log`]: the text of a `warn` / `error` log line, shown as it is
//! (like a file name), because it is the daemon's own diagnostic.
//!
//! Process-wide: facts come from every corner (plugin actor, grid engine,
//! library actor, Icecast sampler…), so recording is a free function, never
//! blocking (a short lock, a broadcast send that never waits).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use tokio::sync::broadcast;

/// How many events are kept for a new watcher.
pub const CAPACITY: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Station,
    Broadcast,
    Grid,
    Library,
    Plugin,
    Live,
    Liquidsoap,
    Icecast,
}

/// What happened. Parameters are named in the variant's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// A `warn` / `error` log line: `message`, `target`, then its fields.
    Log,
    /// stationd started: `version`, `station`.
    Started,
    /// stationd is stopping: `by` (`quit`, `signal`, `operator`).
    Stopping,
    /// `from`, `to`, `by`.
    BroadcastState,
    /// The audience changed: `count`.
    Listeners,
    /// The audience could not be read (a failure, never « nobody »).
    AudienceUnknown,
    /// A track was chosen: `media` (empty on a fallback), `playlist`, `rule`,
    /// `origin`.
    TrackChosen,
    /// An override was queued: `media` or `playlist`, `mode`, `by`.
    OverridePushed,
    /// A queued override could not air and was dropped (missing file, empty
    /// pool, playlist error): `media` or `playlist`, `by`, `reason`.
    OverrideDropped,
    /// A grid was applied: `grid`, `rules`.
    GridApplied,
    /// The active grid file was refused (start-up, reload): `grid`, then
    /// `problems` (how many) or `error` (unreadable).
    GridRefused,
    /// A grid incident: `kind` (`hard_not_cut`, `source_empty`), `rule`,
    /// `playlist`.
    GridIncident,
    /// A DJ took the air: `dj`, `rule`.
    LiveStarted,
    /// The live ended: `dj`, `reason`.
    LiveEnded,
    /// A library scan started.
    ScanStarted,
    /// A scan finished: `found`, `skipped`, `present`, `unavailable`,
    /// `vanished`.
    ScanFinished,
    /// A scan failed: `error`.
    ScanFailed,
    /// A media's tags were written: `media`.
    TagsWritten,
    /// A value was renamed across the library: `from`, `to`, `files`,
    /// `failed`.
    TagRenamed,
    /// A plugin hook failed: `plugin`, `phase`, `reason`.
    PluginFailed,
    /// A plugin was quarantined: `plugin`, `failures`, `reason`.
    PluginQuarantined,
    /// A plugin changed state by request: `plugin`, `state`.
    PluginState,
    /// The BPM analysis of a scan: `estimated`, `failed`, then
    /// `failed_<kind>` counts (`decode`, `too_short`, `no_rhythm`, `weak`,
    /// `competing`, `disagree`).
    BpmAnalyzed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Increasing from 1 at each start.
    pub seq: u64,
    /// Epoch milliseconds.
    pub at_ms: i64,
    pub level: Level,
    pub component: Component,
    pub code: Code,
    pub params: Vec<(String, String)>,
}

impl Event {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

struct Journal {
    ring: Mutex<(u64, VecDeque<Event>)>,
    tx: broadcast::Sender<Event>,
}

fn journal() -> &'static Journal {
    static J: OnceLock<Journal> = OnceLock::new();
    J.get_or_init(|| Journal {
        ring: Mutex::new((0, VecDeque::with_capacity(CAPACITY))),
        tx: broadcast::channel(512).0,
    })
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Record a fact. Never blocks, never fails.
pub fn record<K: Into<String>, V: ToString>(
    level: Level,
    component: Component,
    code: Code,
    params: impl IntoIterator<Item = (K, V)>,
) {
    let params = params.into_iter().map(|(k, v)| (k.into(), v.to_string())).collect();
    push(level, component, code, params);
}

fn push(level: Level, component: Component, code: Code, params: Vec<(String, String)>) {
    let j = journal();
    let event = {
        let mut ring = j.ring.lock().unwrap_or_else(|p| p.into_inner());
        ring.0 += 1;
        let event = Event { seq: ring.0, at_ms: now_ms(), level, component, code, params };
        if ring.1.len() == CAPACITY {
            ring.1.pop_front();
        }
        ring.1.push_back(event.clone());
        event
    };
    // No receiver = nobody watching: fine.
    let _ = j.tx.send(event);
}

/// The last `backlog` events, and a receiver for what comes next — taken
/// under the same lock, so nothing falls between the two.
pub fn subscribe(backlog: usize) -> (Vec<Event>, broadcast::Receiver<Event>) {
    let j = journal();
    let ring = j.ring.lock().unwrap_or_else(|p| p.into_inner());
    let rx = j.tx.subscribe();
    let skip = ring.1.len().saturating_sub(backlog);
    (ring.1.iter().skip(skip).cloned().collect(), rx)
}

// ---------------------------------------------------------------------------
// `warn` / `error` log lines → `Code::Log`
// ---------------------------------------------------------------------------

/// A `tracing` layer that records every `warn` / `error` line in the
/// journal. A line that carries an `event` field is skipped: its fact is
/// already recorded, typed (`event = "grid_refused"` …).
pub struct LogLayer;

pub fn layer() -> LogLayer {
    LogLayer
}

#[derive(Default)]
struct Fields {
    message: String,
    typed: bool,
    rest: Vec<(String, String)>,
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field.name(), value.to_string());
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field.name(), format!("{value:?}"));
    }
}

impl Fields {
    fn put(&mut self, name: &str, value: String) {
        match name {
            "message" => self.message = value,
            "event" => self.typed = true,
            _ => self.rest.push((name.to_string(), value)),
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            _ => return,
        };
        let mut f = Fields::default();
        event.record(&mut f);
        if f.typed {
            return;
        }
        let target = event.metadata().target();
        let mut params = vec![("message".to_string(), f.message), ("target".to_string(), target.to_string())];
        params.extend(f.rest);
        push(level, component_of(target), Code::Log, params);
    }
}

/// The component a log line comes from, by its module.
fn component_of(target: &str) -> Component {
    let module = target.strip_prefix("stationd::").unwrap_or(target);
    let module = module.split("::").next().unwrap_or(module);
    match module {
        "grid_engine" | "grid_files" | "grid_index" | "grid_store" | "grid_toml" | "resolver" | "schedule_grpc"
        | "selection" | "playlist" | "playlist_edit" | "playlist_grpc" | "sync" => Component::Grid,
        "library_actor" | "library_grpc" | "media" | "media_index" | "media_tags" | "scan_writeback"
        | "bpm_analysis" => Component::Library,
        "plugin" | "plugin_db" | "plugin_grpc" => Component::Plugin,
        "live" | "live_grpc" | "live_opening" => Component::Live,
        "ls_bridge" | "ls_control" | "ls_grpc" | "ls_script" => Component::Liquidsoap,
        "icecast" | "icecast_grpc" | "icecast_xml" => Component::Icecast,
        "station_control" | "broadcast_grpc" | "onair" | "onair_sim" | "operator_stop" => Component::Broadcast,
        _ => Component::Station,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// Other tests record too (process-wide): look for our own marks.
    fn find(events: &[Event], mark: &str) -> Vec<Event> {
        events.iter().filter(|e| e.params.iter().any(|(_, v)| v.contains(mark))).cloned().collect()
    }

    #[test]
    fn facts_are_kept_in_order_and_follow_to_watchers() {
        let (_, mut rx) = subscribe(0);
        record(Level::Info, Component::Grid, Code::GridApplied, [("grid", "order-a.toml"), ("rules", "3")]);
        record(Level::Warn, Component::Grid, Code::GridIncident, [("rule", "order-b")]);
        let (backlog, _) = subscribe(CAPACITY);
        let mine = find(&backlog, "order-");
        assert_eq!(mine.len(), 2);
        assert!(mine[0].seq < mine[1].seq);
        assert_eq!(mine[0].param("rules"), Some("3"));
        // The watcher subscribed before: it got both, live.
        let mut live = Vec::new();
        while let Ok(e) = rx.try_recv() {
            live.push(e);
        }
        assert_eq!(find(&live, "order-").len(), 2);
    }

    #[test]
    fn warn_and_error_lines_are_journaled_unless_typed() {
        let subscriber = tracing_subscriber::registry().with(layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(mark = "log-mark-info", "not journaled");
            tracing::warn!(mark = "log-mark-warn", "disk is slow");
            tracing::error!(event = "grid_refused", mark = "log-mark-typed", "already typed");
        });
        let (backlog, _) = subscribe(CAPACITY);
        let mine = find(&backlog, "log-mark-");
        assert_eq!(mine.len(), 1, "{mine:?}");
        assert_eq!(mine[0].code, Code::Log);
        assert_eq!(mine[0].level, Level::Warn);
        assert_eq!(mine[0].param("message"), Some("disk is slow"));
    }

    #[test]
    fn a_module_names_its_component() {
        assert_eq!(component_of("stationd::icecast"), Component::Icecast);
        assert_eq!(component_of("stationd::grid_engine::x"), Component::Grid);
        assert_eq!(component_of("sqlx::query"), Component::Station);
    }
}
