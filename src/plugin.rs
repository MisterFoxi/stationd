//! Plugin system — native core (A1).
//!
//! First milestone of the plugin system, in-process and Rust-native (no WASM
//! yet): the plugin registry, the lifecycle state machine, and event dispatch.
//! It fixes the contract that a WASM runtime will later implement, per
//! `Doc/plugin-hooks.md` and `Doc/plugin-events.md`.
//!
//! Shape: a single owning actor (same template as `library_actor`) holds every
//! plugin slot and serialises three things — event dispatch, lifecycle control
//! (start/stop/restart/reload), and list. A clonable `PluginHandle` is the only
//! way in. `emit` is fire-and-forget (`try_send`, best-effort): emitting an
//! event must NEVER block or fail the caller (a track resolution), so a slow or
//! crashed plugin never holds the air.
//!
//! Scope: lifecycle + `on_event` (A1), `filter_pool` and `on_scan` (sync
//! hooks), and the
//! **host surface** (A2): a [`Host`] handed to the plugin in `on_load`, scoped
//! to that plugin and gated by the capabilities it DECLARES
//! (`capabilities = ["control", "push_override", "db"]`). Native plugins call
//! it directly; WASM plugins through extism host functions (`station_control`,
//! `push_override`, `db_query` / `db_exec` / `db_batch`, JSON in/out). The
//! per-plugin database (`db`) is opened by the core in `start`, the plugin's
//! migrations applied before `on_load` — see `plugin_db`.
//!
//! Failure handling (no-silent-failure, visible): a plugin that fails `on_load`
//! is recorded `Failed` and inactive — the daemon and other plugins still
//! start. A plugin that panics in `on_event` is caught; repeated failures in a
//! sliding window quarantine it (hooks no longer called) until an explicit
//! restart. Nothing retries silently in a loop.

use std::collections::VecDeque;
use std::path::PathBuf;

#[path = "plugin_path.rs"]
mod plugin_path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use extism::{host_fn, Function, Manifest, Plugin as ExtismPlugin, PluginBuilder, UserData, Wasm, PTR};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::plugin_db::{self, DbError, DbInspect, DbLimits, PluginDb, Rows};
use crate::station_control::{
    ControlAction, ControlError, OverrideRequest, PushOutcome, StationControl, Transition,
};

/// Sliding-window failure policy: N failures within WINDOW → quarantine.
const MAX_FAILURES: u32 = 3;
const WINDOW: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Contract: the trait a plugin implements, and the events it observes
// ---------------------------------------------------------------------------

/// A resolved candidate handed to `filter_pool`. Standard media attributes
/// (all from the media index) so a plugin can filter/score without its own
/// catalogue. A plugin returns the subset it keeps (v1: filtering only —
/// reordering/weighting comes later).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub rel_path: String,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub duration_ms: u64,
    pub genres: Vec<String>,
    pub mtime_ns: i64,
}

/// One scanned media handed to `on_scan`: its standard attributes (as the
/// scanner read them) plus the user-defined tags the core does not interpret
/// (`TXXX`, Vorbis/APE keys, MP4 freeform — see `media::CustomTag`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanInput {
    pub rel_path: String,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub duration_ms: u64,
    pub genres: Vec<String>,
    pub custom_tags: Vec<crate::media::CustomTag>,
}

/// What a plugin adds to one media at scan time. v1: extra genres only — they
/// land in `media_genre` like any tag genre, so the playlist `genre` filters
/// (`has`/`has_any`/`has_all`/`has_none`) see them with no new grammar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanEnrichment {
    pub rel_path: String,
    #[serde(default)]
    pub genres: Vec<String>,
    /// Scalar custom metadata. Missing in legacy plugin replies means empty.
    #[serde(default)]
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// What a plugin implements. `Send` because plugins live on the actor task.
///
/// A1 wires `on_load` / `on_unload` (lifecycle) and `on_event` (observation).
/// A2 adds the synchronous hooks: `filter_pool` (influences the decision) and
/// `on_scan` (enriches the library scan).
pub trait Plugin: Send {
    /// Background-only plugins can opt out of the musical filter/simulation path.
    fn filters_pool(&self) -> bool { true }
    /// Trusted CLI administration through gRPC; never forwarded from a Web request.
    fn admin_request(&mut self, _payload: &str) -> Result<String, String> {
        Err("plugin does not support administration requests".into())
    }
    /// Optional configuration fields discovered before on_load without write access.
    fn config_schema(&mut self) -> Result<Vec<crate::plugin_config::Field>, String> { Ok(Vec::new()) }
    fn validate_config(&mut self, _config: &toml::Table, _host: &Host) -> Result<(), String> { Ok(()) }
    /// Optional tabs discovered once after on_load, without write access.
    fn ui_tabs(&mut self) -> Result<Vec<crate::plugin_ui::UiTab>, String> {
        Ok(Vec::new())
    }
    /// Stable name (matches the declaration). Identity for `plugin list`.
    fn name(&self) -> &str;

    /// Activate: open resources, read config. Synchronous, bounded, may fail —
    /// a failure refuses the plugin (state `Failed{load}`), others start anyway.
    /// `host` is this plugin's host surface (scoped to it, gated by its
    /// declared capabilities); keep a clone to act later.
    fn on_load(&mut self, _host: Host) -> Result<(), String> {
        Ok(())
    }

    /// Deactivate: flush, close. Best-effort; a panic here is swallowed and the
    /// plugin still ends `Disabled`.
    fn on_unload(&mut self) {}

    /// Observe a fact that already happened. Returns nothing, must not assume
    /// it influences anything. A panic is caught and counts as a failure.
    fn on_event(&mut self, _event: &PluginEvent) {}

    /// Filter (v1) the resolved candidate pool just before the final pick.
    /// Synchronous — it returns into the decision path, so it must be quick and
    /// bounded. Default keeps everything. Returning fewer candidates removes
    /// them; an empty pool means "nothing acceptable" (→ PoolEmpty downstream).
    /// A panic is caught: that stage degrades to pass-through and counts as a
    /// failure.
    fn filter_pool(&mut self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        candidates
    }

    /// Enrich the scanned library, once per scan with the whole batch (one
    /// boundary crossing, not one per file). Returns only the media it has
    /// something to add to. Synchronous, in the scan path. An `Err` or a panic
    /// counts as a failure (visible in `plugin list`) and that plugin's
    /// enrichment is dropped for this scan: media are still indexed, just
    /// without it (degraded, never lost). Default: nothing to add.
    fn on_scan(&mut self, _media: &[ScanInput]) -> Result<Vec<ScanEnrichment>, String> {
        Ok(Vec::new())
    }

    /// The schema of this plugin's database (capability `db`): ordered
    /// migrations, `[i]` = version `i + 1`, each one or more SQL statements.
    /// Applied by the core before `on_load`; an applied one must never change
    /// (ship a new one). Called only when `db` is declared. Default: none.
    fn db_migrations(&mut self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// Facts the core notifies plugins about. `#[non_exhaustive]`: a plugin must
/// `_ => {}` on unknown variants, so adding events never breaks a plugin.
///
/// Emitted today: `TrackResolved` (grid engine), `ListenersSampled` (Icecast
/// sampler), `BroadcastStateChanged` (station control), `LiveStarted` /
/// `LiveEnded` (live DJs, `live::LiveHub`). Others
/// from `Doc/plugin-events.md` (`TrackSkipped`, `LibraryScanned`, `GridApplied`,
/// …) are added as their sources come online.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginEvent {
    /// A decision was just taken (`GridEngine::next_media`). `media_path` is
    /// `None` on a fallback. `origin` is the resolver origin, as a short label.
    TrackResolved {
        media_path: Option<String>,
        playlist_ref: Option<String>,
        rule_id: Option<String>,
        origin: String,
    },
    /// An audience sample (Icecast later; `stationctl debug listeners` today).
    /// `at` = epoch seconds.
    ListenersSampled { count: u32, at: i64 },
    /// Complete station-wide privacy-reduced snapshot. None means unknown.
    ConnectionsSampled {
        at: i64,
        connections: Option<Vec<crate::listener_snapshot::Connection>>,
    },
    /// Per-mount observation, gated by listener_details. None means unknown;
    /// Some([]) means a successful empty snapshot. Never journal raw IPs.
    ListenerSnapshot {
        mount: String,
        at: i64,
        listeners: Option<Vec<crate::listener_snapshot::Listener>>,
    },
    /// The broadcast state changed (`running|paused|draining|sleeping`). `by`
    /// names the emitter: a plugin, `cli`, `stop-when-idle` for a drain
    /// completed by the core at a track boundary, `live` / `audience-unknown`
    /// for a sleeping station woken by the core.
    BroadcastStateChanged { from: String, to: String, by: String },
    /// A DJ took the air (harbor): `dj` = its id in the DJ file, `rule_id` =
    /// the grid `live` rule whose window let it in. `at` = epoch seconds.
    LiveStarted { dj: String, rule_id: String, at: i64 },
    /// The live ended: `reason` = `disconnected` (the DJ left), `silence`
    /// (cut after `[live] silence_timeout`) or `kicked` (`stationctl live
    /// kick`). The programme resumes with a track chosen at that instant.
    LiveEnded { dj: String, reason: String, at: i64 },
}

/// The facts the plugins hear are also the station's: into the journal.
/// Listener samples only when the count changes (a sample every few
/// seconds would drown the rest).
static LAST_LISTENERS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

fn journal(event: &PluginEvent) {
    use crate::events::{record, Code, Component, Level};
    use std::sync::atomic::Ordering;
    match event {
        PluginEvent::ListenerSnapshot { .. } | PluginEvent::ConnectionsSampled { .. } => {},
        PluginEvent::TrackResolved { media_path, playlist_ref, rule_id, origin } => record(
            Level::Info,
            Component::Grid,
            Code::TrackChosen,
            [
                ("media", media_path.clone().unwrap_or_default()),
                ("playlist", playlist_ref.clone().unwrap_or_default()),
                ("rule", rule_id.clone().unwrap_or_default()),
                ("origin", origin.clone()),
            ],
        ),
        PluginEvent::ListenersSampled { count, .. } => {
            if LAST_LISTENERS.swap(i64::from(*count), Ordering::Relaxed) != i64::from(*count) {
                record(Level::Info, Component::Icecast, Code::Listeners, [("count", count.to_string())]);
            }
        }
        PluginEvent::BroadcastStateChanged { from, to, by } => record(
            Level::Info,
            Component::Broadcast,
            Code::BroadcastState,
            [("from", from.clone()), ("to", to.clone()), ("by", by.clone())],
        ),
        PluginEvent::LiveStarted { dj, rule_id, .. } => {
            record(Level::Info, Component::Live, Code::LiveStarted, [("dj", dj.clone()), ("rule", rule_id.clone())])
        }
        PluginEvent::LiveEnded { dj, reason, .. } => {
            record(Level::Info, Component::Live, Code::LiveEnded, [("dj", dj.clone()), ("reason", reason.clone())])
        }
    }
}

/// An unknown audience (a failed read) is said once, until a count comes back.
pub fn journal_audience_unknown() {
    LAST_LISTENERS.store(-1, std::sync::atomic::Ordering::Relaxed);
    crate::events::record(
        crate::events::Level::Warn,
        crate::events::Component::Icecast,
        crate::events::Code::AudienceUnknown,
        std::iter::empty::<(&str, &str)>(),
    );
}

// ---------------------------------------------------------------------------
// Host surface (A2): what a plugin may call on the core
// ---------------------------------------------------------------------------

/// A capability a plugin must declare to use the matching host call. Closed
/// set: an unknown name is a config error (loud, at start-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// `host.control(...)` — stop / pause / resume / stop-when-idle.
    Control,
    /// `host.push_override(...)` — content ahead of the grid.
    PushOverride,
    /// `host.db()` — the plugin's own SQLite database (`plugin_db`).
    Db,
    /// Receive snapshots containing transient client IPs and user agents.
    ListenerDetails,
    /// Ask the host for country/city from its local DB-IP City Lite file.
    Geoip,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Control => "control",
            Capability::PushOverride => "push_override",
            Capability::Db => "db",
            Capability::ListenerDetails => "listener_details",
            Capability::Geoip => "geoip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("plugin `{plugin}` did not declare capability `{capability}`")]
    Denied { plugin: String, capability: &'static str },
    #[error("no station control is wired")]
    Unavailable,
    #[error("no database is open for this plugin")]
    NoDatabase,
    /// A mutating host call made while the core runs this plugin for a
    /// SIMULATION (the on-air preview): refused, nothing happens.
    #[error("`{0}` refused: the core is running a simulation (nothing may change)")]
    Simulation(&'static str),
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// A plugin's host surface, handed over in `on_load`. Scoped: it carries the
/// plugin's name (recorded as the emitter of every action) and its declared
/// capabilities; a call outside them is refused and logged, never performed.
/// Capabilities, never paths or handles (sandboxing).
#[derive(Clone)]
pub struct Host {
    operator_notice: Arc<Mutex<Option<OperatorNotice>>>,
    plugin: String,
    capabilities: Vec<Capability>,
    control: Option<StationControl>,
    db: Option<Arc<PluginDb>>,
    geoip: Option<Arc<crate::geoip::Geoip>>,
    /// Set by the core around a hook it runs for a simulation: every clone
    /// of this host (the plugin keeps one, WASM host functions capture one)
    /// sees it. While set, `control`, `push_override` and database writes
    /// are refused.
    simulating: Arc<AtomicBool>,
}

impl Host {
    pub fn new(plugin: &str, capabilities: &[Capability], control: Option<StationControl>) -> Self {
        Self {
            operator_notice: Arc::new(Mutex::new(None)),
            plugin: plugin.to_string(),
            capabilities: capabilities.to_vec(),
            control,
            db: None,
            geoip: None,
            simulating: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Enter / leave simulation mode (the core, around a simulated hook):
    /// mutating calls are refused meanwhile, the database is read-only.
    fn set_simulating(&self, on: bool) {
        self.simulating.store(on, Ordering::SeqCst);
        if let Some(db) = &self.db {
            db.set_read_only(on);
        }
    }

    fn refuse_in_simulation(&self, call: &'static str) -> Result<(), HostError> {
        if self.simulating.load(Ordering::SeqCst) {
            return Err(HostError::Simulation(call));
        }
        Ok(())
    }

    /// Attach the plugin's database (opened by the core in `Slot::start`).
    pub fn with_db(mut self, db: Arc<PluginDb>) -> Self {
        self.db = Some(db);
        self
    }

    pub fn plugin_name(&self) -> &str {
        &self.plugin
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    fn check(&self, capability: Capability) -> Result<(), HostError> {
        if !self.has(capability) {
            let err = HostError::Denied {
                plugin: self.plugin.clone(),
                capability: capability.as_str(),
            };
            tracing::warn!(%err, "host call refused");
            return Err(err);
        }
        Ok(())
    }

    fn require(&self, capability: Capability) -> Result<&StationControl, HostError> {
        self.check(capability)?;
        self.control.as_ref().ok_or(HostError::Unavailable)
    }

    /// The plugin's own database. Needs capability `db`.
    pub fn db(&self) -> Result<&PluginDb, HostError> {
        self.check(Capability::Db)?;
        self.db.as_deref().ok_or(HostError::NoDatabase)
    }

    /// Pilot the broadcast (first-class station control; the plugin is just
    /// one more emitter). Needs capability `control`.
    pub fn control(&self, action: ControlAction) -> Result<Option<Transition>, HostError> {
        self.refuse_in_simulation("control")?;
        Ok(self.require(Capability::Control)?.apply(action, &self.plugin)?)
    }

    /// One indication per plugin; visible only while its slot is Loaded.
    pub fn set_operator_notice(&self, notice: OperatorNotice) -> Result<(), HostError> {
        self.refuse_in_simulation("operator_notice")?;
        self.check(Capability::Control)?;
        *self.operator_notice.lock().unwrap_or_else(|p| p.into_inner()) = Some(notice);
        Ok(())
    }

    fn operator_notice(&self) -> Option<OperatorNotice> {
        self.operator_notice.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn connection_sampling_enabled(&self) -> Result<bool, HostError> {
        Ok(self.control.as_ref().ok_or(HostError::Unavailable)?.connection_sampling_enabled())
    }

    /// Privacy-reduced connections, available without listener_details.
    pub fn listener_connections(&self) -> Result<Option<Vec<crate::listener_snapshot::Connection>>, HostError> {
        Ok(self.control.as_ref().ok_or(HostError::Unavailable)?.listener_connections())
    }

    pub fn stop_when_connections_old(&self, max_age: u64) -> Result<Option<Transition>, HostError> {
        self.refuse_in_simulation("control")?;
        Ok(self.require(Capability::Control)?.stop_when_connections_old(max_age, &self.plugin)?)
    }

    /// Push content ahead of the grid. Needs capability `push_override`.
    pub fn push_override(&self, req: OverrideRequest) -> Result<PushOutcome, HostError> {
        self.refuse_in_simulation("push_override")?;
        Ok(self
            .require(Capability::PushOverride)?
            .push_override(req, &self.plugin)?)
    }
}

// ---------------------------------------------------------------------------
// Declaration (config) and runtime state
// ---------------------------------------------------------------------------

/// See [`PluginHandle::tag_hints`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagHints {
    /// User frames (`TXXX`) whose values become genres.
    pub genre_sources: Vec<String>,
    /// Labels of the tempo ranges, in configuration order.
    pub tempo_labels: Vec<String>,
}

/// One `[[plugin]]` entry in the station config. `name` is both identity and
/// kind in A1 (one instance per kind); a separate `kind` for multiple
/// instances is a later refinement. `config` is opaque to the core and handed
/// to the plugin at build time.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDecl {
    pub name: String,
    /// Activated at start-up (`on_load`). Default `false`: declaring a plugin
    /// arms it (listed as `disabled`, startable by `stationctl plugin start`)
    /// without activating it — activation is always explicit.
    #[serde(default)]
    pub enabled: bool,
    /// Hook application order, ascending; default 50 (neutral rank). Ties
    /// broken by name for determinism.
    #[serde(default = "default_order")]
    pub order: u32,
    /// Optional WASM path override. Existing files win; missing paths resolve
    /// by filename. Without a path, built-ins win, then WASM is found by name.
    #[serde(default)]
    pub wasm: Option<String>,
    /// Host calls this plugin may make (`control`, `push_override`, `db`).
    /// Empty by default: a plugin acts on nothing unless it says so.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// `[plugin.db]` — bounds of the plugin's database (capability `db`
    /// only; defaults apply when absent).
    #[serde(default)]
    pub db: Option<DbLimits>,
    #[serde(default)]
    pub config: toml::Table,
}

impl PluginDecl {
    /// `custom-tags` configuration read for a tag editor (see
    /// [`PluginHandle::tag_hints`]).
    fn tag_hints(&self) -> TagHints {
        let strings = |v: Option<&toml::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default()
        };
        let mut tempo_labels = Vec::new();
        if let Some(ranges) = self.config.get("tempo").and_then(|t| t.get("range")).and_then(|r| r.as_array()) {
            for r in ranges {
                if let Some(v) = r.get("value").and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty()) {
                    if !tempo_labels.iter().any(|x: &String| x == v) {
                        tempo_labels.push(v.to_string());
                    }
                }
            }
        }
        TagHints { genre_sources: strings(self.config.get("tags")), tempo_labels }
    }

    fn has_db(&self) -> bool {
        self.capabilities.contains(&Capability::Db)
    }

    fn db_limits(&self) -> DbLimits {
        self.db.unwrap_or_default()
    }
}

/// Check the declared plugins as a whole (at config load, loud): names are
/// unique (a name is the plugin's identity, and its database file); a plugin
/// with capability `db` has a file-safe name and valid `[plugin.db]` bounds;
/// `[plugin.db]` without the capability is a mistake, not ignored.
pub fn validate_decls(decls: &[PluginDecl]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for d in decls {
        if !seen.insert(d.name.as_str()) {
            return Err(format!("plugin `{}` is declared twice", d.name));
        }
        if d.wasm.is_none() && d.name == "remote-supervision" {
            remote_supervision::validate_config(&d.config)?;
        }
        if d.has_db() {
            plugin_db::validate_name(&d.name)?;
            d.db_limits()
                .validate()
                .map_err(|e| format!("plugin `{}`: [plugin.db] {e}", d.name))?;
        } else if d.db.is_some() {
            return Err(format!(
                "plugin `{}`: [plugin.db] is set but capability `db` is not declared",
                d.name
            ));
        }
    }
    Ok(())
}

/// What the plugin actor needs from the daemon: the station control behind
/// the host surface, and where plugin databases live (`None` = capability
/// `db` unavailable: such a plugin fails to start, visibly).
#[derive(Clone, Default)]
pub struct PluginEnv {
    pub control: Option<StationControl>,
    pub db_dir: Option<PathBuf>,
    pub geoip: Option<Arc<crate::geoip::Geoip>>,
}

fn default_order() -> u32 {
    50
}

/// Which lifecycle/hook phase a failure occurred in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Load,
    Unload,
    Event,
    FilterPool,
    Scan,
    /// Opening the plugin's database or applying its migrations.
    Migrate,
}

/// Runtime state of a plugin — kept even when inactive, so it stays visible.
#[derive(Debug, Clone)]
pub enum PluginState {
    Loaded,
    Disabled,
    Failed { phase: Phase, reason: String },
    Quarantined { reason: String, failures: u32 },
}

impl PluginState {
    fn label(&self) -> &'static str {
        match self {
            PluginState::Loaded => "loaded",
            PluginState::Disabled => "disabled",
            PluginState::Failed { .. } => "failed",
            PluginState::Quarantined { .. } => "quarantined",
        }
    }
    fn reason(&self) -> String {
        match self {
            PluginState::Failed { phase, reason } => format!("{phase:?}: {reason}"),
            PluginState::Quarantined { reason, .. } => reason.clone(),
            _ => String::new(),
        }
    }
}

/// A typed operator indication published by a plugin, independent of its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorNotice {
    AutoSleep { max_connection_age: Option<u64> },
}

