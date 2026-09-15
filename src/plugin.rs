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

use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

/// Sliding-window failure policy: N failures within WINDOW → quarantine.
const MAX_FAILURES: u32 = 3;
const WINDOW: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Contract: the trait a plugin implements, and the events it observes
// ---------------------------------------------------------------------------

/// What a plugin implements. `Send` because plugins live on the actor task.
///
/// A1 wires `on_load` / `on_unload` (lifecycle) and `on_event` (observation).
/// `on_scan` / `filter_pool` will be added with their synchronous call sites.
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
}

/// Facts the core notifies plugins about. `#[non_exhaustive]`: a plugin must
/// `_ => {}` on unknown variants, so adding events never breaks a plugin.
///
/// A1 emits only `TrackResolved` (the one source that already exists). Others
/// from `Doc/plugin-events.md` (`TrackSkipped`, `LibraryScanned`, `GridApplied`,
/// `ListenersSampled`, …) are added as their sources come online.
#[non_exhaustive]
#[derive(Debug, Clone)]
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
    match decl.name.as_str() {
        "logger" => Ok(Box::new(LoggerPlugin::from_config(&decl.config))),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(name: &str, enabled: bool, config: toml::Table) -> PluginDecl {
        PluginDecl {
            name: name.to_string(),
            enabled,
            order: 50,
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
}
