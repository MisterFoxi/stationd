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
//! Scope of A1: lifecycle + `on_event`. The synchronous hooks `on_scan` /
//! `filter_pool` (which return a value into the scan / decision path) are NOT
//! wired here — they need a synchronous sharing design distinct from this
//! fire-and-forget actor, and land with their call sites. The host surface
//! (`push_override`, `control`, per-plugin db) is A2.
//!
//! Failure handling (no-silent-failure, visible): a plugin that fails `on_load`
//! is recorded `Failed` and inactive — the daemon and other plugins still
//! start. A plugin that panics in `on_event` is caught; repeated failures in a
//! sliding window quarantine it (hooks no longer called) until an explicit
//! restart. Nothing retries silently in a loop.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use extism::{Manifest, Plugin as ExtismPlugin, Wasm};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

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

/// What a plugin implements. `Send` because plugins live on the actor task.
///
/// A1 wires `on_load` / `on_unload` (lifecycle) and `on_event` (observation).
/// A2 adds `filter_pool` (synchronous, influences the decision). `on_scan`
/// will follow with its call site.
pub trait Plugin: Send {
    /// Stable name (matches the declaration). Identity for `plugin list`.
    fn name(&self) -> &str;

    /// Activate: open resources, read config. Synchronous, bounded, may fail —
    /// a failure refuses the plugin (state `Failed{load}`), others start anyway.
    fn on_load(&mut self) -> Result<(), String> {
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
}

/// Facts the core notifies plugins about. `#[non_exhaustive]`: a plugin must
/// `_ => {}` on unknown variants, so adding events never breaks a plugin.
///
/// A1 emits only `TrackResolved` (the one source that already exists). Others
/// from `Doc/plugin-events.md` (`TrackSkipped`, `LibraryScanned`, `GridApplied`,
/// `ListenersSampled`, …) are added as their sources come online.
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
        }
    }

    /// (Re)build the instance and run `on_load`. Sets `Loaded` or `Failed`.
    fn start(&mut self) {
        match build_plugin(&self.decl) {
            Err(reason) => {
                self.plugin = None;
                self.state = PluginState::Failed { phase: Phase::Load, reason };
            }
            Ok(mut plugin) => match catch(|| plugin.on_load()) {
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

/// Map a declaration to a native plugin instance. A1 knows only `logger`; an
/// unknown name is a loud (visible) failure, never silently ignored.
fn build_plugin(decl: &PluginDecl) -> Result<Box<dyn Plugin>, String> {
    if let Some(path) = &decl.wasm {
        return WasmPlugin::new(decl.name.clone(), path, &decl.config)
            .map(|p| Box::new(p) as Box<dyn Plugin>);
    }
    match decl.name.as_str() {
        "logger" => Ok(Box::new(LoggerPlugin::from_config(&decl.config))),
        "blacklist" => Ok(Box::new(BlacklistPlugin::from_config(&decl.config))),
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
}

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

/// Spawn the owning task from the declared plugins and return a handle. Enabled
/// plugins are loaded immediately (a failure is recorded, not fatal); disabled
/// ones stay `Disabled`. Slots are ordered by (`order`, `name`).
pub fn spawn(mut decls: Vec<PluginDecl>) -> PluginHandle {
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
                slot.start();
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
                Msg::List(reply) => {
                    let _ = reply.send(slots.iter().map(Slot::info).collect());
                }
                Msg::Control { name, action, reply } => {
                    let res = match slots.iter_mut().find(|s| s.decl.name == name) {
                        None => Err(format!("unknown plugin `{name}`")),
                        Some(slot) => {
                            apply_action(slot, action);
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

fn apply_action(slot: &mut Slot, action: Action) {
    match action {
        Action::Start => {
            // Idempotent: starting an already-loaded plugin is a no-op.
            if !matches!(slot.state, PluginState::Loaded) {
                slot.start();
            }
        }
        Action::Stop => slot.stop(),
        // reload == restart in native (no artefact to re-read); they diverge
        // only with WASM.
        Action::Restart | Action::Reload => {
            slot.stop();
            slot.start();
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

    fn on_load(&mut self) -> Result<(), String> {
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

    fn on_load(&mut self) -> Result<(), String> {
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

/// A WASM plugin loaded from a `.wasm` file via extism. Implements the same
/// `Plugin` trait as the built-ins by delegating to the module's exports,
/// serialising data to JSON at the boundary. `extism::Plugin` is `Send`, so
/// this lives on the actor task like any other plugin.
///
/// WASM-1 scope: `filter_pool` and `on_event` exports (both optional — a
/// missing export means "pass-through" / "ignore"). Host functions
/// (`push_override`, `control`, db) are A2. Config-to-guest plumbing is the
/// next increment.
struct WasmPlugin {
    name: String,
    plugin: ExtismPlugin,
    has_filter: bool,
    has_event: bool,
}

impl WasmPlugin {
    fn new(name: String, path: &str, config: &toml::Table) -> Result<Self, String> {
        // Pass the plugin's TOML config to the guest as a single JSON string
        // under the key "config"; the guest reads it via `config::get("config")`.
        let config_json = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
        let manifest = Manifest::new([Wasm::file(path)])
            .with_config([("config".to_string(), config_json)].into_iter());
        let plugin = ExtismPlugin::new(&manifest, [], false).map_err(|e| e.to_string())?;
        let has_filter = plugin.function_exists("filter_pool");
        let has_event = plugin.function_exists("on_event");
        Ok(Self {
            name,
            plugin,
            has_filter,
            has_event,
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
            config,
        }
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
}