/// A flat, cloneable snapshot of a plugin for `plugin list`.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub configurable: bool,
    pub operator_notice: Option<OperatorNotice>,
    pub tabs: Vec<crate::plugin_ui::UiTab>,
    pub name: String,
    pub enabled: bool,
    pub order: u32,
    pub state: String,
    pub reason: String,
    pub failures: u32,
    /// Declared capabilities (what the plugin may call on the core).
    pub capabilities: Vec<String>,
}

/// Lifecycle action requested via `plugin start|stop|restart|reload`.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Reload,
}

// ---------------------------------------------------------------------------
// Slot: a declared plugin plus its live state
mod config_host;
mod metadata_config;
mod remote_supervision;
use config_host::{ConfigPlugin, config_operation, validate_candidate};

// ---------------------------------------------------------------------------

struct Slot {
    schema: Vec<crate::plugin_config::Field>,
    tabs: Vec<crate::plugin_ui::UiTab>,
    decl: PluginDecl,
    state: PluginState,
    plugin: Option<Box<dyn Plugin>>,
    failures: VecDeque<Instant>,
    /// The loaded plugin's host surface (a clone), to switch it into
    /// simulation mode around a simulated hook. `None` when not loaded.
    host: Option<Host>,
}

impl Slot {
    fn info(&self) -> PluginInfo {
        let failures = match &self.state {
            PluginState::Quarantined { failures, .. } => *failures,
            _ => self.failures.len() as u32,
        };
        PluginInfo {
            configurable: !self.schema.is_empty(),
            tabs: self.tabs.clone(),
            operator_notice: if matches!(self.state, PluginState::Loaded) {
                self.host.as_ref().and_then(Host::operator_notice)
            } else { None },
            name: self.decl.name.clone(),
            enabled: self.decl.enabled,
            order: self.decl.order,
            state: self.state.label().to_string(),
            reason: self.state.reason(),
            failures,
            capabilities: self
                .decl
                .capabilities
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
        }
    }

    /// (Re)build the instance and run `on_load` with its scoped host surface.
    /// With capability `db`: open the plugin's database first, then apply its
    /// migrations (failure → `Failed{migrate}`, `on_load` not called).
    /// Sets `Loaded` or `Failed`.
    fn start(&mut self, env: &PluginEnv) {
        let Some(host) = self.open_host(env) else { return };
        match build_plugin(&self.decl, &host) {
            Err(reason) => {
                self.plugin = None;
                self.state = PluginState::Failed { phase: Phase::Load, reason };
            }
            Ok(plugin) => self.migrate_and_load(plugin, host),
        }
    }

    /// The plugin's host surface; with capability `db`, its database is
    /// opened here — before the instance is built, since a WASM plugin's host
    /// functions capture the host. `None` = failed (state set).
    fn open_host(&mut self, env: &PluginEnv) -> Option<Host> {
        let mut host = Host::new(&self.decl.name, &self.decl.capabilities, env.control.clone());
        host.geoip = env.geoip.clone();
        if !self.decl.has_db() {
            return Some(host);
        }
        let opened = match &env.db_dir {
            None => Err("capability `db` declared but no plugin database directory is configured".to_string()),
            Some(dir) => PluginDb::open(dir, &self.decl.name, self.decl.db_limits())
                .map_err(|e| e.to_string()),
        };
        match opened {
            Ok(db) => Some(host.with_db(Arc::new(db))),
            Err(reason) => {
                self.plugin = None;
                self.state = PluginState::Failed { phase: Phase::Migrate, reason };
                None
            }
        }
    }

    /// Apply the plugin's migrations (when it has a database), then `on_load`.
    fn migrate_and_load(&mut self, mut plugin: Box<dyn Plugin>, host: Host) {
        if let Some(db) = host.db.clone() {
            let migrated = catch(|| plugin.db_migrations())
                .and_then(|r| r)
                .and_then(|m| db.migrate(&m).map_err(|e| e.to_string()));
            match migrated {
                Ok(o) if o.applied > 0 => tracing::info!(
                    plugin = %self.decl.name,
                    applied = o.applied,
                    version = o.version,
                    "plugin database migrated"
                ),
                Ok(_) => {}
                Err(reason) => {
                    self.plugin = None;
                    self.state = PluginState::Failed { phase: Phase::Migrate, reason };
                    return;
                }
            }
        }
        self.load(plugin, host);
    }

    /// Run `on_load` with the host surface; `Loaded` or `Failed`.
    fn load(&mut self, mut plugin: Box<dyn Plugin>, host: Host) {
        let kept = host.clone();
        kept.set_simulating(true);
        let schema = catch(|| plugin.config_schema()).and_then(|r| r).and_then(|s| { crate::plugin_config::validate_schema(&s)?; Ok(s) });
        kept.set_simulating(false);
        match schema { Ok(s) => self.schema = s, Err(reason) => { self.state = PluginState::Failed { phase: Phase::Load, reason }; return; } }
        let loaded = catch(|| plugin.on_load(host)).and_then(|r| r).and_then(|()| {
            kept.set_simulating(true);
            let result = catch(|| plugin.ui_tabs()).and_then(|r| r);
            kept.set_simulating(false);
            let tabs = result?;
            crate::plugin_ui::validate(&tabs, self.decl.has_db())?;
            Ok(tabs)
        });
        match loaded {
            Ok(tabs) => {
                self.tabs = tabs;
                self.host = Some(kept);
                self.plugin = Some(plugin);
                self.state = PluginState::Loaded;
                self.failures.clear();
                if let Some(OperatorNotice::AutoSleep { max_connection_age }) =
                    self.host.as_ref().and_then(Host::operator_notice)
                {
                    crate::events::record(crate::events::Level::Info, crate::events::Component::Plugin,
                        crate::events::Code::PluginModeEnabled, [
                            ("plugin", self.decl.name.clone()),
                            ("kind", "auto_sleep".into()),
                            ("max_connection_age", max_connection_age.map(|age| age.to_string()).unwrap_or_default()),
                        ]);
                }
            }
            Err(reason) => {
                let _ = catch(|| plugin.on_unload());
                self.plugin = None;
                self.state = PluginState::Failed { phase: Phase::Load, reason };
            }
        }
    }

    /// Run `on_unload` (best-effort) and drop the instance → `Disabled`.
    fn stop(&mut self) {
        if let Some(mut plugin) = self.plugin.take() {
            let _ = catch(|| plugin.on_unload());
        }
        self.host = None;
        self.state = PluginState::Disabled;
        self.failures.clear();
    }

    /// Record a runtime hook failure; quarantine past the sliding-window limit.
    fn note_failure(&mut self, phase: Phase, reason: String) {
        tracing::warn!(event = "plugin_failed", plugin = %self.decl.name, ?phase, %reason, "plugin hook failed");
        crate::events::record(
            crate::events::Level::Warn,
            crate::events::Component::Plugin,
            crate::events::Code::PluginFailed,
            [
                ("plugin", self.decl.name.clone()),
                ("phase", format!("{phase:?}").to_lowercase()),
                ("reason", reason.clone()),
            ],
        );
        let now = Instant::now();
        self.failures.push_back(now);
        while let Some(&front) = self.failures.front() {
            if now.duration_since(front) > WINDOW {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        if self.failures.len() as u32 >= MAX_FAILURES {
            let failures = self.failures.len() as u32;
            tracing::warn!(event = "plugin_quarantined", plugin = %self.decl.name, failures, "plugin quarantined");
            crate::events::record(
                crate::events::Level::Error,
                crate::events::Component::Plugin,
                crate::events::Code::PluginQuarantined,
                [("plugin", self.decl.name.clone()), ("failures", failures.to_string()), ("reason", reason.clone())],
            );
            self.plugin = None;
            self.state = PluginState::Quarantined { reason, failures };
        }
    }
}

/// Run a closure, turning a panic into an `Err(String)` so a misbehaving
/// plugin cannot unwind into the core.
fn catch<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|e| {
        if let Some(s) = e.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = e.downcast_ref::<String>() {
            s.clone()
        } else {
            "panicked".to_string()
        }
    })
}

