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
//! (`capabilities = ["control", "push_override"]`). Native plugins call it
//! directly; WASM plugins through extism host functions (`station_control`,
//! `push_override`, JSON in/out). The per-plugin db (`db_*`) is a later slice.
//!
//! Failure handling (no-silent-failure, visible): a plugin that fails `on_load`
//! is recorded `Failed` and inactive — the daemon and other plugins still
//! start. A plugin that panics in `on_event` is caught; repeated failures in a
//! sliding window quarantine it (hooks no longer called) until an explicit
//! restart. Nothing retries silently in a loop.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use extism::{host_fn, Function, Manifest, Plugin as ExtismPlugin, UserData, Wasm, PTR};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

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
}

/// What a plugin implements. `Send` because plugins live on the actor task.
///
/// A1 wires `on_load` / `on_unload` (lifecycle) and `on_event` (observation).
/// A2 adds the synchronous hooks: `filter_pool` (influences the decision) and
/// `on_scan` (enriches the library scan).
pub trait Plugin: Send {
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
    /// The broadcast state changed (`running|paused|stopped|draining`). `by`
    /// names the emitter: a plugin, `cli`, or `stop-when-idle` for a drain
    /// completed by the core at a track boundary.
    BroadcastStateChanged { from: String, to: String, by: String },
    /// A DJ took the air (harbor): `dj` = its id in the DJ file, `rule_id` =
    /// the grid `live` rule whose window let it in. `at` = epoch seconds.
    LiveStarted { dj: String, rule_id: String, at: i64 },
    /// The live ended: `reason` = `disconnected` (the DJ left), `silence`
    /// (cut after `[live] silence_timeout`) or `kicked` (`stationctl live
    /// kick`). The programme resumes with a track chosen at that instant.
    LiveEnded { dj: String, reason: String, at: i64 },
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
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Control => "control",
            Capability::PushOverride => "push_override",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("plugin `{plugin}` did not declare capability `{capability}`")]
    Denied { plugin: String, capability: &'static str },
    #[error("no station control is wired")]
    Unavailable,
    #[error(transparent)]
    Control(#[from] ControlError),
}

/// A plugin's host surface, handed over in `on_load`. Scoped: it carries the
/// plugin's name (recorded as the emitter of every action) and its declared
/// capabilities; a call outside them is refused and logged, never performed.
/// Capabilities, never paths or handles (sandboxing).
#[derive(Clone)]
pub struct Host {
    plugin: String,
    capabilities: Vec<Capability>,
    control: Option<StationControl>,
}

impl Host {
    pub fn new(plugin: &str, capabilities: &[Capability], control: Option<StationControl>) -> Self {
        Self {
            plugin: plugin.to_string(),
            capabilities: capabilities.to_vec(),
            control,
        }
    }

    pub fn plugin_name(&self) -> &str {
        &self.plugin
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    fn require(&self, capability: Capability) -> Result<&StationControl, HostError> {
        if !self.has(capability) {
            let err = HostError::Denied {
                plugin: self.plugin.clone(),
                capability: capability.as_str(),
            };
            tracing::warn!(%err, "host call refused");
            return Err(err);
        }
        self.control.as_ref().ok_or(HostError::Unavailable)
    }

    /// Pilot the broadcast (first-class station control; the plugin is just
    /// one more emitter). Needs capability `control`.
    pub fn control(&self, action: ControlAction) -> Result<Option<Transition>, HostError> {
        Ok(self.require(Capability::Control)?.apply(action, &self.plugin)?)
    }

    /// Push content ahead of the grid. Needs capability `push_override`.
    pub fn push_override(&self, req: OverrideRequest) -> Result<PushOutcome, HostError> {
        Ok(self
            .require(Capability::PushOverride)?
            .push_override(req, &self.plugin)?)
    }
}

// ---------------------------------------------------------------------------
// Declaration (config) and runtime state
// ---------------------------------------------------------------------------

/// One `[[plugin]]` entry in the station config. `name` is both identity and
/// kind in A1 (one instance per kind); a separate `kind` for multiple
/// instances is a later refinement. `config` is opaque to the core and handed
/// to the plugin at build time.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDecl {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Hook application order, ascending; default 50 (neutral rank). Ties
    /// broken by name for determinism.
    #[serde(default = "default_order")]
    pub order: u32,
    /// Path to a `.wasm` module. When set, this is a WASM plugin loaded from
    /// that file (via extism); when absent, `name` selects a built-in.
    #[serde(default)]
    pub wasm: Option<String>,
    /// Host calls this plugin may make (`control`, `push_override`). Empty by
    /// default: a plugin acts on nothing unless it says so.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub config: toml::Table,
}

fn default_enabled() -> bool {
    true
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

/// A flat, cloneable snapshot of a plugin for `plugin list`.
#[derive(Debug, Clone)]
pub struct PluginInfo {
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
// ---------------------------------------------------------------------------

struct Slot {
    decl: PluginDecl,
    state: PluginState,
    plugin: Option<Box<dyn Plugin>>,
    failures: VecDeque<Instant>,
}

impl Slot {
    fn info(&self) -> PluginInfo {
        let failures = match &self.state {
            PluginState::Quarantined { failures, .. } => *failures,
            _ => self.failures.len() as u32,
        };
        PluginInfo {
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
    /// Sets `Loaded` or `Failed`.
    fn start(&mut self, control: Option<&StationControl>) {
        let host = Host::new(&self.decl.name, &self.decl.capabilities, control.cloned());
        match build_plugin(&self.decl, &host) {
            Err(reason) => {
                self.plugin = None;
                self.state = PluginState::Failed { phase: Phase::Load, reason };
            }
            Ok(mut plugin) => match catch(|| plugin.on_load(host)) {
                Ok(Ok(())) => {
                    self.plugin = Some(plugin);
                    self.state = PluginState::Loaded;
                    self.failures.clear();
                }
                Ok(Err(reason)) | Err(reason) => {
                    self.plugin = None;
                    self.state = PluginState::Failed { phase: Phase::Load, reason };
                }
            },
        }
    }

    /// Run `on_unload` (best-effort) and drop the instance → `Disabled`.
    fn stop(&mut self) {
        if let Some(mut plugin) = self.plugin.take() {
            let _ = catch(|| plugin.on_unload());
        }
        self.state = PluginState::Disabled;
        self.failures.clear();
    }

    /// Record a runtime hook failure; quarantine past the sliding-window limit.
    fn note_failure(&mut self, phase: Phase, reason: String) {
        tracing::warn!(plugin = %self.decl.name, ?phase, %reason, "plugin hook failed");
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
            tracing::warn!(plugin = %self.decl.name, failures, "plugin quarantined");
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
    if let Some(path) = &decl.wasm {
        return WasmPlugin::new(decl.name.clone(), path, &decl.config, host)
            .map(|p| Box::new(p) as Box<dyn Plugin>);
    }
    match decl.name.as_str() {
        "logger" => Ok(Box::new(LoggerPlugin::from_config(&decl.config))),
        "blacklist" => Ok(Box::new(BlacklistPlugin::from_config(&decl.config))),
        "stop-when-idle" => Ok(Box::new(StopWhenIdlePlugin::from_config(&decl.config)?)),
        other => Err(format!("unknown plugin kind `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

enum Msg {
    List(oneshot::Sender<Vec<PluginInfo>>),
    Control {
        name: String,
        action: Action,
        reply: oneshot::Sender<Result<PluginInfo, String>>,
    },
    Event(PluginEvent),
    FilterPool {
        candidates: Vec<Candidate>,
        reply: oneshot::Sender<Vec<Candidate>>,
    },
    Scan {
        media: Vec<ScanInput>,
        reply: oneshot::Sender<ScanExtras>,
    },
}

/// Merged `on_scan` output: extra genres per `rel_path`, every loaded plugin
/// contributing in `order`. Deduplicated case-insensitively per media.
pub type ScanExtras = std::collections::BTreeMap<String, Vec<String>>;

/// Cheap, clonable handle to the plugin actor. The only way to reach plugins.
#[derive(Clone)]
pub struct PluginHandle {
    tx: mpsc::Sender<Msg>,
}

impl PluginHandle {
    /// Fire-and-forget: emit an event to the plugins. Never blocks the caller;
    /// if the buffer is full the event is dropped (best-effort, as documented).
    pub fn emit(&self, event: PluginEvent) {
        let _ = self.tx.try_send(Msg::Event(event));
    }

    /// Run the candidate pool through every loaded plugin's `filter_pool`, in
    /// `order`. Awaits a reply (it feeds the decision). If the actor is gone,
    /// degrades to the unfiltered pool rather than failing the decision.
    pub async fn filter_pool(&self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        let (reply, rx) = oneshot::channel();
        let fallback = candidates.clone();
        if self
            .tx
            .send(Msg::FilterPool { candidates, reply })
            .await
            .is_err()
        {
            return fallback;
        }
        rx.await.unwrap_or(fallback)
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
    spawn_with(decls, None)
}

/// Spawn the owning task from the declared plugins and return a handle. Enabled
/// plugins are loaded immediately (a failure is recorded, not fatal); disabled
/// ones stay `Disabled`. Slots are ordered by (`order`, `name`). `control` is
/// the station control behind every plugin's host surface.
pub fn spawn_with(mut decls: Vec<PluginDecl>, control: Option<StationControl>) -> PluginHandle {
    decls.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.name.cmp(&b.name)));
    let mut slots: Vec<Slot> = decls
        .into_iter()
        .map(|decl| {
            let mut slot = Slot {
                decl,
                state: PluginState::Disabled,
                plugin: None,
                failures: VecDeque::new(),
            };
            if slot.decl.enabled {
                slot.start(control.as_ref());
            }
            slot
        })
        .collect();

    let (tx, mut rx) = mpsc::channel::<Msg>(128);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Event(event) => dispatch_event(&mut slots, &event),
                Msg::FilterPool { candidates, reply } => {
                    let _ = reply.send(run_filters(&mut slots, candidates));
                }
                Msg::Scan { media, reply } => {
                    let _ = reply.send(run_scan(&mut slots, &media));
                }
                Msg::List(reply) => {
                    let _ = reply.send(slots.iter().map(Slot::info).collect());
                }
                Msg::Control { name, action, reply } => {
                    let res = match slots.iter_mut().find(|s| s.decl.name == name) {
                        None => Err(format!("unknown plugin `{name}`")),
                        Some(slot) => {
                            apply_action(slot, action, control.as_ref());
                            Ok(slot.info())
                        }
                    };
                    let _ = reply.send(res);
                }
            }
        }
    });

    PluginHandle { tx }
}

fn apply_action(slot: &mut Slot, action: Action, control: Option<&StationControl>) {
    match action {
        Action::Start => {
            // Idempotent: starting an already-loaded plugin is a no-op.
            if !matches!(slot.state, PluginState::Loaded) {
                slot.start(control);
            }
        }
        Action::Stop => slot.stop(),
        // reload == restart in native (no artefact to re-read); a WASM plugin
        // is rebuilt from its file on start, so both re-read it.
        Action::Restart | Action::Reload => {
            slot.stop();
            slot.start(control);
        }
    }
}

fn dispatch_event(slots: &mut [Slot], event: &PluginEvent) {
    for slot in slots.iter_mut() {
        if !matches!(slot.state, PluginState::Loaded) {
            continue;
        }
        let outcome = slot.plugin.as_mut().map(|p| catch(|| p.on_event(event)));
        if let Some(Err(reason)) = outcome {
            slot.note_failure(Phase::Event, reason);
        }
    }
}

/// Chain the candidate pool through every loaded plugin's `filter_pool`, in
/// slot order (already sorted by `order`, then name). A plugin that panics
/// degrades to pass-through for that stage and is counted as a failure.
fn run_filters(slots: &mut [Slot], candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut cur = candidates;
    for slot in slots.iter_mut() {
        if !matches!(slot.state, PluginState::Loaded) {
            continue;
        }
        let before = cur.len();
        let outcome = slot.plugin.as_mut().map(|p| {
            let input = cur.clone();
            catch(move || p.filter_pool(input))
        });
        match outcome {
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
    cur
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
                    for g in e.genres {
                        let g = g.trim().to_string();
                        let key = crate::media_index::genre_key(&g);
                        if !entry.iter().any(|x| crate::media_index::genre_key(x) == key) {
                            entry.push(g);
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
    extras.retain(|_, v| !v.is_empty());
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

/// Demo of the A2 composition « observe + act »: on `ListenersSampled` with a
/// count of 0 (for `min_zero_samples` consecutive samples, default 1) it arms
/// `host.control(StopWhenIdle)`. The core owns the mechanism (the drain
/// completes at the next track boundary if the audience is still 0); this
/// plugin only carries the policy. A non-zero sample resets the streak. It
/// arms once per idle period: an operator's `resume` is not overridden until
/// the audience comes back and leaves again.
///
/// Requires capability `control` — refused at `on_load` otherwise (visible
/// `Failed`), never a policy that silently can't act.
struct StopWhenIdlePlugin {
    host: Option<Host>,
    min_zero_samples: u32,
    zero_streak: u32,
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
        Ok(Self { host: None, min_zero_samples: min, zero_streak: 0 })
    }
}

impl Plugin for StopWhenIdlePlugin {
    fn name(&self) -> &str {
        "stop-when-idle"
    }

    fn on_load(&mut self, host: Host) -> Result<(), String> {
        if !host.has(Capability::Control) {
            return Err("stop-when-idle requires `capabilities = [\"control\"]`".into());
        }
        self.host = Some(host);
        self.zero_streak = 0;
        Ok(())
    }

    fn on_event(&mut self, event: &PluginEvent) {
        let PluginEvent::ListenersSampled { count, .. } = event else {
            return;
        };
        if *count > 0 {
            self.zero_streak = 0;
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

/// `station_control` input: `{"action": "stop|pause|resume|stop_when_idle"}`.
fn wasm_station_control(host: &Host, input: &str) -> String {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Req {
        action: ControlAction,
    }
    host_reply(
        serde_json::from_str::<Req>(input)
            .map_err(|e| format!("bad station_control input: {e}"))
            .and_then(|r| host.control(r.action).map_err(|e| e.to_string()))
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

/// A WASM plugin loaded from a `.wasm` file via extism. Implements the same
/// `Plugin` trait as the built-ins by delegating to the module's exports,
/// serialising data to JSON at the boundary. `extism::Plugin` is `Send`, so
/// this lives on the actor task like any other plugin.
///
/// Exports (all optional): `filter_pool` (missing → pass-through), `on_event`
/// (missing → ignored), `on_scan` (JSON `[ScanInput]` → `[ScanEnrichment]`;
/// missing → nothing to add). Host functions offered to the guest (A2), bound to
/// this plugin's scoped `Host`: `station_control`, `push_override` (imported
/// by the guest from `extern "ExtismHost"`). Config reaches the guest as JSON
/// under the key "config".
struct WasmPlugin {
    name: String,
    plugin: ExtismPlugin,
    has_filter: bool,
    has_event: bool,
    has_scan: bool,
}

impl WasmPlugin {
    fn new(name: String, path: &str, config: &toml::Table, host: &Host) -> Result<Self, String> {
        // Pass the plugin's TOML config to the guest as a single JSON string
        // under the key "config"; the guest reads it via `config::get("config")`.
        let config_json = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
        let manifest = Manifest::new([Wasm::file(path)])
            .with_config([("config".to_string(), config_json)].into_iter());
        let functions = [
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
        ];
        let plugin =
            ExtismPlugin::new(&manifest, functions, false).map_err(|e| e.to_string())?;
        let has_filter = plugin.function_exists("filter_pool");
        let has_event = plugin.function_exists("on_event");
        let has_scan = plugin.function_exists("on_scan");
        Ok(Self {
            name,
            plugin,
            has_filter,
            has_event,
            has_scan,
        })
    }
}

impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, enabled: bool, config: toml::Table) -> PluginDecl {
        PluginDecl {
            name: name.to_string(),
            enabled,
            order: 50,
            wasm: None,
            capabilities: vec![],
            config,
        }
    }

    // ----- host surface (A2) ---------------------------------------------

    #[test]
    fn host_refuses_an_undeclared_capability() {
        let control = StationControl::new_in_memory();
        let host = Host::new("stats", &[], Some(control.clone()));
        assert!(matches!(
            host.control(ControlAction::Stop),
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
        assert!(matches!(host.control(ControlAction::Stop), Err(HostError::Unavailable)));
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
            crate::station_control::Gate::Halt(crate::station_control::BroadcastState::Stopped)
        );
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
        Slot {
            decl: decl("panic", true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(Box::new(PanicPlugin)),
            failures: VecDeque::new(),
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
        Slot {
            decl: decl("x", true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(plugin),
            failures: VecDeque::new(),
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
                .map(|m| ScanEnrichment {
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
        Slot {
            decl: decl(name, true, toml::Table::new()),
            state: PluginState::Loaded,
            plugin: Some(plugin),
            failures: VecDeque::new(),
        }
    }

    #[test]
    fn on_scan_merges_plugins_and_dedups_case_insensitively() {
        let mut slots = vec![
            loaded("a", Box::new(TagToGenre("Type"))),
            loaded(
                "b",
                Box::new(FixedScan(Ok(vec![ScanEnrichment {
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
        assert_eq!(extras["x.mp3"], vec!["talks".to_string(), "news".to_string()]);
        assert!(slots.iter().all(|s| matches!(s.state, PluginState::Loaded)));
    }

    #[test]
    fn on_scan_error_or_invalid_reply_counts_as_failure_and_adds_nothing() {
        let bad_path = ScanEnrichment { rel_path: "ghost.mp3".into(), genres: vec!["x".into()] };
        let blank = ScanEnrichment { rel_path: "x.mp3".into(), genres: vec!["  ".into()] };
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
}