/// Map a declaration to a plugin instance: a `.wasm` module when `wasm` is
/// set (its host functions bound to `host`), else a built-in by name. An
/// unknown name is a loud (visible) failure, never silently ignored.
fn build_plugin(decl: &PluginDecl, host: &Host) -> Result<Box<dyn Plugin>, String> {
    if decl.wasm.is_none() {
        match decl.name.as_str() {
            "plugin-config" => return Ok(Box::new(ConfigPlugin)),
            "remote-supervision" => {
                return Ok(Box::new(remote_supervision::RemoteSupervision::from_config(
                    &decl.config,
                )?));
            }
            "logger" => return Ok(Box::new(LoggerPlugin::from_config(&decl.config))),
            "blacklist" => return Ok(Box::new(BlacklistPlugin::from_config(&decl.config))),
            "stop-when-idle" => return Ok(Box::new(StopWhenIdlePlugin::from_config(&decl.config)?)),
            _ => {}
        }
    }
    let path = plugin_path::resolve(&decl.name, decl.wasm.as_deref()).map_err(|e| {
        if decl.wasm.is_none() { format!("unknown plugin kind `{}`: {e}", decl.name) } else { e }
    })?;
    tracing::info!(plugin = %decl.name, path = %path.display(), "resolved WASM plugin");
    let path = path.to_str().ok_or_else(|| format!("plugin {}: chemin WASM non UTF-8", decl.name))?;
    WasmPlugin::new(decl.name.clone(), path, &decl.config, host)
        .map(|p| Box::new(p) as Box<dyn Plugin>)
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

enum Msg {
    Config { request: crate::proto::plugin::PluginConfigUpdateRequest, read: bool, reply: oneshot::Sender<Result<crate::proto::plugin::PluginConfigResponse, String>> },
    TabLocate {
        name: String,
        tab_id: String,
        reply: oneshot::Sender<Result<(DbLocation, String), DbAdminError>>,
    },
    List(oneshot::Sender<Vec<PluginInfo>>),
    TagHints(oneshot::Sender<TagHints>),
    Control {
        name: String,
        action: Action,
        reply: oneshot::Sender<Result<PluginInfo, String>>,
    },
    Admin { name: String, payload: String, reply: oneshot::Sender<Result<String, String>> },
    Event(PluginEvent),
    FilterPool {
        candidates: Vec<Candidate>,
        /// Run for the on-air simulation: mutations refused, failures not
        /// counted, reported back instead.
        simulation: bool,
        reply: oneshot::Sender<(Vec<Candidate>, Vec<(String, String)>)>,
    },
    Scan {
        media: Vec<ScanInput>,
        reply: oneshot::Sender<ScanExtras>,
    },
    DbLocate {
        name: String,
        reply: oneshot::Sender<Result<DbLocation, DbAdminError>>,
    },
    DbReset {
        name: String,
        reply: oneshot::Sender<Result<bool, DbAdminError>>,
    },
}

/// Where a plugin's database lives, for the admin reads.
#[derive(Debug, Clone)]
struct DbLocation {
    path: PathBuf,
    limits: DbLimits,
}

/// Why an admin operation on a plugin's database was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbAdminError {
    #[error("unknown plugin `{0}`")]
    UnknownPlugin(String),
    /// The plugin has no database (no capability `db`, or no directory), or
    /// it is loaded (`reset`).
    #[error("{0}")]
    Precondition(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// `stationctl plugin db <name> info`.
#[derive(Debug, Clone, PartialEq)]
pub struct DbInfo {
    pub path: PathBuf,
    pub limits: DbLimits,
    /// `None` = the file does not exist yet (plugin never started).
    pub inspect: Option<DbInspect>,
}

/// Merged `on_scan` output: extra genres per `rel_path`, every loaded plugin
/// contributing in `order`. Deduplicated case-insensitively per media.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanAddition {
    pub genres: Vec<String>,
    pub metadata: std::collections::BTreeMap<String, String>,
}

pub type ScanExtras = std::collections::BTreeMap<String, ScanAddition>;

/// Cheap, clonable handle to the plugin actor. The only way to reach plugins.
#[derive(Clone)]
pub struct PluginHandle {
    tx: mpsc::Sender<Msg>,
    /// `Some` = a simulation handle ([`PluginHandle::simulation`]): events are
    /// not emitted, `filter_pool` runs in simulation mode and what went wrong
    /// is collected here.
    sim: Option<Arc<Mutex<Vec<(String, String)>>>>,
}

impl PluginHandle {
    pub async fn admin(&self, name: &str, payload: String) -> Result<String, String> {
        if self.sim.is_some() || payload.len() > 16384 { return Err("administration request refused".into()); }
        let (reply, rx) = oneshot::channel();
        self.tx.send(Msg::Admin { name: name.into(), payload, reply }).await.map_err(|_| "plugin actor unavailable")?;
        rx.await.map_err(|_| "plugin actor unavailable")?
    }
    pub async fn config(&self, request: crate::proto::plugin::PluginConfigUpdateRequest, read: bool) -> Result<crate::proto::plugin::PluginConfigResponse, String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Msg::Config { request, read, reply: tx }).await.map_err(|_| "plugin actor unavailable")?;
        rx.await.map_err(|_| "plugin actor unavailable")?
    }

    /// What the loaded `custom-tags` plugin makes of a file's tags, for a
    /// tag editor: the user frames it turns into genres (`tags`), and the
    /// tempo labels of its BPM ranges. Empty when it is not loaded.
    pub async fn tag_hints(&self) -> TagHints {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Msg::TagHints(reply)).await.is_err() {
            return TagHints::default();
        }
        rx.await.unwrap_or_default()
    }

    /// Fire-and-forget: emit an event to the plugins. Never blocks the caller;
    /// if the buffer is full the event is dropped (best-effort, as documented).
    pub fn emit(&self, event: PluginEvent) {
        if self.sim.is_some() {
            return; // a simulated decision is not a fact: plugins never hear of it
        }
        journal(&event);
        let _ = self.tx.try_send(Msg::Event(event));
    }

    /// A handle for the on-air simulation: same plugins, but no event is ever
    /// emitted, `filter_pool` runs with every mutating host call refused
    /// (control, push_override, database writes) and a plugin failing then is
    /// NOT counted towards quarantine — it is reported in
    /// [`PluginHandle::simulation_notes`] and its stage passes through.
    pub fn simulation(&self) -> PluginHandle {
        PluginHandle { tx: self.tx.clone(), sim: Some(Arc::new(Mutex::new(Vec::new()))) }
    }

    /// What went wrong in the plugins during this simulation, as
    /// `(plugin, reason)` (deduplicated).
    pub fn simulation_notes(&self) -> Vec<(String, String)> {
        self.sim
            .as_ref()
            .map(|n| n.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .unwrap_or_default()
    }

    /// Run the candidate pool through every loaded plugin's `filter_pool`, in
    /// `order`. Awaits a reply (it feeds the decision). If the actor is gone,
    /// degrades to the unfiltered pool rather than failing the decision.
    pub async fn filter_pool(&self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        let (reply, rx) = oneshot::channel();
        let fallback = candidates.clone();
        let simulation = self.sim.is_some();
        if self
            .tx
            .send(Msg::FilterPool { candidates, simulation, reply })
            .await
            .is_err()
        {
            return fallback;
        }
        match rx.await {
            Ok((kept, notes)) => {
                if let Some(sim) = &self.sim {
                    let mut all = sim.lock().unwrap_or_else(|p| p.into_inner());
                    for n in notes {
                        if !all.contains(&n) {
                            all.push(n);
                        }
                    }
                }
                kept
            }
            Err(_) => fallback,
        }
    }

    /// Run the scanned batch through every loaded plugin's `on_scan`. Awaits a
    /// reply (it feeds the index). If the actor is gone, degrades to "nothing
    /// to add" with a warning rather than failing the scan.
    pub async fn on_scan(&self, media: Vec<ScanInput>) -> ScanExtras {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Msg::Scan { media, reply }).await.is_err() {
            tracing::warn!("plugin actor gone: scan indexed without plugin enrichment");
            return ScanExtras::new();
        }
        rx.await.unwrap_or_else(|_| {
            tracing::warn!("plugin actor gone: scan indexed without plugin enrichment");
            ScanExtras::new()
        })
    }

    /// Snapshot of every declared plugin and its state.
    pub async fn list(&self) -> Vec<PluginInfo> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Msg::List(reply)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// SQL stays server-side; only declared tabs of loaded plugins are readable.
    pub async fn read_tab(&self, name: &str, tab_id: &str) -> Result<Rows, DbAdminError> {
        let (reply, rx) = oneshot::channel();
        let gone = || DbAdminError::Precondition("plugin actor is no longer running".into());
        self.tx.send(Msg::TabLocate { name: name.into(), tab_id: tab_id.into(), reply })
            .await.map_err(|_| gone())?;
        let (loc, sql) = rx.await.map_err(|_| gone())??;
        tokio::task::spawn_blocking(move || {
            plugin_db::query_file_bounded(&loc.path, &sql, loc.limits.max_rows.min(1000), loc.limits.query_timeout_ms.min(1000))
        }).await.map_err(|e| DbAdminError::Db(DbError::Sql(e.to_string())))?
            .map_err(DbAdminError::Db)
    }

    async fn db_locate(&self, name: &str) -> Result<DbLocation, DbAdminError> {
        let (reply, rx) = oneshot::channel();
        let gone = || DbAdminError::Precondition("plugin actor is no longer running".into());
        self.tx
            .send(Msg::DbLocate { name: name.to_string(), reply })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Describe a plugin's database (read on a separate read-only
    /// connection, off the async runtime).
    pub async fn db_info(&self, name: &str) -> Result<DbInfo, DbAdminError> {
        let loc = self.db_locate(name).await?;
        let path = loc.path.clone();
        let inspect = tokio::task::spawn_blocking(move || plugin_db::inspect(&path))
            .await
            .map_err(|e| DbAdminError::Db(DbError::Sql(e.to_string())))??;
        Ok(DbInfo { path: loc.path, limits: loc.limits, inspect })
    }

    /// Read-only query on a plugin's database (admin, separate connection).
    pub async fn db_query(&self, name: &str, sql: &str) -> Result<Rows, DbAdminError> {
        let loc = self.db_locate(name).await?;
        let sql = sql.to_string();
        let rows = tokio::task::spawn_blocking(move || {
            plugin_db::query_file(&loc.path, &sql, loc.limits.max_rows)
        })
        .await
        .map_err(|e| DbAdminError::Db(DbError::Sql(e.to_string())))??;
        Ok(rows)
    }

    /// Delete a plugin's database. Refused while the plugin is loaded (it
    /// holds the file open); the next start recreates it and re-applies the
    /// migrations. `Ok(false)` = there was no file.
    pub async fn db_reset(&self, name: &str) -> Result<bool, DbAdminError> {
        let (reply, rx) = oneshot::channel();
        let gone = || DbAdminError::Precondition("plugin actor is no longer running".into());
        self.tx
            .send(Msg::DbReset { name: name.to_string(), reply })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Apply a lifecycle action to one plugin, returning its new state.
    pub async fn control(&self, name: &str, action: Action) -> Result<PluginInfo, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Control {
                name: name.to_string(),
                action,
                reply,
            })
            .await
            .map_err(|_| "plugin actor is no longer running".to_string())?;
        rx.await
            .map_err(|_| "plugin actor is no longer running".to_string())?
    }
}

/// Spawn the plugin actor without a station control: host calls answer
/// `Unavailable` (tests, tools).
pub fn spawn(decls: Vec<PluginDecl>) -> PluginHandle {
    spawn_env(decls, PluginEnv::default())
}

/// Spawn with a station control and no plugin database directory.
pub fn spawn_with(decls: Vec<PluginDecl>, control: Option<StationControl>) -> PluginHandle {
    spawn_env(decls, PluginEnv { control, db_dir: None, geoip: None })
}

/// Spawn the owning task from the declared plugins and return a handle. Enabled
/// plugins are loaded immediately (a failure is recorded, not fatal); disabled
/// ones stay `Disabled`. Slots are ordered by (`order`, `name`). `env` carries
/// the station control behind every plugin's host surface and the directory
/// of the plugin databases.
pub fn spawn_env(decls: Vec<PluginDecl>, env: PluginEnv) -> PluginHandle { spawn_configured(decls, env, None) }

pub fn spawn_configured(mut decls: Vec<PluginDecl>, env: PluginEnv, config_path: Option<PathBuf>) -> PluginHandle {
    let config_path = config_path.and_then(|p| std::fs::canonicalize(p).ok());
    let metadata_runtime = config_path.as_deref()
        .and_then(|path| metadata_config::read(path).ok().map(|(_, rules)| rules))
        .unwrap_or_default();
    decls.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
    let mut slots: Vec<Slot> = decls
        .into_iter()
        .map(|decl| {
            let mut slot = Slot { schema: Vec::new(), tabs: Vec::new(),
                decl,
                state: PluginState::Disabled,
                plugin: None,
                failures: VecDeque::new(),
            host: None,
            };
            if slot.decl.wasm.is_none() && slot.decl.name == "stop-when-idle" { slot.schema = crate::plugin_config::stop_fields(); }
            if slot.decl.enabled {
                slot.start(&env);
            }
            slot
        })
        .collect();

    let (tx, mut rx) = mpsc::channel::<Msg>(128);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Config { request, read, reply } => { let _ = reply.send(config_operation(&mut slots, &env, config_path.as_deref(), &metadata_runtime, request, read)); }
                Msg::Admin { name, payload, reply } => {
                    let result = slots.iter_mut().find(|s| s.decl.name == name && matches!(s.state, PluginState::Loaded))
                        .and_then(|s|s.plugin.as_mut()).ok_or_else(|| "plugin is not loaded".to_string())
                        .and_then(|p|catch(||p.admin_request(&payload)).and_then(|r|r))
                        .and_then(|value|if value.len() <= 1048576 { Ok(value) } else { Err("administration response too large".into()) });
                    let _ = reply.send(result);
                }
                Msg::Event(event) => dispatch_event(&mut slots, &event),
                Msg::FilterPool { candidates, simulation, reply } => {
                    let _ = reply.send(run_filters_mode(&mut slots, candidates, simulation));
                }
                Msg::Scan { media, reply } => {
                    let _ = reply.send(run_scan(&mut slots, &media));
                }
                Msg::TagHints(reply) => {
                    let hints = slots
                        .iter()
                        .find(|slot| matches!(slot.state, PluginState::Loaded) && slot.decl.name == "custom-tags")
                        .map(|slot| slot.decl.tag_hints())
                        .unwrap_or_default();
                    let _ = reply.send(hints);
                }
                Msg::List(reply) => {
                    let _ = reply.send(slots.iter().map(Slot::info).collect());
                }
                Msg::Control { name, action, reply } => {
                    let res = match slots.iter_mut().find(|s| s.decl.name == name) {
                        None => Err(format!("unknown plugin `{name}`")),
                        Some(slot) => {
                            let will_load = matches!(action, Action::Reload | Action::Restart)
                                || (matches!(action, Action::Start) && !matches!(slot.state, PluginState::Loaded));
                            if will_load && !slot.schema.is_empty() {
                                if let Some(path) = config_path.as_deref() {
                                    let update = crate::plugin_config::read(path, &name).and_then(|(_, config)| { validate_candidate(slot, &env, &config)?; Ok(config) });
                                    match update { Ok(config) => slot.decl.config = config, Err(e) => { let _ = reply.send(Err(e)); continue; } }
                                }
                            }
                            apply_action(slot, action, &env);
                            let info = slot.info();
                            crate::events::record(
                                crate::events::Level::Info,
                                crate::events::Component::Plugin,
                                crate::events::Code::PluginState,
                                [("plugin", info.name.clone()), ("state", info.state.clone())],
                            );
                            Ok(info)
                        }
                    };
                    let _ = reply.send(res);
                }
                Msg::TabLocate { name, tab_id, reply } => {
                    let _ = reply.send(tab_location(&slots, &name, &tab_id, &env));
                }
                Msg::DbLocate { name, reply } => {
                    let _ = reply.send(db_location(&slots, &name, &env).map(|(_, loc)| loc));
                }
                Msg::DbReset { name, reply } => {
                    let res = db_location(&slots, &name, &env).and_then(|(loaded, loc)| {
                        if loaded {
                            return Err(DbAdminError::Precondition(format!(
                                "plugin `{name}` is loaded: stop it first (stationctl plugin stop {name})"
                            )));
                        }
                        let removed = plugin_db::remove(&loc.path)?;
                        tracing::info!(plugin = %name, removed, path = %loc.path.display(), "plugin database reset");
                        Ok(removed)
                    });
                    let _ = reply.send(res);
                }
            }
        }
    });

    PluginHandle { tx, sim: None }
}

fn tab_location(slots: &[Slot], name: &str, tab_id: &str, env: &PluginEnv)
    -> Result<(DbLocation, String), DbAdminError>
{
    let (loaded, loc) = db_location(slots, name, env)?;
    if !loaded { return Err(DbAdminError::Precondition(format!("plugin {name} is not loaded"))); }
    let tab = slots.iter().find(|s| s.decl.name == name).unwrap().tabs.iter()
        .find(|t| t.id == tab_id)
        .ok_or_else(|| DbAdminError::Precondition(format!("unknown tab {tab_id} for plugin {name}")))?;
    if tab.kind == "plugin_config" { return Err(DbAdminError::Precondition("this tab is a configuration editor".into())); }
    Ok((loc, tab.sql.clone()))
}

/// A plugin's database location: (loaded, location).
fn db_location(slots: &[Slot], name: &str, env: &PluginEnv) -> Result<(bool, DbLocation), DbAdminError> {
    let slot = slots
        .iter()
        .find(|s| s.decl.name == name)
        .ok_or_else(|| DbAdminError::UnknownPlugin(name.to_string()))?;
    if !slot.decl.has_db() {
        return Err(DbAdminError::Precondition(format!(
            "plugin `{name}` has no database (capability `db` not declared)"
        )));
    }
    let dir = env.db_dir.as_ref().ok_or_else(|| {
        DbAdminError::Precondition("no plugin database directory is configured".into())
    })?;
    Ok((
        matches!(slot.state, PluginState::Loaded),
        DbLocation {
            path: plugin_db::db_path(dir, name),
            limits: slot.decl.db_limits(),
        },
    ))
}

fn apply_action(slot: &mut Slot, action: Action, env: &PluginEnv) {
    match action {
        Action::Start => {
            // Idempotent: starting an already-loaded plugin is a no-op.
            if !matches!(slot.state, PluginState::Loaded) {
                slot.start(env);
            }
        }
        Action::Stop => slot.stop(),
        // reload == restart in native (no artefact to re-read); a WASM plugin
        // is rebuilt from its file on start, so both re-read it.
        Action::Restart | Action::Reload => {
            slot.stop();
            slot.start(env);
        }
    }
}

fn dispatch_event(slots: &mut [Slot], event: &PluginEvent) {
    for slot in slots.iter_mut() {
        if !matches!(slot.state, PluginState::Loaded) {
            continue;
        }
        if matches!(event, PluginEvent::ListenerSnapshot { .. })
            && !slot.decl.capabilities.contains(&Capability::ListenerDetails)
        {
            continue;
        }
        let outcome = slot.plugin.as_mut().map(|p| catch(|| p.on_event(event)));
        if let Some(Err(reason)) = outcome {
            slot.note_failure(Phase::Event, reason);
        }
    }
}

#[cfg(test)]
#[path = "listener_stats_tests.rs"]
mod listener_stats_tests;

/// Chain the candidate pool through every loaded plugin's `filter_pool`, in
/// slot order (already sorted by `order`, then name). A plugin that panics
/// degrades to pass-through for that stage and is counted as a failure.
#[cfg(test)]
fn run_filters(slots: &mut [Slot], candidates: Vec<Candidate>) -> Vec<Candidate> {
    run_filters_mode(slots, candidates, false).0
}

/// [`run_filters`], optionally for a simulation: each plugin's host is put in
/// simulation mode around its call (mutations refused), a failure is not
/// counted (no quarantine from a preview) but returned as a note, and nothing
/// is logged (a preview runs often).
fn run_filters_mode(
    slots: &mut [Slot],
    candidates: Vec<Candidate>,
    simulation: bool,
) -> (Vec<Candidate>, Vec<(String, String)>) {
    let mut notes = Vec::new();
    let mut cur = candidates;
    for slot in slots.iter_mut() {
        if !matches!(slot.state, PluginState::Loaded) {
            continue;
        }
        if slot.plugin.as_ref().is_some_and(|p| !p.filters_pool()) { continue; }
        let before = cur.len();
        if simulation {
            if let Some(h) = &slot.host {
                h.set_simulating(true);
            }
        }
        let outcome = slot.plugin.as_mut().map(|p| {
            let input = cur.clone();
            catch(move || p.filter_pool(input))
        });
        if simulation {
            if let Some(h) = &slot.host {
                h.set_simulating(false);
            }
        }
        match outcome {
            Some(Ok(kept)) if simulation => cur = kept,
            Some(Err(reason)) if simulation => {
                notes.push((slot.decl.name.clone(), reason));
            }
            Some(Ok(kept)) => {
                if kept.len() != before {
                    tracing::info!(
                        plugin = %slot.decl.name,
                        before,
                        after = kept.len(),
                        "filter_pool changed the pool"
                    );
                }
                cur = kept;
            }
            Some(Err(reason)) => slot.note_failure(Phase::FilterPool, reason),
            None => {}
        }
    }
    (cur, notes)
}

/// Check a plugin's `on_scan` reply against the batch it was given. A reply
/// naming an unknown media or carrying a blank genre is a plugin bug: the
/// whole reply is refused (loud), never partially applied.
fn validate_enrichment(
    known: &std::collections::HashSet<&str>,
    out: &[ScanEnrichment],
) -> Result<(), String> {
    for e in out {
        if !known.contains(e.rel_path.as_str()) {
            return Err(format!("on_scan returned unknown media `{}`", e.rel_path));
        }
        for (key, value) in &e.metadata {
            if key.is_empty() || !key.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
                || value.trim().is_empty()
            {
                return Err(format!("on_scan returned invalid metadata for `{}`", e.rel_path));
            }
            if key == "creation" && crate::media_index::normalize_creation(value).is_err() {
                return Err(format!("on_scan returned invalid creation for `{}`", e.rel_path));
            }
        }
        if e.genres.iter().any(|g| g.trim().is_empty()) {
            return Err(format!("on_scan returned a blank genre for `{}`", e.rel_path));
        }
    }
    Ok(())
}

/// Run every loaded plugin's `on_scan` over the same batch (additive, not
/// chained) and merge the extra genres. A plugin that errs, panics or returns
/// an invalid reply contributes nothing this scan and counts as a failure.
fn run_scan(slots: &mut [Slot], media: &[ScanInput]) -> ScanExtras {
    let known: std::collections::HashSet<&str> =
        media.iter().map(|m| m.rel_path.as_str()).collect();
    let mut extras = ScanExtras::new();
    for slot in slots.iter_mut() {
        if !matches!(slot.state, PluginState::Loaded) {
            continue;
        }
        let Some(p) = slot.plugin.as_mut() else { continue };
        let outcome = catch(|| p.on_scan(media))
            .and_then(|r| r)
            .and_then(|out| validate_enrichment(&known, &out).map(|()| out));
        match outcome {
            Ok(out) => {
                let mut added = 0usize;
                for e in out {
                    let entry = extras.entry(e.rel_path).or_default();
                    // First loaded plugin wins a key, in configured plugin order.
                    for (key, value) in e.metadata {
                        entry.metadata.entry(key).or_insert(value);
                        added += 1;
                    }
                    for g in e.genres {
                        let g = g.trim().to_string();
                        let key = crate::media_index::genre_key(&g);
                        if !entry.genres.iter().any(|x| crate::media_index::genre_key(x) == key) {
                            entry.genres.push(g);
                            added += 1;
                        }
                    }
                }
                if added > 0 {
                    tracing::info!(plugin = %slot.decl.name, added, "on_scan enriched the library");
                }
            }
            Err(reason) => slot.note_failure(Phase::Scan, reason),
        }
    }
    extras.retain(|_, v| !v.genres.is_empty() || !v.metadata.is_empty());
    extras
}

// ---------------------------------------------------------------------------
// The test/demonstration plugin: logs one line per event
// ---------------------------------------------------------------------------

/// Minimal plugin: logs each event it receives. `fail_on_load = true` in its
/// config makes `on_load` fail (to exercise the `Failed` path from `plugin
/// list`).
struct LoggerPlugin {
    fail_on_load: bool,
}

impl LoggerPlugin {
    fn from_config(config: &toml::Table) -> Self {
        let fail_on_load = config
            .get("fail_on_load")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self { fail_on_load }
    }
}

impl Plugin for LoggerPlugin {
    fn name(&self) -> &str {
        "logger"
    }

    fn on_load(&mut self, _host: Host) -> Result<(), String> {
        if self.fail_on_load {
            return Err("fail_on_load = true (test)".to_string());
        }
        tracing::info!("[logger] loaded");
        Ok(())
    }

    fn on_unload(&mut self) {
        tracing::info!("[logger] unloaded");
    }

    fn on_event(&mut self, event: &PluginEvent) {
        match event {
            PluginEvent::TrackResolved {
                media_path,
                playlist_ref,
                rule_id,
                origin,
            } => {
                tracing::info!(
                    %origin,
                    playlist_ref = playlist_ref.as_deref().unwrap_or("-"),
                    rule_id = rule_id.as_deref().unwrap_or("-"),
                    media = media_path.as_deref().unwrap_or("-"),
                    "[logger] track resolved"
                );
            }
            PluginEvent::ListenersSampled { count, at } => {
                tracing::info!(count, at, "[logger] listeners sampled");
            }
            PluginEvent::BroadcastStateChanged { from, to, by } => {
                tracing::info!(%from, %to, %by, "[logger] broadcast state changed");
            }
            PluginEvent::LiveStarted { dj, rule_id, at } => {
                tracing::info!(%dj, %rule_id, at, "[logger] live started");
            }
            PluginEvent::LiveEnded { dj, reason, at } => {
                tracing::info!(%dj, %reason, at, "[logger] live ended");
            }
            // Required for real (downstream/WASM) plugins: PluginEvent is
            // #[non_exhaustive], so new variants must be ignored gracefully.
            // Unreachable here only because this demo plugin lives in-crate.
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}

/// Blacklist plugin: drops candidates whose path starts with an excluded
/// prefix, or whose artist is in the excluded list. Config (both optional):
///   exclude_path_prefixes = ["Podcast/Jingles/end/"]
///   exclude_artists       = ["Some Artist"]
/// Comparison is case-sensitive (media paths keep case).
struct BlacklistPlugin {
    exclude_path_prefixes: Vec<String>,
    exclude_artists: Vec<String>,
}

impl BlacklistPlugin {
    fn from_config(config: &toml::Table) -> Self {
        Self {
            exclude_path_prefixes: string_list(config, "exclude_path_prefixes"),
            exclude_artists: string_list(config, "exclude_artists"),
        }
    }
}

/// Read a config key as a list of strings (missing / wrong type → empty).
fn string_list(config: &toml::Table, key: &str) -> Vec<String> {
    config
        .get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

impl Plugin for BlacklistPlugin {
    fn name(&self) -> &str {
        "blacklist"
    }

    fn on_load(&mut self, _host: Host) -> Result<(), String> {
        tracing::info!(
            paths = self.exclude_path_prefixes.len(),
            artists = self.exclude_artists.len(),
            "[blacklist] loaded"
        );
        Ok(())
    }

    fn filter_pool(&mut self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        candidates
            .into_iter()
            .filter(|c| {
                let path_excluded = self
                    .exclude_path_prefixes
                    .iter()
                    .any(|p| c.rel_path.starts_with(p));
                let artist_excluded = c
                    .artist
                    .as_deref()
                    .map_or(false, |a| self.exclude_artists.iter().any(|x| x == a));
                !(path_excluded || artist_excluded)
            })
            .collect()
    }
}

/// Optional `max_connection_age = "12h"` instead observes ConnectionsSampled:
/// all connections must be at least that old. The core rechecks at the boundary
/// and wakes only on a new connection, or unknown audience/details.
/// Demo of the A2 composition « observe + act »: on `ListenersSampled` with a
/// count of 0 (for `min_zero_samples` consecutive samples, default 1) it arms
/// `host.control(StopWhenIdle)`; with a count > 0 it calls `Wake`. The core
/// owns the mechanism (the drain falls asleep at the next track boundary if
/// the audience is still 0; `wake` only leaves `sleeping`, so a listener
/// never un-pauses nor restarts an operator's stop); this plugin only carries
/// the policy. A non-zero sample resets the streak. It arms once per idle
/// period: an operator's `resume` is not overridden until the audience comes
/// back and leaves again.
///
/// Requires capability `control` — refused at `on_load` otherwise (visible
/// `Failed`), never a policy that silently can't act.
struct StopWhenIdlePlugin {
    host: Option<Host>,
    min_zero_samples: u32,
    zero_streak: u32,
    max_connection_age: Option<u64>,
}

impl StopWhenIdlePlugin {
    fn from_config(config: &toml::Table) -> Result<Self, String> {
        let min = match config.get("min_zero_samples") {
            None => 1,
            Some(v) => match v.as_integer() {
                Some(n) if n >= 1 && n <= u32::MAX as i64 => n as u32,
                _ => return Err("`min_zero_samples` must be an integer ≥ 1".into()),
            },
        };
        let max_connection_age = match config.get("max_connection_age") {
            None => None,
            Some(v) => {
                let age = v.as_str().ok_or("`max_connection_age` must be a duration such as 12h")?;
                if !age.is_ascii() {
                    return Err("`max_connection_age` must be a duration such as 12h".into());
                }
                let seconds = crate::playlist::parse_duration_secs(age)?;
                if seconds == 0 {
                    return Err("`max_connection_age` must be positive".into());
                }
                Some(seconds)
            }
        };
        Ok(Self { host: None, min_zero_samples: min, zero_streak: 0, max_connection_age })
    }
}

impl Plugin for StopWhenIdlePlugin {
    fn config_schema(&mut self) -> Result<Vec<crate::plugin_config::Field>, String> { Ok(crate::plugin_config::stop_fields()) }
    fn validate_config(&mut self, config: &toml::Table, host: &Host) -> Result<(), String> {
        let candidate = Self::from_config(config)?;
        if !host.has(Capability::Control) { return Err("stop-when-idle requires control capability".into()); }
        if candidate.max_connection_age.is_some() && host.connection_sampling_enabled() != Ok(true) { return Err("max_connection_age requires [icecast] listener_snapshots = true".into()); }
        Ok(())
    }
    fn name(&self) -> &str {
        "stop-when-idle"
    }

    fn on_load(&mut self, host: Host) -> Result<(), String> {
        if !host.has(Capability::Control) {
            return Err("stop-when-idle requires `capabilities = [\"control\"]`".into());
        }
        // A persisted sleeping state has no connection baseline after restart.
        // Recover conservatively instead of guessing which clients are new.
        if self.max_connection_age.is_some() {
            if !host.connection_sampling_enabled().map_err(|e| e.to_string())? {
                return Err("max_connection_age requires [icecast] listener_snapshots = true".into());
            }
            host.control(ControlAction::Wake).map_err(|e| e.to_string())?;
        }
        host.set_operator_notice(OperatorNotice::AutoSleep { max_connection_age: self.max_connection_age })
            .map_err(|e| e.to_string())?;
        self.host = Some(host);
        self.zero_streak = 0;
        Ok(())
    }

    fn on_event(&mut self, event: &PluginEvent) {
        if let Some(age) = self.max_connection_age {
            let PluginEvent::ConnectionsSampled { connections, .. } = event else { return };
            if connections.as_ref().is_none_or(|clients| clients.iter().any(|c| c.connected_seconds < age)) {
                self.zero_streak = 0;
                return;
            }
            let Some(host) = &self.host else { return };
            // Read the current host view: queued events must not arm from old data.
            let Ok(Some(clients)) = host.listener_connections() else {
                self.zero_streak = 0;
                return;
            };
            if clients.iter().any(|c| c.connected_seconds < age) {
                self.zero_streak = 0;
                return;
            }
            self.zero_streak = self.zero_streak.saturating_add(1);
            if self.zero_streak == self.min_zero_samples {
                if let Err(e) = host.stop_when_connections_old(age) {
                    tracing::warn!(%e, "[stop-when-idle] could not arm connection-age sleep");
                }
            }
            return;
        }
        let PluginEvent::ListenersSampled { count, .. } = event else {
            return;
        };
        if *count > 0 {
            self.zero_streak = 0;
            let Some(host) = &self.host else { return };
            match host.control(ControlAction::Wake) {
                Ok(Some(_)) => tracing::info!(listeners = count, "[stop-when-idle] listeners back: station woken"),
                Ok(None) => {}
                Err(e) => tracing::warn!(%e, "[stop-when-idle] could not wake the station"),
            }
            return;
        }
        self.zero_streak = self.zero_streak.saturating_add(1);
        if self.zero_streak != self.min_zero_samples {
            return; // not yet, or already armed for this idle period
        }
        let Some(host) = &self.host else { return };
        match host.control(ControlAction::StopWhenIdle) {
            Ok(Some(t)) => tracing::info!(
                from = t.from.as_str(),
                to = t.to.as_str(),
                "[stop-when-idle] no listeners: graceful stop armed"
            ),
            Ok(None) => {}
            Err(e) => tracing::warn!(%e, "[stop-when-idle] could not arm the graceful stop"),
        }
    }
}

// ----- WASM host functions (the host surface, JSON in / JSON out) ----------
//
// Both always return a JSON string — `{"ok":true,…}` or `{"ok":false,"error":…}`
// — so a refused call is data for the guest, never a trap. The `Host` carried
// as user data is the plugin's own (scoped name + declared capabilities).

fn host_reply<T: Serialize>(res: Result<T, String>) -> String {
    let v = match res.map(serde_json::to_value) {
        Ok(Ok(serde_json::Value::Object(mut m))) => {
            m.insert("ok".into(), serde_json::Value::Bool(true));
            serde_json::Value::Object(m)
        }
        Ok(Ok(other)) => serde_json::json!({ "ok": true, "result": other }),
        Ok(Err(e)) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    };
    v.to_string()
}

/// `station_control` input: `{"action": "pause|resume|stop_when_idle|wake"}`.
/// `stop` is refused (unknown action): stopping stationd is the operator's.
fn wasm_station_control(host: &Host, input: &str) -> String {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Req {
        action: ControlAction,
        #[serde(default)]
        max_connection_age: Option<u64>,
    }
    host_reply(
        serde_json::from_str::<Req>(input)
            .map_err(|e| format!("bad station_control input: {e}"))
            .and_then(|r| match (r.action, r.max_connection_age) {
                (ControlAction::StopWhenIdle, Some(age)) => host.stop_when_connections_old(age).map_err(|e| e.to_string()),
                (_, Some(_)) => Err("max_connection_age requires stop_when_idle".into()),
                (action, None) => host.control(action).map_err(|e| e.to_string()),
            })
            .map(|t| match t {
                Some(t) => serde_json::json!({ "changed": true, "from": t.from, "to": t.to }),
                None => serde_json::json!({ "changed": false }),
            }),
    )
}

/// `push_override` input: an `OverrideRequest`, e.g.
/// `{"content": {"media": "news/flash.mp3"}, "mode": "soft", "expiry": "5m"}`.
fn wasm_push_override(host: &Host, input: &str) -> String {
    host_reply(
        serde_json::from_str::<OverrideRequest>(input)
            .map_err(|e| format!("bad push_override input: {e}"))
            .and_then(|r| host.push_override(r).map_err(|e| e.to_string())),
    )
}

fn wasm_operator_notice(host: &Host, input: &str) -> String {
    host_reply(serde_json::from_str::<OperatorNotice>(input)
        .map_err(|e| format!("bad operator_notice input: {e}"))
        .and_then(|notice| host.set_operator_notice(notice).map_err(|e| e.to_string())))
}

host_fn!(operator_notice(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_operator_notice(&host, &input))
});

fn wasm_listener_connections(host: &Host) -> String {
    host_reply((|| {
        let enabled = host.connection_sampling_enabled()?;
        let connections = host.listener_connections()?;
        Ok::<_, HostError>(serde_json::json!({ "enabled": enabled, "connections": connections }))
    })().map_err(|e| e.to_string()))
}

host_fn!(listener_connections(user_data: Host; _input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_listener_connections(&host))
});

host_fn!(station_control(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_station_control(&host, &input))
});

host_fn!(push_override(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_push_override(&host, &input))
});

/// `db_query` input: `{"sql": "SELECT …", "params": [..] | {..}}` →
/// `{"ok":true,"columns":[..],"rows":[[..],..]}`. Read-only statements only.
fn wasm_db_query(host: &Host, input: &str) -> String {
    host_reply(
        serde_json::from_str::<plugin_db::Statement>(input)
            .map_err(|e| format!("bad db_query input: {e}"))
            .and_then(|q| {
                host.db()
                    .and_then(|db| Ok(db.query(&q.sql, &q.params)?))
                    .map_err(|e| e.to_string())
            }),
    )
}

/// `db_exec` input: `{"sql": "INSERT …", "params": [..] | {..}}` (one
/// statement) → `{"ok":true,"changes":n,"last_insert_rowid":id}`.
fn wasm_db_exec(host: &Host, input: &str) -> String {
    host_reply(
        serde_json::from_str::<plugin_db::Statement>(input)
            .map_err(|e| format!("bad db_exec input: {e}"))
            .and_then(|st| {
                host.db()
                    .and_then(|db| Ok(db.exec(&st)?))
                    .map_err(|e| e.to_string())
            }),
    )
}

/// `db_batch` input: `{"statements": [{"sql": …, "params": …}, …]}`, all or
/// nothing → `{"ok":true,"results":[{"changes":…,"last_insert_rowid":…},…]}`.
fn wasm_db_batch(host: &Host, input: &str) -> String {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Req {
        statements: Vec<plugin_db::Statement>,
    }
    host_reply(
        serde_json::from_str::<Req>(input)
            .map_err(|e| format!("bad db_batch input: {e}"))
            .and_then(|r| {
                host.db()
                    .and_then(|db| Ok(db.batch(&r.statements)?))
                    .map_err(|e| e.to_string())
            })
            .map(|results| serde_json::json!({ "results": results })),
    )
}

/// Local MMDB lookup. No network call; IP capture is opt-in. Missing/invalid database
/// at startup yields unavailable; absent/reserved addresses yield not_found.
fn wasm_geoip_lookup(host: &Host, input: &str) -> String {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Req {
        ip: std::net::IpAddr,
    }
    host_reply(host.check(Capability::Geoip).map_err(|e| e.to_string()).and_then(|_| {
        let request = serde_json::from_str::<Req>(input)
            .map_err(|_| "bad geoip_lookup input".to_string())?;
        match &host.geoip {
            Some(geoip) => geoip.lookup(request.ip),
            None => Ok(crate::geoip::Location::unavailable()),
        }
    }))
}

host_fn!(geoip_lookup(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_geoip_lookup(&host, &input))
});

host_fn!(db_query(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_db_query(&host, &input))
});

host_fn!(db_exec(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_db_exec(&host, &input))
});

host_fn!(db_batch(user_data: Host; input: String) -> String {
    let host = user_data.get()?;
    let host = host.lock().map_err(|_| anyhow::anyhow!("host surface poisoned"))?;
    Ok(wasm_db_batch(&host, &input))
});

/// A WASM plugin loaded from a `.wasm` file via extism. Implements the same
/// `Plugin` trait as the built-ins by delegating to the module's exports,
/// serialising data to JSON at the boundary. `extism::Plugin` is `Send`, so
/// this lives on the actor task like any other plugin.
///
/// Exports (all optional): `filter_pool` (missing → pass-through), `on_event`
/// (missing → ignored), `on_scan` (JSON `[ScanInput]` → `[ScanEnrichment]`;
/// missing → nothing to add), `db_migrations` (no input → JSON `["SQL", …]`;
/// missing → no schema). Host functions offered to the guest (A2), bound to
/// this plugin's scoped `Host`: `station_control`, `push_override`,
/// `db_query`, `db_exec`, `db_batch` (imported by the guest from
/// `extern "ExtismHost"`). Config reaches the guest as JSON under the key
/// "config".
struct WasmPlugin {
    name: String,
    plugin: ExtismPlugin,
    has_filter: bool,
    has_event: bool,
    has_scan: bool,
    has_migrations: bool,
}

impl WasmPlugin {
    fn new(name: String, path: &str, config: &toml::Table, host: &Host) -> Result<Self, String> {
        // Pass the plugin's TOML config to the guest as a single JSON string
        // under the key "config"; the guest reads it via `config::get("config")`.
        let config_json = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
        let manifest = Manifest::new([Wasm::file(path)])
            .with_config([("config".to_string(), config_json)].into_iter());
        let functions = [
            Function::new("operator_notice", [PTR], [PTR], UserData::new(host.clone()), operator_notice),
            Function::new("listener_connections", [PTR], [PTR], UserData::new(host.clone()), listener_connections),
            Function::new(
                "station_control",
                [PTR],
                [PTR],
                UserData::new(host.clone()),
                station_control,
            ),
            Function::new(
                "push_override",
                [PTR],
                [PTR],
                UserData::new(host.clone()),
                push_override,
            ),
            Function::new("geoip_lookup", [PTR], [PTR], UserData::new(host.clone()), geoip_lookup),
            Function::new("db_query", [PTR], [PTR], UserData::new(host.clone()), db_query),
            Function::new("db_exec", [PTR], [PTR], UserData::new(host.clone()), db_exec),
            Function::new("db_batch", [PTR], [PTR], UserData::new(host.clone()), db_batch),
        ];
        // No wasmtime disk cache: by default it lives in `$HOME/.cache/wasmtime`,
        // and in the production image stationd runs as `stationd` with root's
        // HOME — every WASM plugin then failed to load. The cache only speeds
        // up start-up compilation (our plugins are small); stationd writes
        // nothing outside its own directories.
        let plugin = PluginBuilder::new(&manifest)
            .with_wasi(false)
            .with_functions(functions)
            .with_cache_disabled()
            .build()
            .map_err(|e| e.to_string())?;
        let has_filter = plugin.function_exists("filter_pool");
        let has_event = plugin.function_exists("on_event");
        let has_scan = plugin.function_exists("on_scan");
        let has_migrations = plugin.function_exists("db_migrations");
        Ok(Self {
            name,
            plugin,
            has_filter,
            has_event,
            has_scan,
            has_migrations,
        })
    }
}

impl Plugin for WasmPlugin {
    fn config_schema(&mut self) -> Result<Vec<crate::plugin_config::Field>, String> {
        if !self.plugin.function_exists("config_schema") { return Ok(Vec::new()); }
        let out = self.plugin.call::<&str, String>("config_schema", "").map_err(|_| "wasm config_schema failed")?;
        if out.len() > 65536 { return Err("config_schema exceeds 64 KiB".into()); }
        serde_json::from_str(&out).map_err(|_| "invalid config_schema JSON".into())
    }
    fn validate_config(&mut self, config: &toml::Table, _host: &Host) -> Result<(), String> {
        if !self.plugin.function_exists("validate_config") { return Ok(()); }
        let input = serde_json::to_string(config).map_err(|_| "cannot encode configuration")?;
        self.plugin.call::<&str, String>("validate_config", &input).map_err(|_| "plugin refused configuration")?;
        Ok(())
    }
    fn ui_tabs(&mut self) -> Result<Vec<crate::plugin_ui::UiTab>, String> {
        if !self.plugin.function_exists("ui_tabs") { return Ok(Vec::new()); }
        let out = self.plugin.call::<&str, String>("ui_tabs", "")
            .map_err(|e| format!("wasm ui_tabs failed: {e}"))?;
        if out.len() > 65536 { return Err("ui_tabs: descriptor exceeds 64 KiB".into()); }
        serde_json::from_str(&out).map_err(|e| format!("bad ui_tabs output: {e}"))
    }
    fn name(&self) -> &str {
        &self.name
    }

    fn on_load(&mut self, _host: Host) -> Result<(), String> {
        if self.plugin.function_exists("on_load") {
            self.plugin.call::<&str, &str>("on_load", "")
                .map_err(|e| format!("wasm on_load failed: {e}"))?;
        }
        Ok(())
    }

    fn on_event(&mut self, event: &PluginEvent) {
        if !self.has_event {
            return;
        }
        let input = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(_) => return,
        };
        // Best-effort: an event is fire-and-forget; a WASM error is logged, not
        // propagated (nothing to return).
        if let Err(e) = self.plugin.call::<&str, &str>("on_event", input.as_str()) {
            tracing::warn!(plugin = %self.name, %e, "wasm on_event failed");
        }
    }

    fn filter_pool(&mut self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        if !self.has_filter {
            return candidates;
        }
        // Degrade to pass-through on any boundary error (serialise, call, parse).
        let input = match serde_json::to_string(&candidates) {
            Ok(s) => s,
            Err(_) => return candidates,
        };
        match self.plugin.call::<&str, String>("filter_pool", input.as_str()) {
            Ok(out) => serde_json::from_str::<Vec<Candidate>>(&out).unwrap_or(candidates),
            Err(e) => {
                tracing::warn!(plugin = %self.name, %e, "wasm filter_pool failed");
                candidates
            }
        }
    }

    fn on_scan(&mut self, media: &[ScanInput]) -> Result<Vec<ScanEnrichment>, String> {
        if !self.has_scan {
            return Ok(Vec::new());
        }
        // Any boundary error is returned (→ counted failure, visible), not
        // swallowed: a scan plugin that silently adds nothing would be
        // indistinguishable from one with nothing to add.
        let input = serde_json::to_string(media).map_err(|e| format!("serialise: {e}"))?;
        let out = self
            .plugin
            .call::<&str, String>("on_scan", input.as_str())
            .map_err(|e| format!("wasm on_scan failed: {e}"))?;
        serde_json::from_str::<Vec<ScanEnrichment>>(&out)
            .map_err(|e| format!("bad on_scan output: {e}"))
    }

    fn db_migrations(&mut self) -> Result<Vec<String>, String> {
        if !self.has_migrations {
            return Ok(Vec::new());
        }
        let out = self
            .plugin
            .call::<&str, String>("db_migrations", "")
            .map_err(|e| format!("wasm db_migrations failed: {e}"))?;
        serde_json::from_str::<Vec<String>>(&out)
            .map_err(|e| format!("bad db_migrations output (expected [\"SQL\", …]): {e}"))
    }
}

#[cfg(test)]
mod tests {
    /// Loading a WASM plugin must not depend on `$HOME`: in the production
    /// image stationd runs as `stationd` with root's HOME (s6-setuidgid keeps
    /// the environment), and wasmtime's default compilation cache
    /// (`$HOME/.cache/wasmtime`) made every WASM plugin fail to load. HOME
    /// points at a regular FILE here, so no cache directory can ever be
    /// created under it — even as root.
    #[test]
    fn wasm_plugin_loads_without_a_usable_home() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("empty.wasm");
        std::fs::write(&wasm, b"\0asm\x01\0\0\0").unwrap(); // empty module
        let not_a_dir = dir.path().join("home-is-a-file");
        std::fs::write(&not_a_dir, b"").unwrap();
        let saved: Vec<_> = ["HOME", "XDG_CACHE_HOME"].iter().map(|k| (*k, std::env::var_os(k))).collect();
        std::env::set_var("HOME", &not_a_dir);
        std::env::set_var("XDG_CACHE_HOME", not_a_dir.join("cache"));
        let host = Host::new("t", &[], None);
        let loaded = WasmPlugin::new("t".into(), wasm.to_str().unwrap(), &toml::Table::new(), &host);
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        assert!(loaded.is_ok(), "{}", loaded.err().unwrap_or_default());
    }

    use super::*;

    fn decl(name: &str, enabled: bool, config: toml::Table) -> PluginDecl {
        PluginDecl {
            name: name.to_string(),
            enabled,
            order: 50,
            wasm: None,
            capabilities: vec![],
            db: None,
            config,
        }
    }

    // ----- host surface (A2) ---------------------------------------------

    #[test]
    fn host_refuses_an_undeclared_capability() {
        let control = StationControl::new_in_memory();
        let host = Host::new("stats", &[], Some(control.clone()));
        assert!(matches!(
            host.control(ControlAction::Pause),
            Err(HostError::Denied { .. })
        ));
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running);
        let req = OverrideRequest {
            content: crate::station_control::OverrideContent::Media("a.mp3".into()),
            mode: Default::default(),
            expiry: None,
            tracks: None,
        };
        assert!(matches!(host.push_override(req), Err(HostError::Denied { .. })));
        assert!(control.list_overrides().is_empty());
    }

    #[test]
    fn host_acts_within_its_capabilities_and_signs_as_the_plugin() {
        let control = StationControl::new_in_memory();
        let host = Host::new(
            "urgence",
            &[Capability::Control, Capability::PushOverride],
            Some(control.clone()),
        );
        host.control(ControlAction::Pause).unwrap();
        assert_eq!(control.state(), crate::station_control::BroadcastState::Paused);
        let req = OverrideRequest {
            content: crate::station_control::OverrideContent::Media("alert.mp3".into()),
            mode: Default::default(),
            expiry: None,
            tracks: None,
        };
        host.push_override(req).unwrap();
        assert_eq!(control.list_overrides()[0].source, "urgence");
    }

    #[test]
    fn host_without_control_is_unavailable() {
        let host = Host::new("x", &[Capability::Control], None);
        assert!(matches!(host.control(ControlAction::Pause), Err(HostError::Unavailable)));
    }

    #[test]
    fn wasm_host_calls_answer_json_in_band() {
        let control = StationControl::new_in_memory();
        let host = Host::new("w", &[Capability::Control], Some(control.clone()));
        let ok: serde_json::Value =
            serde_json::from_str(&wasm_station_control(&host, r#"{"action":"stop_when_idle"}"#))
                .unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["to"], "draining");
        assert_eq!(control.state(), crate::station_control::BroadcastState::Draining);
        // Bad input and missing capability are data, not traps.
        let bad: serde_json::Value =
            serde_json::from_str(&wasm_station_control(&host, r#"{"action":"explode"}"#)).unwrap();
        assert_eq!(bad["ok"], false);
        // A plugin can never stop stationd.
        let stop: serde_json::Value =
            serde_json::from_str(&wasm_station_control(&host, r#"{"action":"stop"}"#)).unwrap();
        assert_eq!(stop["ok"], false);
        assert_eq!(control.state(), crate::station_control::BroadcastState::Draining);
        let denied: serde_json::Value = serde_json::from_str(&wasm_push_override(
            &host,
            r#"{"content":{"media":"a.mp3"}}"#,
        ))
        .unwrap();
        assert_eq!(denied["ok"], false);
        assert!(denied["error"].as_str().unwrap().contains("push_override"));
    }

    #[test]
    fn wasm_push_override_queues_with_the_plugin_as_source() {
        let control = StationControl::new_in_memory();
        let host = Host::new("w", &[Capability::PushOverride], Some(control.clone()));
        let out: serde_json::Value = serde_json::from_str(&wasm_push_override(
            &host,
            r#"{"content":{"playlist":"Shows/Flash"},"mode":"hard","expiry":"5m"}"#,
        ))
        .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["degraded"], true, "hard without LS is degraded");
        let e = &control.list_overrides()[0];
        assert_eq!(e.source, "w");
        assert_eq!(
            e.content,
            crate::station_control::OverrideContent::Playlist("shows/flash".into())
        );
    }

    #[tokio::test]
    async fn stop_when_idle_requires_the_control_capability() {
        let h = spawn_with(
            vec![decl("stop-when-idle", true, toml::Table::new())],
            Some(StationControl::new_in_memory()),
        );
        let info = &h.list().await[0];
        assert_eq!(info.state, "failed");
        assert!(info.reason.contains("control"));
    }

    #[tokio::test]
    async fn stop_when_idle_arms_the_drain_on_zero_listeners() {
        let control = StationControl::new_in_memory();
        let mut d = decl("stop-when-idle", true, toml::Table::new());
        d.capabilities = vec![Capability::Control];
        let h = spawn_with(vec![d], Some(control.clone()));
        control.attach_plugins(h.clone());
        assert_eq!(h.list().await[0].state, "loaded");
        assert_eq!(h.list().await[0].capabilities, vec!["control".to_string()]);

        control.sample_listeners(4);
        h.list().await; // the actor handles messages in order: events processed
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running);

        control.sample_listeners(0);
        h.list().await;
        assert_eq!(control.state(), crate::station_control::BroadcastState::Draining);
        // The core completes it at the next track boundary (audience still 0).
        assert_eq!(
            control.gate(),
            crate::station_control::Gate::Halt(crate::station_control::BroadcastState::Sleeping)
        );
        // A listener comes back: woken.
        control.sample_listeners(1);
        h.list().await;
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running);
    }

    /// Exercise the real guest against the same host/state machine as the native policy.
    /// Run after compiling plugins/stop-when-idle-wasm for wasm32-unknown-unknown.
    #[test]
    #[ignore = "requires compiled stop-when-idle WASM"]
    fn wasm_connection_age_matches_native_policy() {
        use crate::listener_snapshot::Connection;
        use crate::station_control::{BroadcastState, Gate};
        let wasm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/stop-when-idle-wasm/target/wasm32-unknown-unknown/release/stop_when_idle_wasm.wasm");
        assert!(wasm.is_file(), "build stop-when-idle-wasm first");
        for use_wasm in [false, true] {
            let control = StationControl::new_in_memory();
            control.configure_connection_sampling(true);
            let mut cfg = toml::Table::new();
            cfg.insert("max_connection_age".into(), toml::Value::String("1h".into()));
            cfg.insert("min_zero_samples".into(), toml::Value::Integer(2));
            let host = Host::new("p", &[Capability::Control], Some(control.clone()));
            let mut p: Box<dyn Plugin> = if use_wasm {
                Box::new(WasmPlugin::new("p".into(), wasm.to_str().unwrap(), &cfg, &host).unwrap())
            } else {
                Box::new(StopWhenIdlePlugin::from_config(&cfg).unwrap())
            };
            control.sleep_now();
            p.on_load(host.clone()).unwrap();
            assert_eq!(host.operator_notice(), Some(OperatorNotice::AutoSleep { max_connection_age: Some(3600) }));
            assert_eq!(control.state(), BroadcastState::Running, "reload recovers without a baseline");
            control.sample_listeners(2);
            let old = vec![
                Connection { mount: "/a".into(), id: "1".into(), connected_seconds: 3600 },
                Connection { mount: "/b".into(), id: "1".into(), connected_seconds: 4000 },
            ];
            let sample = |p: &mut dyn Plugin, clients: Option<Vec<Connection>>| {
                control.sample_connections(clients.clone(), Duration::from_secs(30));
                p.on_event(&PluginEvent::ConnectionsSampled { at: 0, connections: clients });
            };
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.state(), BroadcastState::Running);
            // Unknown queued data must reset the streak even if the host is already fresh.
            p.on_event(&PluginEvent::ConnectionsSampled { at: 0, connections: None });
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.state(), BroadcastState::Running);
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.gate(), Gate::Halt(BroadcastState::Sleeping));
            p.on_event(&PluginEvent::ListenersSampled { count: 2, at: 0 });
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.state(), BroadcastState::Sleeping, "old clients do not wake");
            control.apply(ControlAction::Resume, "cli").unwrap();
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.state(), BroadcastState::Running, "operator resume is respected");
            let young = vec![Connection { mount: "/a".into(), id: "2".into(), connected_seconds: 0 }];
            sample(p.as_mut(), Some(young.clone()));
            sample(p.as_mut(), Some(old.clone()));
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.gate(), Gate::Halt(BroadcastState::Sleeping));
            sample(p.as_mut(), Some(young));
            assert_eq!(control.state(), BroadcastState::Running, "reopening the stream wakes");
            sample(p.as_mut(), Some(old.clone()));
            sample(p.as_mut(), Some(old.clone()));
            assert_eq!(control.gate(), Gate::Halt(BroadcastState::Sleeping));
            for _ in 0..2 {
                sample(p.as_mut(), None);
                assert_eq!(control.state(), BroadcastState::Sleeping, "transient missing mount does not wake");
            }
            sample(p.as_mut(), None);
            assert_eq!(control.state(), BroadcastState::Running, "persistently missing mount wakes");
            control.apply(ControlAction::Pause, "cli").unwrap();
            sample(p.as_mut(), Some(vec![Connection { mount: "/a".into(), id: "3".into(), connected_seconds: 0 }]));
            assert_eq!(control.state(), BroadcastState::Paused);
        }
        // Guest configuration failures are propagated through the optional on_load hook.
        let host = Host::new("p", &[Capability::Control], Some(StationControl::new_in_memory()));
        for age in [toml::Value::Integer(3600), toml::Value::String("0h".into()),
            toml::Value::String("é".into()), toml::Value::String("18446744073709551615d".into())] {
            let mut cfg = toml::Table::new();
            cfg.insert("max_connection_age".into(), age);
            let mut p = WasmPlugin::new("p".into(), wasm.to_str().unwrap(), &cfg, &host).unwrap();
            assert!(p.on_load(host.clone()).is_err());
        }
        // A renamed guest makes its own prerequisite check, independently of config.rs.
        let c = StationControl::new_in_memory();
        c.sleep_now();
        let host = Host::new("custom-alias", &[Capability::Control], Some(c.clone()));
        let mut cfg = toml::Table::new();
        cfg.insert("max_connection_age".into(), toml::Value::String("1h".into()));
        let mut p = WasmPlugin::new("custom-alias".into(), wasm.to_str().unwrap(), &cfg, &host).unwrap();
        assert!(p.on_load(host.clone()).unwrap_err().contains("listener_snapshots"));
        assert_eq!(c.state(), BroadcastState::Sleeping);
        c.configure_connection_sampling(true);
        p.on_load(host).unwrap();
        assert_eq!(c.state(), BroadcastState::Running);
        // With no age option, the guest retains the zero-count policy.
        let c = StationControl::new_in_memory();
        let host = Host::new("p", &[Capability::Control], Some(c.clone()));
        let mut p = WasmPlugin::new("p".into(), wasm.to_str().unwrap(), &toml::Table::new(), &host).unwrap();
        p.on_load(host).unwrap();
        c.sample_listeners(0);
        p.on_event(&PluginEvent::ListenersSampled { count: 0, at: 0 });
        assert_eq!(c.gate(), Gate::Halt(BroadcastState::Sleeping));
        c.sample_listeners(1);
        p.on_event(&PluginEvent::ListenersSampled { count: 1, at: 0 });
        assert_eq!(c.state(), BroadcastState::Running);
    }

    #[test]
    fn connection_age_policy_ignores_counts_and_preserves_operator_resume() {
        use crate::listener_snapshot::Connection;
        use crate::station_control::{BroadcastState, Gate};
        let control = StationControl::new_in_memory();
        control.configure_connection_sampling(true);
        let mut cfg = toml::Table::new();
        cfg.insert("max_connection_age".into(), toml::Value::String("1h".into()));
        cfg.insert("min_zero_samples".into(), toml::Value::Integer(2));
        let mut p = StopWhenIdlePlugin::from_config(&cfg).unwrap();
        let host = Host::new("stop-when-idle", &[Capability::Control], Some(control.clone()));
        p.on_load(host.clone()).unwrap();
        control.sample_listeners(1);
        let event = PluginEvent::ConnectionsSampled { at: 0, connections: Some(vec![Connection {
            mount: "/radio".into(), id: "1".into(), connected_seconds: 3600,
        }]) };
        let sample = |age| control.sample_connections(Some(vec![Connection {
            mount: "/radio".into(), id: "1".into(), connected_seconds: age,
        }]), Duration::from_secs(30));
        sample(3600);
        p.on_event(&event);
        assert_eq!(control.state(), BroadcastState::Running);
        control.sample_connections(None, Duration::from_secs(30));
        p.on_event(&PluginEvent::ConnectionsSampled { at: 0, connections: None });
        sample(3600);
        p.on_event(&event);
        assert_eq!(control.state(), BroadcastState::Running, "unknown reset the streak");
        p.on_event(&event);
        assert_eq!(control.gate(), Gate::Halt(BroadcastState::Sleeping));
        p.on_event(&PluginEvent::ListenersSampled { count: 1, at: 0 });
        assert_eq!(control.state(), BroadcastState::Sleeping, "old client count must not wake");
        let json: serde_json::Value = serde_json::from_str(&wasm_listener_connections(&host)).unwrap();
        assert!(json["ok"].as_bool().unwrap());
        let client = json["connections"][0].as_object().unwrap();
        assert_eq!(client.len(), 3);
        assert!(!client.contains_key("ip"));
        assert!(!client.contains_key("user_agent"));
        control.apply(ControlAction::Resume, "cli").unwrap();
        p.on_event(&event);
        assert_eq!(control.state(), BroadcastState::Running, "operator resume is not overridden");
        sample(0);
        p.on_event(&event);
        sample(3600);
        p.on_event(&event);
        p.on_event(&event);
        assert_eq!(control.state(), BroadcastState::Draining);
    }

    #[test]
    fn connection_age_config_and_wasm_control_are_validated() {
        for value in [toml::Value::Integer(3600), toml::Value::String("0h".into()),
            toml::Value::String("bad".into()), toml::Value::String("18446744073709551615d".into())] {
            let mut cfg = toml::Table::new();
            cfg.insert("max_connection_age".into(), value);
            assert!(StopWhenIdlePlugin::from_config(&cfg).is_err());
        }
        let control = StationControl::new_in_memory();
        let host = Host::new("p", &[Capability::Control], Some(control.clone()));
        for input in [r#"{"action":"wake","max_connection_age":100}"#,
            r#"{"action":"stop_when_idle","max_connection_age":0}"#] {
            let reply: serde_json::Value = serde_json::from_str(&wasm_station_control(&host, input)).unwrap();
            assert_eq!(reply["ok"], false);
        }
        let reply: serde_json::Value = serde_json::from_str(&wasm_station_control(&host,
            r#"{"action":"stop_when_idle","max_connection_age":3600}"#)).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(control.state(), crate::station_control::BroadcastState::Draining);
        assert_eq!(control.gate(), crate::station_control::Gate::Play, "missing details");
        let denied = Host::new("p", &[], Some(control));
        assert!(denied.stop_when_connections_old(3600).is_err());
    }

    #[tokio::test]
    async fn native_auto_sleep_notice_is_visible_before_any_eligible_sample() {
        let control = StationControl::new_in_memory();
        control.configure_connection_sampling(true);
        let mut cfg = toml::Table::new();
        cfg.insert("max_connection_age".into(), toml::Value::String("10m".into()));
        let mut d = decl("stop-when-idle", true, cfg);
        d.capabilities = vec![Capability::Control];
        let h = spawn_env(vec![d], PluginEnv { control: Some(control.clone()), ..Default::default() });
        let info = h.list().await;
        assert_eq!(info[0].state, "loaded");
        assert_eq!(info[0].operator_notice, Some(OperatorNotice::AutoSleep { max_connection_age: Some(600) }));
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running,
            "an active policy is not an armed drain");
        h.control("stop-when-idle", Action::Stop).await.unwrap();
        assert!(h.list().await[0].operator_notice.is_none());
        h.control("stop-when-idle", Action::Start).await.unwrap();
        assert!(h.list().await[0].operator_notice.is_some());
        let (journal, _) = crate::events::subscribe(crate::events::CAPACITY);
        assert!(journal.iter().any(|e| e.code == crate::events::Code::PluginModeEnabled
            && e.param("plugin") == Some("stop-when-idle") && e.param("max_connection_age") == Some("600")));
    }

    #[tokio::test]
    #[ignore = "requires compiled stop-when-idle WASM"]
    async fn wasm_auto_sleep_notice_follows_the_loaded_slot() {
        let control = StationControl::new_in_memory();
        control.configure_connection_sampling(true);
        let wasm = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/stop-when-idle-wasm/target/wasm32-unknown-unknown/release/stop_when_idle_wasm.wasm");
        let mut cfg = toml::Table::new();
        cfg.insert("max_connection_age".into(), toml::Value::String("10m".into()));
        let mut d = decl("renamed-sleep-policy", true, cfg.clone());
        d.wasm = Some(wasm.to_str().unwrap().into());
        d.capabilities = vec![Capability::Control];
        let h = spawn_env(vec![d.clone()], PluginEnv { control: Some(control.clone()), ..Default::default() });
        let info = h.list().await;
        assert_eq!(info[0].state, "loaded", "{}", info[0].reason);
        assert_eq!(info[0].operator_notice, Some(OperatorNotice::AutoSleep { max_connection_age: Some(600) }));
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running);
        h.control("renamed-sleep-policy", Action::Stop).await.unwrap();
        assert!(h.list().await[0].operator_notice.is_none());
        h.control("renamed-sleep-policy", Action::Reload).await.unwrap();
        assert!(h.list().await[0].operator_notice.is_some());
        let no_sampler = StationControl::new_in_memory();
        let failed = spawn_env(vec![d], PluginEnv { control: Some(no_sampler), ..Default::default() });
        let info = failed.list().await;
        assert_eq!(info[0].state, "failed");
        assert!(info[0].operator_notice.is_none());
    }

    #[test]
    fn connection_age_requires_sampler_in_plugin_on_load() {
        let control = StationControl::new_in_memory();
        control.sleep_now();
        let host = Host::new("custom-alias", &[Capability::Control], Some(control.clone()));
        let mut cfg = toml::Table::new();
        cfg.insert("max_connection_age".into(), toml::Value::String("12h".into()));
        let mut p = StopWhenIdlePlugin::from_config(&cfg).unwrap();
        assert!(p.on_load(host.clone()).unwrap_err().contains("listener_snapshots"));
        assert_eq!(control.state(), crate::station_control::BroadcastState::Sleeping);
        control.configure_connection_sampling(true);
        // Enabled sampling with no sample yet is distinct from disabled sampling.
        assert_eq!(host.listener_connections().unwrap(), None);
        assert!(p.on_load(host).is_ok());
    }

    #[test]
    fn loading_connection_age_policy_wakes_without_a_persisted_baseline() {
        let control = StationControl::new_in_memory();
        control.configure_connection_sampling(true);
        control.sleep_now();
        let mut cfg = toml::Table::new();
        cfg.insert("max_connection_age".into(), toml::Value::String("12h".into()));
        let mut p = StopWhenIdlePlugin::from_config(&cfg).unwrap();
        p.on_load(Host::new("p", &[Capability::Control], Some(control.clone()))).unwrap();
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running);
    }

    #[test]
    fn stop_when_idle_never_wakes_a_paused_station() {
        let control = StationControl::new_in_memory();
        let mut p = StopWhenIdlePlugin::from_config(&toml::Table::new()).unwrap();
        p.on_load(Host::new("stop-when-idle", &[Capability::Control], Some(control.clone())))
            .unwrap();
        control.apply(ControlAction::Pause, "cli").unwrap();
        p.on_event(&PluginEvent::ListenersSampled { count: 3, at: 0 });
        assert_eq!(control.state(), crate::station_control::BroadcastState::Paused);
    }

    #[test]
    fn stop_when_idle_honours_min_zero_samples() {
        let control = StationControl::new_in_memory();
        let mut cfg = toml::Table::new();
        cfg.insert("min_zero_samples".into(), toml::Value::Integer(2));
        let mut p = StopWhenIdlePlugin::from_config(&cfg).unwrap();
        p.on_load(Host::new("stop-when-idle", &[Capability::Control], Some(control.clone())))
            .unwrap();
        let zero = PluginEvent::ListenersSampled { count: 0, at: 0 };
        p.on_event(&zero);
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running);
        p.on_event(&zero);
        assert_eq!(control.state(), crate::station_control::BroadcastState::Draining);
        // Bad config is a load-time refusal.
        let mut bad = toml::Table::new();
        bad.insert("min_zero_samples".into(), toml::Value::Integer(0));
        assert!(StopWhenIdlePlugin::from_config(&bad).is_err());
    }

    #[tokio::test]
    async fn enabled_logger_loads() {
        let h = spawn(vec![decl("logger", true, toml::Table::new())]);
        let list = h.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, "loaded");
    }

    #[test]
    fn declared_plugin_is_not_enabled_by_default() {
        // Declaring arms, it does not activate: `enabled` must be explicit.
        let d: PluginDecl = toml::from_str(r#"name = "logger""#).unwrap();
        assert!(!d.enabled);
        let d: PluginDecl = toml::from_str("name = \"logger\"\nenabled = true").unwrap();
        assert!(d.enabled);
    }

    #[tokio::test]
    async fn disabled_plugin_stays_disabled() {
        let h = spawn(vec![decl("logger", false, toml::Table::new())]);
        assert_eq!(h.list().await[0].state, "disabled");
    }

    #[tokio::test]
    async fn fail_on_load_is_visible_as_failed() {
        let mut cfg = toml::Table::new();
        cfg.insert("fail_on_load".into(), toml::Value::Boolean(true));
        let h = spawn(vec![decl("logger", true, cfg)]);
        let info = &h.list().await[0];
        assert_eq!(info.state, "failed");
        assert!(info.reason.contains("fail_on_load"));
    }

    #[tokio::test]
    async fn unknown_kind_is_a_visible_failure() {
        let h = spawn(vec![decl("nope", true, toml::Table::new())]);
        let info = &h.list().await[0];
        assert_eq!(info.state, "failed");
        assert!(info.reason.contains("unknown plugin kind"));
    }

    #[tokio::test]
    async fn stop_then_start_roundtrips() {
        let h = spawn(vec![decl("logger", true, toml::Table::new())]);
        assert_eq!(h.control("logger", Action::Stop).await.unwrap().state, "disabled");
        assert_eq!(h.control("logger", Action::Start).await.unwrap().state, "loaded");
    }

    #[tokio::test]
    async fn control_unknown_plugin_errors() {
        let h = spawn(vec![]);
        assert!(h.control("ghost", Action::Start).await.is_err());
    }

    // ----- failure window / quarantine (direct on a Slot, no actor) -----

    struct PanicPlugin;
    impl Plugin for PanicPlugin {
        fn name(&self) -> &str {
            "panic"
        }
        fn on_event(&mut self, _e: &PluginEvent) {
            panic!("boom");
        }
    }

    fn loaded_panic_slot() -> Slot {
        Slot { schema: Vec::new(), tabs: Vec::new(),
            decl: decl("panic", true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(Box::new(PanicPlugin)),
            failures: VecDeque::new(),
            host: None,
        }
    }

    #[test]
    fn repeated_event_panics_quarantine_the_plugin() {
        let mut slots = vec![loaded_panic_slot()];
        let ev = PluginEvent::TrackResolved {
            media_path: None,
            playlist_ref: None,
            rule_id: None,
            origin: "test".into(),
        };
        // MAX_FAILURES = 3 within the window.
        dispatch_event(&mut slots, &ev);
        assert!(matches!(slots[0].state, PluginState::Loaded));
        dispatch_event(&mut slots, &ev);
        dispatch_event(&mut slots, &ev);
        assert!(matches!(slots[0].state, PluginState::Quarantined { .. }));
        // Quarantined → no longer dispatched (no plugin instance held).
        assert!(slots[0].plugin.is_none());
    }

    // ----- filter_pool chaining (direct on a Slot, no actor) -----------

    struct KeepContaining(&'static str);
    impl Plugin for KeepContaining {
        fn name(&self) -> &str {
            "keep"
        }
        fn filter_pool(&mut self, candidates: Vec<Candidate>) -> Vec<Candidate> {
            candidates
                .into_iter()
                .filter(|c| c.rel_path.contains(self.0))
                .collect()
        }
    }

    struct PanicFilter;
    impl Plugin for PanicFilter {
        fn name(&self) -> &str {
            "pf"
        }
        fn filter_pool(&mut self, _candidates: Vec<Candidate>) -> Vec<Candidate> {
            panic!("boom");
        }
    }

    fn cand(rel: &str) -> Candidate {
        Candidate {
            rel_path: rel.into(),
            artist: None,
            title: None,
            album: None,
            year: None,
            duration_ms: 1,
            genres: vec![],
            mtime_ns: 0,
        }
    }

    fn loaded_slot(plugin: Box<dyn Plugin>) -> Slot {
        Slot { schema: Vec::new(), tabs: Vec::new(),
            decl: decl("x", true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(plugin),
            failures: VecDeque::new(),
            host: None,
        }
    }

    #[test]
    fn filter_pool_removes_non_matching() {
        let mut slots = vec![loaded_slot(Box::new(KeepContaining("keep")))];
        let out = run_filters(&mut slots, vec![cand("a/keep.mp3"), cand("b/drop.mp3")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rel_path, "a/keep.mp3");
    }

    #[test]
    fn panicking_filter_degrades_to_passthrough_and_counts_failure() {
        let mut slots = vec![loaded_slot(Box::new(PanicFilter))];
        let out = run_filters(&mut slots, vec![cand("x.mp3")]);
        assert_eq!(out.len(), 1, "pool passes through unchanged on panic");
        assert_eq!(slots[0].failures.len(), 1);
    }

    #[test]
    fn blacklist_drops_by_path_prefix_and_artist() {
        let mut cfg = toml::Table::new();
        cfg.insert(
            "exclude_path_prefixes".into(),
            toml::Value::Array(vec![toml::Value::String("ads/".into())]),
        );
        cfg.insert(
            "exclude_artists".into(),
            toml::Value::Array(vec![toml::Value::String("Nope".into())]),
        );
        let mut p = BlacklistPlugin::from_config(&cfg);

        let keep = cand("music/a.mp3");
        let drop_path = cand("ads/b.mp3");
        let mut drop_artist = cand("music/c.mp3");
        drop_artist.artist = Some("Nope".into());

        let out = p.filter_pool(vec![keep, drop_path, drop_artist]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rel_path, "music/a.mp3");
    }

    // ----- on_scan (direct on a Slot, no actor) --------------------------

    fn scan_input(rel: &str, tags: &[(&str, &str)]) -> ScanInput {
        ScanInput {
            rel_path: rel.into(),
            artist: None,
            title: None,
            album: None,
            year: None,
            duration_ms: 1000,
            genres: vec![],
            custom_tags: tags
                .iter()
                .map(|(n, v)| crate::media::CustomTag { name: (*n).into(), value: (*v).into() })
                .collect(),
        }
    }

    /// Test double: every `TXXX`-like tag named `tag` → a genre.
    struct TagToGenre(&'static str);
    impl Plugin for TagToGenre {
        fn name(&self) -> &str {
            "tag-to-genre"
        }
        fn on_scan(&mut self, media: &[ScanInput]) -> Result<Vec<ScanEnrichment>, String> {
            Ok(media
                .iter()
                .map(|m| ScanEnrichment { metadata: Default::default(),
                    rel_path: m.rel_path.clone(),
                    genres: m
                        .custom_tags
                        .iter()
                        .filter(|t| t.name == self.0)
                        .map(|t| t.value.clone())
                        .collect(),
                })
                .collect())
        }
    }

    /// Test double returning a fixed (possibly invalid) reply, or an error.
    struct FixedScan(Result<Vec<ScanEnrichment>, String>);
    impl Plugin for FixedScan {
        fn name(&self) -> &str {
            "fixed"
        }
        fn on_scan(&mut self, _m: &[ScanInput]) -> Result<Vec<ScanEnrichment>, String> {
            self.0.clone()
        }
    }

    fn loaded(name: &str, plugin: Box<dyn Plugin>) -> Slot {
        Slot { schema: Vec::new(), tabs: Vec::new(),
            decl: decl(name, true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(plugin),
            failures: VecDeque::new(),
            host: None,
        }
    }

    #[test]
    fn on_scan_merges_plugins_and_dedups_case_insensitively() {
        let mut slots = vec![
            loaded("a", Box::new(TagToGenre("Type"))),
            loaded(
                "b",
                Box::new(FixedScan(Ok(vec![ScanEnrichment { metadata: Default::default(),
                    rel_path: "x.mp3".into(),
                    genres: vec!["TALKS".into(), "news".into()],
                }]))),
            ),
            loaded("logger", Box::new(LoggerPlugin { fail_on_load: false })), // default: nothing
        ];
        let media = vec![
            scan_input("x.mp3", &[("Type", "talks")]),
            scan_input("y.mp3", &[("Mood", "calm")]),
        ];
        let extras = run_scan(&mut slots, &media);
        assert_eq!(extras.len(), 1, "media with nothing to add are absent");
        assert_eq!(extras["x.mp3"].genres, vec!["talks".to_string(), "news".to_string()]);
        assert!(slots.iter().all(|s| matches!(s.state, PluginState::Loaded)));
    }

    #[test]
    fn on_scan_error_or_invalid_reply_counts_as_failure_and_adds_nothing() {
        let bad_path = ScanEnrichment { metadata: Default::default(), rel_path: "ghost.mp3".into(), genres: vec!["x".into()] };
        let blank = ScanEnrichment { metadata: Default::default(), rel_path: "x.mp3".into(), genres: vec!["  ".into()] };
        let mut slots = vec![
            loaded("err", Box::new(FixedScan(Err("boom".into())))),
            loaded("ghost", Box::new(FixedScan(Ok(vec![bad_path])))),
            loaded("blank", Box::new(FixedScan(Ok(vec![blank])))),
        ];
        let media = vec![scan_input("x.mp3", &[])];
        assert!(run_scan(&mut slots, &media).is_empty());
        for s in &slots {
            assert_eq!(s.failures.len(), 1, "{} must record a failure", s.decl.name);
        }
    }

    #[tokio::test]
    async fn on_scan_through_the_actor_with_default_plugins_adds_nothing() {
        let h = spawn(vec![decl("logger", true, toml::Table::new())]);
        let extras = h.on_scan(vec![scan_input("x.mp3", &[("Type", "talks")])]).await;
        assert!(extras.is_empty());
        assert_eq!(h.list().await[0].state, "loaded");
    }

    // ----- per-plugin database (capability `db`) ------------------------

    /// Counts `TrackResolved` per media in its own database.
    struct CounterPlugin {
        migrations: Vec<String>,
        host: Option<Host>,
    }

    impl CounterPlugin {
        fn new(migrations: &[&str]) -> Self {
            Self { migrations: migrations.iter().map(|m| m.to_string()).collect(), host: None }
        }
    }

    impl Plugin for CounterPlugin {
        fn name(&self) -> &str {
            "counter"
        }
        fn on_load(&mut self, host: Host) -> Result<(), String> {
            host.db().map_err(|e| e.to_string())?;
            self.host = Some(host);
            Ok(())
        }
        fn db_migrations(&mut self) -> Result<Vec<String>, String> {
            Ok(self.migrations.clone())
        }
        fn on_event(&mut self, event: &PluginEvent) {
            let PluginEvent::TrackResolved { media_path: Some(m), .. } = event else { return };
            let db = self.host.as_ref().unwrap().db().unwrap();
            db.exec(&plugin_db::Statement {
                sql: "INSERT INTO play (media, n) VALUES (?1, 1) ON CONFLICT(media) DO UPDATE SET n = n + 1".into(),
                params: plugin_db::Params::Positional(vec![serde_json::json!(m)]),
            })
            .unwrap();
        }
    }

    const PLAY_SCHEMA: &str = "CREATE TABLE play (media TEXT PRIMARY KEY, n INTEGER NOT NULL)";

    fn db_slot(name: &str) -> Slot {
        let mut d = decl(name, true, toml::Table::new());
        d.capabilities = vec![Capability::Db];
        Slot { schema: Vec::new(), tabs: Vec::new(), decl: d, state: PluginState::Disabled, plugin: None, failures: VecDeque::new(), host: None }
    }

    fn db_env(dir: &std::path::Path) -> PluginEnv {
        PluginEnv { control: None, db_dir: Some(dir.to_path_buf()), geoip: None }
    }

    fn resolved(media: &str) -> PluginEvent {
        PluginEvent::TrackResolved {
            media_path: Some(media.into()),
            playlist_ref: None,
            rule_id: None,
            origin: "base".into(),
        }
    }

    #[test]
    fn db_plugin_is_migrated_before_on_load_and_writes_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = db_slot("counter");
        let host = slot.open_host(&db_env(dir.path())).expect("db opened");
        slot.migrate_and_load(Box::new(CounterPlugin::new(&[PLAY_SCHEMA])), host);
        assert_eq!(slot.state.label(), "loaded", "{}", slot.state.reason());
        let mut slots = vec![slot];
        dispatch_event(&mut slots, &resolved("a.mp3"));
        dispatch_event(&mut slots, &resolved("a.mp3"));
        let path = plugin_db::db_path(dir.path(), "counter");
        let rows = plugin_db::query_file(&path, "SELECT media, n FROM play", 10).unwrap();
        assert_eq!(rows.rows, vec![vec![serde_json::json!("a.mp3"), serde_json::json!(2)]]);
    }

    #[test]
    fn a_failing_migration_refuses_the_start_visibly() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = db_slot("counter");
        let host = slot.open_host(&db_env(dir.path())).unwrap();
        slot.migrate_and_load(Box::new(CounterPlugin::new(&["CREATE TABLE oops ("])), host);
        assert_eq!(slot.state.label(), "failed");
        assert!(slot.state.reason().starts_with("Migrate: migration 1"), "{}", slot.state.reason());
        assert!(slot.plugin.is_none());
        // An edited, already-applied migration is refused too.
        let mut slot = db_slot("counter");
        let host = slot.open_host(&db_env(dir.path())).unwrap();
        slot.migrate_and_load(Box::new(CounterPlugin::new(&[PLAY_SCHEMA])), host);
        assert_eq!(slot.state.label(), "loaded");
        slot.stop();
        let host = slot.open_host(&db_env(dir.path())).unwrap();
        slot.migrate_and_load(Box::new(CounterPlugin::new(&["CREATE TABLE play (media TEXT)"])), host);
        assert_eq!(slot.state.label(), "failed");
        assert!(slot.state.reason().contains("modified"), "{}", slot.state.reason());
    }

    #[test]
    fn db_capability_without_a_directory_fails_visibly() {
        let mut slot = db_slot("counter");
        assert!(slot.open_host(&PluginEnv::default()).is_none());
        assert_eq!(slot.state.label(), "failed");
        assert!(slot.state.reason().contains("directory"), "{}", slot.state.reason());
    }

    #[test]
    fn host_db_needs_the_capability() {
        let host = Host::new("x", &[], None);
        assert!(matches!(host.db(), Err(HostError::Denied { capability: "db", .. })));
        let host = Host::new("x", &[Capability::Db], None);
        assert!(matches!(host.db(), Err(HostError::NoDatabase)));
    }

    #[test]
    fn wasm_db_calls_answer_json_in_band() {
        let dir = tempfile::tempdir().unwrap();
        let db = PluginDb::open(dir.path(), "w", DbLimits::default()).unwrap();
        db.migrate(&[PLAY_SCHEMA.into()]).unwrap();
        let host = Host::new("w", &[Capability::Db], None).with_db(Arc::new(db));
        let j = |s: String| serde_json::from_str::<serde_json::Value>(&s).unwrap();

        let ins = j(wasm_db_exec(&host, r#"{"sql":"INSERT INTO play VALUES (?, ?)","params":["a",1]}"#));
        assert_eq!(ins["ok"], true);
        assert_eq!(ins["changes"], 1);
        let batch = j(wasm_db_batch(
            &host,
            r#"{"statements":[{"sql":"INSERT INTO play VALUES ('b', 1)"},{"sql":"UPDATE play SET n = n + 1 WHERE media = :m","params":{"m":"a"}}]}"#,
        ));
        assert_eq!(batch["ok"], true);
        assert_eq!(batch["results"].as_array().unwrap().len(), 2);
        let q = j(wasm_db_query(&host, r#"{"sql":"SELECT media, n FROM play ORDER BY media"}"#));
        assert_eq!(q["ok"], true);
        assert_eq!(q["columns"], serde_json::json!(["media", "n"]));
        assert_eq!(q["rows"], serde_json::json!([["a", 2], ["b", 1]]));

        // Refusals and errors are data, never traps.
        let failed = j(wasm_db_batch(
            &host,
            r#"{"statements":[{"sql":"INSERT INTO play VALUES ('c', 1)"},{"sql":"INSERT INTO play VALUES ('a', 1)"}]}"#,
        ));
        assert_eq!(failed["ok"], false);
        assert!(failed["error"].as_str().unwrap().starts_with("statement 1"), "{failed}");
        let pragma = j(wasm_db_exec(&host, r#"{"sql":"PRAGMA journal_mode = OFF"}"#));
        assert_eq!(pragma["ok"], false);
        assert!(pragma["error"].as_str().unwrap().contains("not allowed"), "{pragma}");
        let bad = j(wasm_db_query(&host, r#"{"query":"SELECT 1"}"#));
        assert_eq!(bad["ok"], false);
        let without = Host::new("w", &[], None);
        let denied = j(wasm_db_query(&without, r#"{"sql":"SELECT 1"}"#));
        assert_eq!(denied["ok"], false);
        assert!(denied["error"].as_str().unwrap().contains("`db`"), "{denied}");
    }

    #[tokio::test]
    async fn db_admin_info_query_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let mut logger = decl("logger", true, toml::Table::new());
        logger.capabilities = vec![Capability::Db];
        let plain = decl("blacklist", true, toml::Table::new());
        let h = spawn_env(vec![logger, plain], db_env(dir.path()));

        let info = h.db_info("logger").await.unwrap();
        let inspect = info.inspect.expect("created at start");
        assert_eq!(inspect.schema_version, 0);
        assert!(inspect.tables.is_empty());
        let rows = h.db_query("logger", "SELECT count(*) FROM _stationd_migrations").await.unwrap();
        assert_eq!(rows.rows, vec![vec![serde_json::json!(0)]]);
        assert!(matches!(
            h.db_query("logger", "CREATE TABLE t (x)").await,
            Err(DbAdminError::Db(DbError::NotReadOnly))
        ));

        assert!(matches!(h.db_reset("logger").await, Err(DbAdminError::Precondition(_))));
        h.control("logger", Action::Stop).await.unwrap();
        assert_eq!(h.db_reset("logger").await, Ok(true));
        assert_eq!(h.db_info("logger").await.unwrap().inspect, None);
        assert_eq!(h.db_reset("logger").await, Ok(false));
        // Restart recreates it.
        h.control("logger", Action::Start).await.unwrap();
        assert!(h.db_info("logger").await.unwrap().inspect.is_some());

        assert!(matches!(h.db_info("nope").await, Err(DbAdminError::UnknownPlugin(_))));
        assert!(matches!(h.db_info("blacklist").await, Err(DbAdminError::Precondition(_))));
    }

    #[test]
    fn declarations_are_validated_as_a_whole() {
        let mut a = decl("a", true, toml::Table::new());
        let b = decl("a", false, toml::Table::new());
        assert!(validate_decls(&[a.clone(), b]).unwrap_err().contains("twice"));
        a.db = Some(DbLimits::default());
        assert!(validate_decls(&[a.clone()]).unwrap_err().contains("capability `db`"));
        a.capabilities = vec![Capability::Db];
        assert!(validate_decls(&[a.clone()]).is_ok());
        a.name = "a/b".into();
        assert!(validate_decls(&[a]).is_err());
    }

    // ----- simulation mode (on-air preview) -----------------------------

    /// Tries to act from its filter: push an override every time.
    struct Pushy {
        host: Option<Host>,
    }

    impl Plugin for Pushy {
        fn name(&self) -> &str {
            "pushy"
        }
        fn on_load(&mut self, host: Host) -> Result<(), String> {
            self.host = Some(host);
            Ok(())
        }
        fn filter_pool(&mut self, candidates: Vec<Candidate>) -> Vec<Candidate> {
            if let Some(h) = &self.host {
                let _ = h.push_override(OverrideRequest {
                    content: crate::station_control::OverrideContent::Media("a.mp3".into()),
                    mode: Default::default(),
                    expiry: None,
                    tracks: None,
                });
                let _ = h.control(ControlAction::Pause);
            }
            candidates
        }
    }

    #[test]
    fn a_simulated_filter_cannot_act_and_a_real_one_can() {
        let control = StationControl::new_in_memory();
        let host = Host::new("pushy", &[Capability::PushOverride, Capability::Control], Some(control.clone()));
        let mut p = Pushy { host: None };
        p.on_load(host.clone()).unwrap();
        let mut slots = vec![Slot { schema: Vec::new(), tabs: Vec::new(),
            decl: decl("pushy", true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(Box::new(p)),
            failures: VecDeque::new(),
            host: Some(host),
        }];

        let (kept, notes) = run_filters_mode(&mut slots, vec![cand("x.mp3")], true);
        assert_eq!(kept.len(), 1);
        assert!(notes.is_empty(), "a refused call is data for the plugin, not a failure");
        assert!(control.list_overrides().is_empty(), "no override pushed from a simulation");
        assert_eq!(control.state(), crate::station_control::BroadcastState::Running, "no pause either");

        // Out of the simulation, the same plugin acts again.
        run_filters_mode(&mut slots, vec![cand("x.mp3")], false);
        assert_eq!(control.list_overrides().len(), 1);
    }

    struct FilterPanic;
    impl Plugin for FilterPanic {
        fn name(&self) -> &str {
            "filter-panic"
        }
        fn filter_pool(&mut self, _candidates: Vec<Candidate>) -> Vec<Candidate> {
            panic!("boom in filter");
        }
    }

    #[test]
    fn a_failure_in_a_simulation_is_a_note_not_a_strike() {
        let mut slots = vec![Slot { schema: Vec::new(), tabs: Vec::new(),
            decl: decl("filter-panic", true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(Box::new(FilterPanic)),
            failures: VecDeque::new(),
            host: None,
        }];
        let (kept, notes) = run_filters_mode(&mut slots, vec![cand("x.mp3")], true);
        assert_eq!(kept.len(), 1, "the stage passes through");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, "filter-panic", "{notes:?}");
        assert!(slots[0].failures.is_empty(), "not counted towards quarantine");
        assert!(matches!(slots[0].state, PluginState::Loaded));
    }

    #[tokio::test]
    async fn a_simulation_handle_emits_nothing() {
        let h = spawn(vec![]);
        let sim = h.simulation();
        sim.emit(PluginEvent::ListenersSampled { count: 1, at: 0 }); // no-op, no panic
        assert_eq!(sim.filter_pool(vec![cand("a.mp3")]).await.len(), 1);
        assert!(sim.simulation_notes().is_empty());
    }
    #[test]
    fn metadata_only_replies_merge_first_wins_and_legacy_json_still_works() {
        let legacy: ScanEnrichment = serde_json::from_str(r#"{"rel_path":"x.mp3","genres":["talks"]}"#).unwrap();
        assert!(legacy.metadata.is_empty());
        let reply = |value: &str| ScanEnrichment {
            rel_path: "x.mp3".into(), genres: vec![],
            metadata: [("tempo".into(), value.into())].into(),
        };
        let mut slots = vec![
            loaded("first", Box::new(FixedScan(Ok(vec![reply("slow")])))),
            loaded("second", Box::new(FixedScan(Ok(vec![reply("fast")])))),
        ];
        let extras = run_scan(&mut slots, &[scan_input("x.mp3", &[])]);
        assert_eq!(extras["x.mp3"].metadata["tempo"], "slow");
        assert!(extras["x.mp3"].genres.is_empty());
        let known = ["x.mp3"].into_iter().collect();
        for (key, value) in [("", "x"), ("bad key", "x"), ("tempo", " "), ("creation", "invalid")] {
            let invalid = ScanEnrichment { rel_path: "x.mp3".into(), genres: vec![], metadata: [(key.into(), value.into())].into() };
            assert!(validate_enrichment(&known, &[invalid]).is_err());
        }
    }

}

#[cfg(test)]
mod bpm_policy_tests {
    use super::*;
    #[test]
    fn tag_hints_read_the_custom_tags_configuration() {
        let d: PluginDecl = toml::from_str(
            r#"
name = "custom-tags"
[config]
tags = ["Type", " "]
[config.tempo]
enabled = true
[[config.tempo.range]]
max = 99
value = "slow"
[[config.tempo.range]]
min = 100
value = "fast"
"#,
        )
        .unwrap();
        let h = d.tag_hints();
        assert_eq!(h.genre_sources, ["Type"]);
        assert_eq!(h.tempo_labels, ["slow", "fast"]);
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use crate::plugin_ui::UiTab;
    struct TablePlugin { tab: UiTab, host: Option<Host> }
    impl Plugin for TablePlugin {
        fn name(&self) -> &str { "table" }
        fn on_load(&mut self, host: Host) -> Result<(), String> { self.host = Some(host); Ok(()) }
        fn ui_tabs(&mut self) -> Result<Vec<UiTab>, String> {
            // Metadata discovery must not be a route to a database mutation.
            let db = self.host.as_ref().unwrap().db().unwrap();
            assert!(matches!(db.exec(&plugin_db::Statement {
                sql: "INSERT INTO sample VALUES (99)".into(), params: Default::default(),
            }), Err(DbError::ReadOnly)));
            Ok(vec![self.tab.clone()])
        }
    }
    fn tab(sql: &str) -> UiTab {
        UiTab { id: "sample".into(), title: "Sample".into(), description: "".into(), kind: String::new(), sql: sql.into() }
    }
    fn slot() -> Slot {
        let decl: PluginDecl = toml::from_str("name = 'table'\nenabled = true\ncapabilities = ['db']").unwrap();
        Slot { schema: Vec::new(), tabs: vec![], decl, state: PluginState::Disabled, plugin: None, failures: VecDeque::new(), host: None }
    }
    #[test]
    fn ui_tabs_are_read_only_scoped_and_retained_when_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let env = PluginEnv { db_dir: Some(dir.path().into()), ..Default::default() };
        let mut slot = slot();
        let host = slot.open_host(&env).unwrap();
        host.db().unwrap().migrate(&["CREATE TABLE sample (n); INSERT INTO sample VALUES (7)".into()]).unwrap();
        slot.load(Box::new(TablePlugin { tab: tab("SELECT n FROM sample"), host: None }), host);
        assert_eq!(slot.info().state, "loaded");
        assert_eq!(slot.info().tabs.len(), 1);
        let slots = vec![slot];
        let (loc, sql) = tab_location(&slots, "table", "sample", &env).unwrap();
        let rows = plugin_db::query_file_bounded(&loc.path, &sql, 1000, 200).unwrap();
        assert_eq!(rows.rows, vec![vec![serde_json::json!(7)]]);
        assert!(tab_location(&slots, "missing", "sample", &env).is_err());
        assert!(tab_location(&slots, "table", "missing", &env).is_err());
        assert!(plugin_db::query_file_bounded(&loc.path, "DELETE FROM sample", 1000, 200).is_err());
        assert!(plugin_db::query_file_bounded(&loc.path, "ATTACH DATABASE ':memory:' AS other", 1000, 200).is_err());
        let mut slots = slots;
        slots[0].stop();
        assert_eq!(slots[0].info().tabs.len(), 1);
        assert!(tab_location(&slots, "table", "sample", &env).is_err());
    }
    #[test]
    fn invalid_ui_is_visible_as_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let env = PluginEnv { db_dir: Some(dir.path().into()), ..Default::default() };
        let mut slot = slot();
        let host = slot.open_host(&env).unwrap();
        let mut invalid = tab("SELECT 1"); invalid.id = "../escape".into();
        slot.load(Box::new(TablePlugin { tab: invalid, host: None }), host);
        assert_eq!(slot.info().state, "failed");
        assert!(slot.info().reason.contains("invalid tab id"));
    }

    #[tokio::test]
    #[ignore = "requires STATIOND_TEST_LISTENER_UI_WASM pointing to the rebuilt listener-stats guest"]
    async fn real_wasm_ui_tabs_aggregate_audience_and_preserve_historical_geography() {
        let dir = tempfile::tempdir().unwrap();
        let mut decl: PluginDecl = toml::from_str("name = 'audience-test'\nenabled = true\ncapabilities = ['db', 'listener_details', 'geoip']").unwrap();
        decl.wasm = Some(std::env::var("STATIOND_TEST_LISTENER_UI_WASM").unwrap());
        let handle = spawn_env(vec![decl], PluginEnv { db_dir: Some(dir.path().into()), ..Default::default() });
        let info = handle.list().await.remove(0);
        assert_eq!(info.state, "loaded", "{}", info.reason);
        assert_eq!(info.tabs.len(), 4);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        handle.emit(PluginEvent::ListenerSnapshot { mount: "/radio".into(), at: now - 2,
            listeners: Some(vec![crate::listener_snapshot::Listener {
                id: "private-id".into(), ip: "8.8.8.8".parse().unwrap(), connected_seconds: 10, user_agent: None,
            }]),
        });
        let geo = handle.read_tab("audience-test", "geography").await.unwrap();
        assert_eq!(geo.rows.len(), 1);
        assert_eq!(geo.rows[0][7], serde_json::json!("unavailable"));
        handle.emit(PluginEvent::ListenerSnapshot { mount: "/radio".into(), at: now - 1, listeners: None });
        let table = handle.read_tab("audience-test", "audience").await.unwrap();
        assert_eq!(table.rows[0][1], serde_json::Value::Null);
        assert_eq!(handle.read_tab("audience-test", "geography").await.unwrap().rows.len(), 1);
        handle.emit(PluginEvent::ListenerSnapshot { mount: "/radio".into(), at: now, listeners: Some(vec![]) });
        let table = handle.read_tab("audience-test", "audience").await.unwrap();
        assert_eq!(table.rows[0][1], serde_json::json!(0));
        assert_eq!(table.rows[0][2], serde_json::json!(0.5));
        assert_eq!(table.rows[0][3], serde_json::json!(1));
        assert_eq!(table.rows[0][4], serde_json::json!(66.7));
        let geo = handle.read_tab("audience-test", "geography").await.unwrap();
        assert_eq!(geo.rows[0][4], serde_json::json!(0.5));
        for tab in ["hourly", "daily"] {
            assert!(!handle.read_tab("audience-test", tab).await.unwrap().rows.is_empty());
        }
    }


    #[tokio::test]
    #[ignore = "requires STATIOND_TEST_UI_WASM pointing to the rebuilt play-stats guest"]
    async fn real_wasm_ui_tabs_survive_restart_and_work_with_a_renamed_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let mut decl: PluginDecl = toml::from_str("name = 'renamed-stats'\nenabled = true\ncapabilities = ['db']").unwrap();
        decl.wasm = Some(std::env::var("STATIOND_TEST_UI_WASM").unwrap());
        let handle = spawn_env(vec![decl], PluginEnv { db_dir: Some(dir.path().into()), ..Default::default() });
        let info = handle.list().await.remove(0);
        assert_eq!(info.state, "loaded", "{}", info.reason);
        assert_eq!(info.tabs[0].id, "plays");
        let table = handle.read_tab("renamed-stats", "plays").await.unwrap();
        assert_eq!(table.columns.len(), 3);
        assert!(table.rows.is_empty());
        handle.emit(PluginEvent::TrackResolved { media_path: Some("song.mp3".into()), playlist_ref: None, rule_id: None, origin: "test".into() });
        let table = handle.read_tab("renamed-stats", "plays").await.unwrap();
        assert_eq!(table.rows[0][0], serde_json::json!("song.mp3"));
        assert_eq!(table.rows[0][1], serde_json::json!(1));
        assert!(handle.read_tab("renamed-stats", "unknown").await.is_err());
        handle.control("renamed-stats", Action::Stop).await.unwrap();
        assert_eq!(handle.list().await[0].tabs.len(), 1);
        assert!(handle.read_tab("renamed-stats", "plays").await.is_err());
        handle.control("renamed-stats", Action::Reload).await.unwrap();
        assert!(handle.read_tab("renamed-stats", "plays").await.is_ok());
    }
}
