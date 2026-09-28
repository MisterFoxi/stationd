//! Station runtime control — the mechanism behind the plugin host surface (A2)
//! and the first-class `stationctl station …` / `stationctl override …`.
//!
//! « Mécanisme au core, politique au plugin » (Doc/plugin-host.md): this module
//! owns the *capabilities* — the broadcast state machine, the override queue,
//! the last listener sample — and holds no business condition. A plugin (or a
//! human via the CLI) decides *when* to use them.
//!
//! - **Broadcast state** `running | paused | draining | sleeping`. `draining`
//!   is an armed graceful stop: it becomes `sleeping` at the next track
//!   boundary (`gate`) once the last listener sample is 0. `sleeping` = the
//!   idle stop (background noise on air, nothing resolved); `wake` brings it
//!   back and is a no-op from any other state, so an audience can never undo
//!   an operator's pause. The operator's stop is not a broadcast state: it is
//!   stationd itself stopped (`shutdown`, marker `data/stationd.stopped`).
//!   Persisted (family B, migrations 0014 / 0021) through an ordered writer
//!   task, so the synchronous API stays callable from a plugin hook.
//! - **Core wake rules** (mechanism, not policy): a sleeping station wakes
//!   when the audience becomes *unknown* (a failure is never read as « no
//!   one listens ») and when a DJ takes the air.
//! - **Override queue**: content pushed ahead of the grid (`next_media`
//!   consults it before `resolve_next`). In memory, capped, with per-entry
//!   expiry (a missed override is dropped, never replayed late). With
//!   Liquidsoap wired, a push is forwarded to the air: `soft` drops the
//!   prepared track (airs at the next boundary), `hard` cuts the current track
//!   now. Without Liquidsoap — or while the station is halted — `hard` is
//!   degraded to `soft` (warned).
//! - **Manual clock** (testing): the frozen instant shared with `GridEngine`,
//!   so override expiry and grid resolution ride the same clock.
//!
//! With Liquidsoap wired, every transition is also forwarded to the air
//! (`attach_air` → `ls_control`): `paused` pauses the air now, `running`
//! resumes it; `sleeping` is reached at a track boundary (via `gate`).
//!
//! Every state change emits `BroadcastStateChanged`; every listener sample
//! emits `ListenersSampled` (best-effort, via the plugin handle once
//! attached). The API is synchronous and lock-short: no lock is ever held
//! across an await or an event emission.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::{mpsc, watch};

use crate::plugin::{PluginEvent, PluginHandle};
use crate::resolver::Epoch;

/// Cap on pending overrides: a plugin that pushes on every event is bounded
/// (refused past this, loudly), it can't grow the queue without limit.
pub const MAX_PENDING_OVERRIDES: usize = 64;

// ---------------------------------------------------------------------------
// Broadcast state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastState {
    Running,
    Paused,
    /// Armed graceful stop: sleeps at the next clean occasion (track boundary
    /// AND zero listeners). Not a kill.
    Draining,
    /// Idle stop (no listeners): background noise on air, nothing resolved.
    /// Left by `wake` (audience back, DJ, unknown audience) or `resume`.
    Sleeping,
}

impl BroadcastState {
    pub fn as_str(self) -> &'static str {
        match self {
            BroadcastState::Running => "running",
            BroadcastState::Paused => "paused",
            BroadcastState::Draining => "draining",
            BroadcastState::Sleeping => "sleeping",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => BroadcastState::Running,
            "paused" => BroadcastState::Paused,
            "draining" => BroadcastState::Draining,
            "sleeping" => BroadcastState::Sleeping,
            _ => return None,
        })
    }
}

/// A control command, from the CLI or a plugin (`host.control`). Stopping
/// stationd itself is not one of them: it is the operator's `shutdown`
/// (gRPC `Station.Shutdown`), out of any plugin's reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Pause,
    Resume,
    StopWhenIdle,
    /// Leave `sleeping`; a no-op from any other state (never un-pauses,
    /// never cancels a drain).
    Wake,
}

/// A state change actually performed (a no-op returns `None` instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Transition {
    pub from: BroadcastState,
    pub to: BroadcastState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error("{0}")]
    Refused(String),
    #[error("invalid override: {0}")]
    InvalidOverride(String),
}

/// What the air (Liquidsoap control) is told about. See `ls_control`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirEvent {
    /// A broadcast state change (pause / resume / stop act on the air).
    Transition(Transition),
    /// An override was queued: `soft` → drop the prepared track so it airs
    /// at the next boundary; `hard` → cut the current track now.
    Override { id: u64, mode: OverrideMode },
    /// An `AtClock` hard rendez-vous is due at `at` (sent by the ticker,
    /// `ls_control::spawn_at_clock_ticker`): cut it in now if it still must.
    HardMark { at: Epoch },
    /// End the live: disconnect the DJ from the harbor (silence, or
    /// `stationctl live kick`). The return to the programme follows through
    /// the harbor's own disconnection hook.
    LiveKick,
}

/// What `next_media` may do at a track boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Play,
    Halt(BroadcastState),
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

/// What an override plays: a concrete media (path under the media root, not
/// necessarily indexed — checked on disk when it airs) or a playlist ref
/// (resolved like a grid source when it airs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideContent {
    Media(String),
    Playlist(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideMode {
    /// Inserted at the next track boundary. Honoured now.
    #[default]
    Soft,
    /// Cut the current track now. Needs Liquidsoap (and a running station):
    /// otherwise accepted and degraded to `soft`, with a warning.
    Hard,
}

/// A push request, from the CLI or a plugin (also the WASM JSON shape).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideRequest {
    pub content: OverrideContent,
    #[serde(default)]
    pub mode: OverrideMode,
    /// Staleness window from the push instant ("5m"). Absent = never stale.
    #[serde(default)]
    pub expiry: Option<String>,
    /// Playlist overrides only: how many tracks it holds the air (default 1).
    #[serde(default)]
    pub tracks: Option<u32>,
}

/// A pending override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideEntry {
    pub id: u64,
    pub content: OverrideContent,
    /// As requested. A degraded `Hard` is played as `soft`.
    pub mode: OverrideMode,
    /// Who pushed it: a plugin name, or `cli`.
    pub source: String,
    pub pushed_at: Epoch,
    pub expires_at: Option<Epoch>,
    /// Tracks left (media = 1).
    pub remaining: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PushOutcome {
    pub id: u64,
    /// A `hard` was requested and degraded to `soft` (no Liquidsoap wired,
    /// or the station is paused/sleeping).
    pub degraded: bool,
    /// Pending overrides after the push.
    pub pending: usize,
}

/// Cap on remembered grid incidents (one per rule and kind, the latest).
pub const MAX_INCIDENTS: usize = 64;

/// What went wrong in the grid, as the engine saw it (never a sentence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncidentKind {
    /// A hard rendez-vous was due but its source produced nothing: no cut.
    HardNotCut,
    /// A grid source was due but produced nothing: the engine fell through
    /// to a lower priority (empty pool, or emptied by the constraints).
    SourceEmpty,
}

/// A grid incident: which rule, which playlist, when (latest), how often.
/// Kept in memory for the on-air view: an operator must SEE that a rule
/// airs nothing, not find it in the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub kind: IncidentKind,
    pub rule_id: Option<String>,
    pub playlist_ref: String,
    /// `AtClockHard`, `Every`, `DayPart`…
    pub origin: String,
    /// Latest occurrence (epoch UTC).
    pub at: Epoch,
    /// First occurrence of this run of the same incident.
    pub first_at: Epoch,
    pub count: u32,
}

// ---------------------------------------------------------------------------
// The control handle
// ---------------------------------------------------------------------------

struct Inner {
    state: BroadcastState,
    /// Last listener sample (count, instant). `None` = never sampled: a
    /// draining station then never stops on its own (no audience signal).
    listeners: Option<(u32, Epoch)>,
    overrides: VecDeque<OverrideEntry>,
    next_id: u64,
    /// Manual clock override (testing). Shared with `GridEngine`.
    clock: Option<Epoch>,
    /// DJ on air (harbor), if any: the live holds the air above everything,
    /// so no hard cut can happen meanwhile (`live::LiveHub` sets it).
    live: Option<String>,
    /// Set on `sleeping → running`: the grid engine releases a held group
    /// before resolving (the wake airs the slot of NOW, not a cycle cut
    /// long ago). Taken once (`take_woken`).
    woken: bool,
    /// The operator is stopping stationd (`Station.Shutdown`): from now on
    /// every track boundary answers `halted`, so Liquidsoap keeps the noise
    /// rather than preparing a track stationd will not be there to report.
    /// In memory only: the stop itself is the marker file.
    stopping: bool,
    /// Grid incidents, one entry per (kind, rule), latest last.
    incidents: VecDeque<Incident>,
}

/// Cheap, clonable handle. One per station; shared by the grid engine, the
/// gRPC services and the plugin host surface.
#[derive(Clone)]
pub struct StationControl {
    inner: Arc<Mutex<Inner>>,
    plugins: Arc<OnceLock<PluginHandle>>,
    /// Transitions forwarded to the air (Liquidsoap control socket): pause /
    /// resume act NOW on the air, whoever made them. Set once when wired.
    air: Arc<OnceLock<mpsc::UnboundedSender<AirEvent>>>,
    /// Ordered persistence of state changes (single consumer → writes land in
    /// order). `None` = in-memory only (tests).
    persist: Option<mpsc::UnboundedSender<(BroadcastState, Epoch)>>,
    /// Change counters for the on-air view (`onair`): who shows the air
    /// learns WHEN to look again, never what changed.
    revs: Arc<Revisions>,
}

/// Two change counters, bumped by whoever changes the air (`bump_air`: a
/// track boundary, a state change, an override, a live, an apply…) or only
/// what surrounds it (`bump_meta`: the audience). The on-air view re-runs its
/// simulation of what comes next on `air` only.
struct Revisions {
    air: watch::Sender<u64>,
    meta: watch::Sender<u64>,
}

impl StationControl {
    /// In-memory control starting `running` (tests, or a `GridEngine` built
    /// without an explicit control).
    pub fn new_in_memory() -> Self {
        Self::with_state(BroadcastState::Running, None)
    }

    fn with_state(
        state: BroadcastState,
        persist: Option<mpsc::UnboundedSender<(BroadcastState, Epoch)>>,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                state,
                listeners: None,
                overrides: VecDeque::new(),
                next_id: 1,
                clock: None,
                live: None,
                woken: false,
                stopping: false,
                incidents: VecDeque::new(),
            })),
            plugins: Arc::new(OnceLock::new()),
            air: Arc::new(OnceLock::new()),
            persist,
            revs: Arc::new(Revisions { air: watch::Sender::new(0), meta: watch::Sender::new(0) }),
        }
    }

    /// A detached copy for the on-air simulation: the pending overrides (same
    /// ids, same expiry), the last audience sample, the station `running`;
    /// no persistence, no air, no plugins, no manual clock, no live, and its
    /// own change counters — acting on it never touches the real station.
    pub fn simulation_copy(&self) -> StationControl {
        let copy = Self::with_state(BroadcastState::Running, None);
        {
            let src = self.lock();
            let mut g = copy.lock();
            g.overrides = src.overrides.clone();
            g.next_id = src.next_id;
            g.listeners = src.listeners;
        }
        copy
    }

    // ----- grid incidents (on-air view) ------------------------------------

    /// Remember a grid incident. The same (kind, rule) again updates its
    /// entry (latest instant, count) rather than piling up: an `every` due
    /// with an empty pool falls through at every boundary.
    pub fn record_incident(
        &self,
        kind: IncidentKind,
        rule_id: Option<&str>,
        playlist_ref: &str,
        origin: &str,
        at: Epoch,
    ) {
        {
            let mut g = self.lock();
            let same = |i: &Incident| i.kind == kind && i.rule_id.as_deref() == rule_id && i.playlist_ref == playlist_ref;
            let mut entry = match g.incidents.iter().position(same) {
                Some(pos) => g.incidents.remove(pos).expect("position is valid"),
                None => Incident {
                    kind,
                    rule_id: rule_id.map(str::to_string),
                    playlist_ref: playlist_ref.to_string(),
                    origin: origin.to_string(),
                    at,
                    first_at: at,
                    count: 0,
                },
            };
            entry.at = at;
            entry.count += 1;
            g.incidents.push_back(entry);
            while g.incidents.len() > MAX_INCIDENTS {
                g.incidents.pop_front();
            }
        }
        self.bump_meta();
    }

    /// Incidents whose latest occurrence is at or after `since`, oldest first.
    pub fn incidents_since(&self, since: Epoch) -> Vec<Incident> {
        self.lock().incidents.iter().filter(|i| i.at.0 >= since.0).cloned().collect()
    }

    // ----- change counters (on-air view) ----------------------------------

    /// Something that may change what airs next happened.
    pub fn bump_air(&self) {
        self.revs.air.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Something around the air changed (the audience), not what comes next.
    pub fn bump_meta(&self) {
        self.revs.meta.send_modify(|v| *v = v.wrapping_add(1));
    }

    pub fn watch_air(&self) -> watch::Receiver<u64> {
        self.revs.air.subscribe()
    }

    pub fn watch_meta(&self) -> watch::Receiver<u64> {
        self.revs.meta.subscribe()
    }

    /// Load the persisted broadcast state (absent row → `running`) and start
    /// the ordered writer. An unreadable stored value is a loud error.
    pub async fn load(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state FROM broadcast_state WHERE id = 1")
                .fetch_optional(&pool)
                .await?;
        let state = match row {
            None => BroadcastState::Running,
            Some((s,)) => BroadcastState::parse(&s).ok_or_else(|| {
                sqlx::Error::Decode(format!("unknown broadcast_state `{s}`").into())
            })?,
        };
        if state != BroadcastState::Running {
            tracing::warn!(state = state.as_str(), "broadcast state restored from the last run");
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<(BroadcastState, Epoch)>();
        tokio::spawn(async move {
            while let Some((state, at)) = rx.recv().await {
                let res = sqlx::query(
                    "INSERT INTO broadcast_state (id, state, updated_at) VALUES (1, ?1, ?2) \
                     ON CONFLICT(id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
                )
                .bind(state.as_str())
                .bind(at.0)
                .execute(&pool)
                .await;
                if let Err(e) = res {
                    tracing::error!(%e, state = state.as_str(), "could not persist broadcast state");
                }
            }
        });
        Ok(Self::with_state(state, Some(tx)))
    }

    /// Wire the plugin system so state changes / samples are broadcast as
    /// events. Set once (after the plugin actor is spawned with this control).
    pub fn attach_plugins(&self, handle: PluginHandle) {
        let _ = self.plugins.set(handle);
    }

    /// Wire the air (Liquidsoap control): every transition is forwarded, the
    /// receiver decides what acts on the air. Set once.
    pub fn attach_air(&self, tx: mpsc::UnboundedSender<AirEvent>) {
        let _ = self.air.set(tx);
    }

    fn to_air(&self, ev: AirEvent) {
        if let Some(tx) = self.air.get() {
            let _ = tx.send(ev);
        }
    }

    /// Emit a plugin event (best-effort; nothing when no plugin is wired).
    pub fn emit_event(&self, event: PluginEvent) {
        self.emit(event);
    }

    /// Forward an air event (nothing when Liquidsoap is not wired).
    pub fn send_air(&self, ev: AirEvent) {
        self.to_air(ev);
    }

    // ----- live -----------------------------------------------------------

    /// A DJ took the air (`Some`) or gave it back (`None`). A DJ taking the
    /// air wakes a sleeping station (core rule).
    pub fn set_live(&self, dj: Option<String>) {
        let on_air = dj.is_some();
        self.lock().live = dj;
        if on_air {
            self.wake_if_sleeping("live");
        }
        self.bump_air();
    }

    /// The DJ on air, if any.
    pub fn live_dj(&self) -> Option<String> {
        self.lock().live.clone()
    }

    fn emit(&self, event: PluginEvent) {
        if let Some(h) = self.plugins.get() {
            h.emit(event);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a panic mid-update of plain data; the data
        // itself is still coherent (every update is a single assignment).
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ----- clock --------------------------------------------------------

    /// Freeze (`Some`) or release (`None`) the manual clock.
    pub fn set_clock(&self, frozen: Option<Epoch>) {
        self.lock().clock = frozen;
        self.bump_air();
    }

    pub fn clock_override(&self) -> Option<Epoch> {
        self.lock().clock
    }

    /// The station's effective now: manual clock if frozen, else wall time.
    pub fn now(&self) -> Epoch {
        self.clock_override().unwrap_or_else(wall_now)
    }

    // ----- broadcast state ----------------------------------------------

    pub fn state(&self) -> BroadcastState {
        self.lock().state
    }

    /// Apply a control action. `by` names the emitter (plugin name / `cli`).
    /// Returns the transition, or `None` when already in the target state.
    pub fn apply(&self, action: ControlAction, by: &str) -> Result<Option<Transition>, ControlError> {
        use BroadcastState::*;
        let transition = {
            let mut g = self.lock();
            let from = g.state;
            let to = match (action, from) {
                (ControlAction::Resume, _) => Running,
                (ControlAction::Pause, Running | Draining | Paused) => Paused,
                (ControlAction::Pause, Sleeping) => {
                    return Err(ControlError::Refused(
                        "cannot pause a sleeping station (resume first)".into(),
                    ))
                }
                (ControlAction::StopWhenIdle, Running | Draining) => Draining,
                (ControlAction::StopWhenIdle, Sleeping) => Sleeping,
                (ControlAction::StopWhenIdle, Paused) => {
                    return Err(ControlError::Refused(
                        "cannot arm stop-when-idle on a paused station (resume first)".into(),
                    ))
                }
                (ControlAction::Wake, Sleeping) => Running,
                (ControlAction::Wake, s) => s,
            };
            if to == from {
                return Ok(None);
            }
            g.state = to;
            if from == Sleeping && to == Running {
                g.woken = true;
            }
            Transition { from, to }
        };
        self.changed(transition, by);
        Ok(Some(transition))
    }

    /// Core wake rule: `sleeping → running`, signed `by`. No-op otherwise.
    fn wake_if_sleeping(&self, by: &str) {
        let t = {
            let mut g = self.lock();
            if g.state != BroadcastState::Sleeping {
                return;
            }
            g.state = BroadcastState::Running;
            g.woken = true;
            Transition { from: BroadcastState::Sleeping, to: BroadcastState::Running }
        };
        self.changed(t, by);
    }

    /// stationd is being stopped by the operator: nothing is resolved any
    /// more (every boundary halts). Irreversible for this process.
    pub fn begin_operator_stop(&self) {
        self.lock().stopping = true;
        self.bump_air();
    }

    /// The station woke since the last call (`sleeping → running`): the grid
    /// engine then releases a held group before resolving.
    pub fn take_woken(&self) -> bool {
        std::mem::take(&mut self.lock().woken)
    }

    /// Test helper: put the station to sleep right away (the real path is
    /// `stop_when_idle` + a zero sample + a track boundary).
    #[cfg(test)]
    pub fn sleep_now(&self) {
        let t = {
            let mut g = self.lock();
            let from = g.state;
            g.state = BroadcastState::Sleeping;
            Transition { from, to: BroadcastState::Sleeping }
        };
        self.changed(t, "test");
    }

    fn changed(&self, t: Transition, by: &str) {
        let at = self.now();
        tracing::info!(from = t.from.as_str(), to = t.to.as_str(), by, "broadcast state changed");
        if let Some(tx) = &self.persist {
            let _ = tx.send((t.to, at));
        }
        self.to_air(AirEvent::Transition(t));
        self.bump_air();
        self.emit(PluginEvent::BroadcastStateChanged {
            from: t.from.as_str().to_string(),
            to: t.to.as_str().to_string(),
            by: by.to_string(),
        });
    }

    /// Record an audience sample (the Icecast sampler, or `stationctl debug
    /// listeners`) and notify plugins. Never stops anything by itself: a drain
    /// completes only at a track boundary (`gate`).
    pub fn sample_listeners(&self, count: u32) {
        let at = self.now();
        self.lock().listeners = Some((count, at));
        self.bump_meta();
        self.emit(PluginEvent::ListenersSampled { count, at: at.0 });
    }

    /// The audience became unknown (Icecast unreachable, one of our mounts has
    /// no source, unreadable stats): forget the last sample. A draining
    /// station then keeps playing and a sleeping one wakes — a failure is
    /// never read as « 0 listeners ». No event: `ListenersSampled` carries
    /// facts, not their absence (the error is visible in `stationctl icecast
    /// status`). Returns `true` when a sample was actually forgotten.
    pub fn clear_listeners(&self) -> bool {
        let forgot = self.lock().listeners.take().is_some();
        self.wake_if_sleeping("audience-unknown");
        if forgot {
            self.bump_meta();
        }
        forgot
    }

    /// Last listener count, if ever sampled (and not forgotten since).
    pub fn listeners(&self) -> Option<u32> {
        self.lock().listeners.map(|(c, _)| c)
    }

    /// Called at a track boundary, before anything is resolved. `draining`
    /// with a last sample of 0 goes to sleep here.
    pub fn gate(&self) -> Gate {
        let completed = {
            let mut g = self.lock();
            if g.stopping {
                // Reported as `sleeping` to Liquidsoap: noise, not a track.
                return Gate::Halt(BroadcastState::Sleeping);
            }
            match g.state {
                BroadcastState::Running => return Gate::Play,
                s @ (BroadcastState::Paused | BroadcastState::Sleeping) => return Gate::Halt(s),
                BroadcastState::Draining => {
                    if g.listeners.map(|(c, _)| c) == Some(0) {
                        g.state = BroadcastState::Sleeping;
                        Transition { from: BroadcastState::Draining, to: BroadcastState::Sleeping }
                    } else {
                        return Gate::Play;
                    }
                }
            }
        };
        self.changed(completed, "stop-when-idle");
        Gate::Halt(BroadcastState::Sleeping)
    }

    // ----- overrides ----------------------------------------------------

    /// Queue an override. Validates the content shape (safe media path,
    /// normalized playlist ref, tracks, expiry) — existence is checked when it
    /// airs. Refused past [`MAX_PENDING_OVERRIDES`].
    pub fn push_override(
        &self,
        req: OverrideRequest,
        source: &str,
    ) -> Result<PushOutcome, ControlError> {
        let bad = |m: String| ControlError::InvalidOverride(m);
        let (content, remaining) = match req.content {
            OverrideContent::Media(p) => {
                if matches!(req.tracks, Some(n) if n != 1) {
                    return Err(bad("`tracks` applies to a playlist override, not a media".into()));
                }
                (OverrideContent::Media(normalize_media_ref(&p).map_err(bad)?), 1)
            }
            OverrideContent::Playlist(r) => {
                let key = crate::playlist::normalize_ref(&r).map_err(bad)?;
                let n = req.tracks.unwrap_or(1);
                if n == 0 {
                    return Err(bad("`tracks` must be ≥ 1".into()));
                }
                (OverrideContent::Playlist(key), n)
            }
        };
        let now = self.now();
        let expires_at = match &req.expiry {
            None => None,
            Some(d) => {
                let secs = crate::playlist::parse_duration_secs(d)
                    .map_err(|e| bad(format!("expiry: {e}")))?;
                Some(Epoch(now.0.saturating_add(secs as i64)))
            }
        };
        // A hard cut needs the air wired AND a station on air: a halted
        // (paused / sleeping) station keeps it queued; it plays as soft once
        // it airs again. A live DJ
        // holds the air above any cut: soft too, after the live.
        let air_live = self.air.get().is_some()
            && matches!(self.state(), BroadcastState::Running | BroadcastState::Draining)
            && self.live_dj().is_none();
        let degraded = req.mode == OverrideMode::Hard && !air_live;
        let outcome = {
            let mut g = self.lock();
            if g.overrides.len() >= MAX_PENDING_OVERRIDES {
                return Err(ControlError::Refused(format!(
                    "override queue full ({MAX_PENDING_OVERRIDES} pending)"
                )));
            }
            let id = g.next_id;
            g.next_id += 1;
            g.overrides.push_back(OverrideEntry {
                id,
                content,
                mode: req.mode,
                source: source.to_string(),
                pushed_at: now,
                expires_at,
                remaining,
            });
            PushOutcome { id, degraded, pending: g.overrides.len() }
        };
        if degraded {
            tracing::warn!(
                id = outcome.id,
                source,
                "hard override without a live air (no Liquidsoap, station halted, or a DJ on air): degraded to soft"
            );
        } else {
            tracing::info!(id = outcome.id, source, mode = ?req.mode, "override queued");
        }
        let mode = if degraded { OverrideMode::Soft } else { req.mode };
        self.to_air(AirEvent::Override { id: outcome.id, mode });
        self.bump_air();
        Ok(outcome)
    }

    /// Snapshot of the pending overrides, in play order.
    pub fn list_overrides(&self) -> Vec<OverrideEntry> {
        self.lock().overrides.iter().cloned().collect()
    }

    /// Remove one override (`Some(id)`) or all (`None`). Returns how many.
    pub fn clear_overrides(&self, id: Option<u64>) -> usize {
        let removed = {
            let mut g = self.lock();
            let before = g.overrides.len();
            match id {
                Some(id) => g.overrides.retain(|e| e.id != id),
                None => g.overrides.clear(),
            }
            before - g.overrides.len()
        };
        if removed > 0 {
            self.bump_air();
        }
        removed
    }

    /// The override to air at `now`: stale entries are dropped first (each
    /// one logged — a missed override is abandoned, never replayed late).
    pub fn next_override(&self, now: Epoch) -> Option<OverrideEntry> {
        let mut g = self.lock();
        g.overrides.retain(|e| match e.expires_at {
            Some(exp) if now > exp => {
                tracing::warn!(id = e.id, source = %e.source, "override expired before airing; abandoned");
                false
            }
            _ => true,
        });
        g.overrides.front().cloned()
    }

    /// Override `id` itself (a hard cut airs it out of queue order), if still
    /// pending and not stale (a stale one is dropped, logged).
    pub fn override_by_id(&self, id: u64, now: Epoch) -> Option<OverrideEntry> {
        let mut g = self.lock();
        let pos = g.overrides.iter().position(|e| e.id == id)?;
        if let Some(exp) = g.overrides[pos].expires_at {
            if now > exp {
                let e = g.overrides.remove(pos).expect("position is valid");
                tracing::warn!(id, source = %e.source, "override expired before airing; abandoned");
                return None;
            }
        }
        Some(g.overrides[pos].clone())
    }

    /// One track of override `id` aired: decrement, drop when exhausted.
    pub fn consume_override(&self, id: u64) {
        {
            let mut g = self.lock();
            if let Some(pos) = g.overrides.iter().position(|e| e.id == id) {
                let left = {
                    let e = &mut g.overrides[pos];
                    e.remaining = e.remaining.saturating_sub(1);
                    e.remaining
                };
                if left == 0 {
                    g.overrides.remove(pos);
                }
            }
        }
        self.bump_air();
    }

    /// Override `id` could not air (missing file, empty pool, bad ref): drop
    /// it, loudly. The grid takes over — never a silent gap.
    pub fn drop_override(&self, id: u64, reason: &str) {
        {
            let mut g = self.lock();
            if let Some(pos) = g.overrides.iter().position(|e| e.id == id) {
                let e = g.overrides.remove(pos).expect("position is valid");
                tracing::warn!(id, source = %e.source, reason, "override could not air; dropped");
            }
        }
        self.bump_air();
    }
}

/// A media path pushed by an override: relative to the media root, `/`
/// separators, case kept. Absolute paths, drive letters, `..`, NUL → refused.
pub fn normalize_media_ref(raw: &str) -> Result<String, String> {
    let unified = raw.replace('\\', "/");
    if unified.contains('\0') {
        return Err(format!("media path {raw:?} contains NUL"));
    }
    let bytes = unified.as_bytes();
    if unified.starts_with('/') || (bytes.len() >= 2 && bytes[1] == b':') {
        return Err(format!("media path `{raw}` must be relative to the media root"));
    }
    let mut segments = Vec::new();
    for seg in unified.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return Err(format!("media path `{raw}` must not contain `..`")),
            s => segments.push(s),
        }
    }
    if segments.is_empty() {
        return Err(format!("media path `{raw}` is empty"));
    }
    Ok(segments.join("/"))
}

fn wall_now() -> Epoch {
    use std::time::{SystemTime, UNIX_EPOCH};
    Epoch(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use BroadcastState::*;

    fn media(p: &str) -> OverrideRequest {
        OverrideRequest {
            content: OverrideContent::Media(p.into()),
            mode: OverrideMode::Soft,
            expiry: None,
            tracks: None,
        }
    }

    #[test]
    fn transitions_follow_the_state_machine() {
        let c = StationControl::new_in_memory();
        assert_eq!(c.state(), Running);
        let t = c.apply(ControlAction::Pause, "cli").unwrap().unwrap();
        assert_eq!((t.from, t.to), (Running, Paused));
        assert!(c.apply(ControlAction::Pause, "cli").unwrap().is_none(), "no-op");
        assert!(c.apply(ControlAction::StopWhenIdle, "cli").is_err(), "paused → refused");
        c.apply(ControlAction::Resume, "cli").unwrap();
        c.sleep_now();
        assert!(c.apply(ControlAction::Pause, "cli").is_err(), "sleeping → refused");
        assert!(c.apply(ControlAction::StopWhenIdle, "cli").unwrap().is_none(), "already asleep");
        c.apply(ControlAction::Resume, "cli").unwrap();
        assert_eq!(c.state(), Running);
    }

    #[test]
    fn wake_only_leaves_sleeping() {
        let c = StationControl::new_in_memory();
        assert!(c.apply(ControlAction::Wake, "p").unwrap().is_none(), "running: no-op");
        c.apply(ControlAction::Pause, "cli").unwrap();
        assert!(c.apply(ControlAction::Wake, "p").unwrap().is_none(), "never un-pauses");
        assert_eq!(c.state(), Paused);
        c.apply(ControlAction::Resume, "cli").unwrap();
        c.apply(ControlAction::StopWhenIdle, "cli").unwrap();
        assert!(c.apply(ControlAction::Wake, "p").unwrap().is_none(), "never cancels a drain");
        assert_eq!(c.state(), Draining);
        c.sleep_now();
        let t = c.apply(ControlAction::Wake, "p").unwrap().unwrap();
        assert_eq!((t.from, t.to), (Sleeping, Running));
        assert!(c.take_woken(), "the engine is told once");
        assert!(!c.take_woken());
    }

    #[test]
    fn resume_from_pause_is_not_a_wake() {
        let c = StationControl::new_in_memory();
        c.apply(ControlAction::Pause, "cli").unwrap();
        c.apply(ControlAction::Resume, "cli").unwrap();
        assert!(!c.take_woken(), "a pause is a freeze: the held group stays");
        c.sleep_now();
        c.apply(ControlAction::Resume, "cli").unwrap();
        assert!(c.take_woken(), "an operator's resume from sleep is a wake too");
    }

    #[test]
    fn an_unknown_audience_wakes_a_sleeping_station() {
        let c = StationControl::new_in_memory();
        c.sleep_now();
        // Restart in sleeping: nothing sampled yet, stays asleep.
        assert_eq!(c.listeners(), None);
        assert_eq!(c.gate(), Gate::Halt(Sleeping));
        c.sample_listeners(0);
        assert_eq!(c.state(), Sleeping);
        // Icecast becomes unreadable: wake (never read as « no one »).
        c.clear_listeners();
        assert_eq!(c.state(), Running);
        assert!(c.take_woken());
        // Not sleeping: nothing changes.
        c.clear_listeners();
        assert_eq!(c.state(), Running);
    }

    #[test]
    fn a_dj_taking_the_air_wakes_a_sleeping_station() {
        let c = StationControl::new_in_memory();
        c.sleep_now();
        c.set_live(Some("dj".into()));
        assert_eq!(c.state(), Running);
        c.set_live(None);
        c.apply(ControlAction::Pause, "cli").unwrap();
        c.set_live(Some("dj".into()));
        assert_eq!(c.state(), Paused, "a live never un-pauses");
    }

    #[test]
    fn an_operator_stop_halts_every_boundary() {
        let c = StationControl::new_in_memory();
        assert_eq!(c.gate(), Gate::Play);
        c.begin_operator_stop();
        assert_eq!(c.gate(), Gate::Halt(Sleeping));
        assert_eq!(c.state(), Running, "not a broadcast state: nothing persisted");
    }

    #[test]
    fn stopped_is_no_longer_a_state() {
        assert_eq!(BroadcastState::parse("stopped"), None);
        assert_eq!(BroadcastState::parse("sleeping"), Some(Sleeping));
        assert!(serde_json::from_str::<ControlAction>(r#""stop""#).is_err());
        assert_eq!(serde_json::from_str::<ControlAction>(r#""wake""#).unwrap(), ControlAction::Wake);
    }

    #[test]
    fn gate_halts_when_paused_or_stopped() {
        let c = StationControl::new_in_memory();
        assert_eq!(c.gate(), Gate::Play);
        c.apply(ControlAction::Pause, "cli").unwrap();
        assert_eq!(c.gate(), Gate::Halt(Paused));
        c.sleep_now();
        assert_eq!(c.gate(), Gate::Halt(Sleeping));
    }

    #[test]
    fn draining_stops_only_at_a_boundary_with_zero_listeners() {
        let c = StationControl::new_in_memory();
        c.apply(ControlAction::StopWhenIdle, "cli").unwrap();
        assert_eq!(c.state(), Draining);
        // Never sampled → no audience signal → keeps playing.
        assert_eq!(c.gate(), Gate::Play);
        c.sample_listeners(3);
        assert_eq!(c.gate(), Gate::Play);
        // A zero sample does not stop by itself…
        c.sample_listeners(0);
        assert_eq!(c.state(), Draining);
        // …the next track boundary does.
        assert_eq!(c.gate(), Gate::Halt(Sleeping));
        assert_eq!(c.state(), Sleeping);
    }

    #[test]
    fn an_unknown_audience_never_completes_a_drain() {
        let c = StationControl::new_in_memory();
        c.apply(ControlAction::StopWhenIdle, "cli").unwrap();
        c.sample_listeners(0);
        // Icecast becomes unreachable before the boundary: the stale zero
        // is forgotten, the station keeps playing.
        assert!(c.clear_listeners());
        assert_eq!(c.listeners(), None);
        assert_eq!(c.gate(), Gate::Play);
        assert_eq!(c.state(), Draining);
        assert!(!c.clear_listeners(), "already unknown");
        // A fresh zero sample completes it again.
        c.sample_listeners(0);
        assert_eq!(c.gate(), Gate::Halt(Sleeping));
    }

    #[test]
    fn resume_cancels_a_drain() {
        let c = StationControl::new_in_memory();
        c.apply(ControlAction::StopWhenIdle, "cli").unwrap();
        c.sample_listeners(0);
        c.apply(ControlAction::Resume, "cli").unwrap();
        assert_eq!(c.gate(), Gate::Play);
    }

    #[test]
    fn override_media_path_is_normalized_and_confined() {
        assert_eq!(normalize_media_ref("news\\flash.mp3").unwrap(), "news/flash.mp3");
        assert_eq!(normalize_media_ref("./News//Flash.mp3").unwrap(), "News/Flash.mp3");
        for bad in ["/etc/passwd", "C:/x.mp3", "../x.mp3", "a/../../x", "", "./"] {
            assert!(normalize_media_ref(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn push_validates_and_queues_in_order() {
        let c = StationControl::new_in_memory();
        let a = c.push_override(media("a.mp3"), "cli").unwrap();
        let b = c
            .push_override(
                OverrideRequest {
                    content: OverrideContent::Playlist("Shows/Urgence".into()),
                    mode: OverrideMode::Soft,
                    expiry: Some("5m".into()),
                    tracks: Some(2),
                },
                "plugin-x",
            )
            .unwrap();
        assert_eq!((a.pending, b.pending), (1, 2));
        let list = c.list_overrides();
        assert_eq!(list[0].content, OverrideContent::Media("a.mp3".into()));
        assert_eq!(list[1].content, OverrideContent::Playlist("shows/urgence".into()));
        assert_eq!(list[1].remaining, 2);
        assert_eq!(list[1].source, "plugin-x");
        // Invalid shapes.
        let mut r = media("a.mp3");
        r.tracks = Some(3);
        assert!(c.push_override(r, "cli").is_err(), "tracks on a media");
        let mut r = media("a.mp3");
        r.expiry = Some("5".into());
        assert!(c.push_override(r, "cli").is_err(), "bad expiry");
        assert!(c.push_override(media("../x"), "cli").is_err());
    }

    #[test]
    fn hard_is_accepted_but_degraded() {
        let c = StationControl::new_in_memory();
        let mut r = media("a.mp3");
        r.mode = OverrideMode::Hard;
        let out = c.push_override(r, "cli").unwrap();
        assert!(out.degraded);
        assert_eq!(c.list_overrides()[0].mode, OverrideMode::Hard);
    }

    #[test]
    fn with_the_air_wired_hard_cuts_unless_halted() {
        let c = StationControl::new_in_memory();
        let (tx, mut rx) = mpsc::unbounded_channel();
        c.attach_air(tx);
        let hard = || {
            let mut r = media("a.mp3");
            r.mode = OverrideMode::Hard;
            r
        };
        let out = c.push_override(hard(), "cli").unwrap();
        assert!(!out.degraded, "live air: a real cut");
        assert_eq!(rx.try_recv().unwrap(), AirEvent::Override { id: out.id, mode: OverrideMode::Hard });
        let soft = c.push_override(media("b.mp3"), "cli").unwrap();
        assert_eq!(rx.try_recv().unwrap(), AirEvent::Override { id: soft.id, mode: OverrideMode::Soft });
        // paused: nothing to cut — queued, played as soft once resumed
        c.apply(ControlAction::Pause, "cli").unwrap();
        let _ = rx.try_recv(); // the transition
        let out = c.push_override(hard(), "cli").unwrap();
        assert!(out.degraded);
        assert_eq!(rx.try_recv().unwrap(), AirEvent::Override { id: out.id, mode: OverrideMode::Soft });
    }

    #[test]
    fn override_by_id_skips_queue_order_and_expiry() {
        let c = StationControl::new_in_memory();
        let a = c.push_override(media("a.mp3"), "cli").unwrap();
        let b = c.push_override(media("b.mp3"), "cli").unwrap();
        assert_eq!(c.override_by_id(b.id, c.now()).unwrap().id, b.id);
        assert!(c.override_by_id(999, c.now()).is_none());
        let mut r = media("c.mp3");
        r.expiry = Some("1s".into());
        let e = c.push_override(r, "cli").unwrap();
        assert!(c.override_by_id(e.id, Epoch(c.now().0 + 10)).is_none(), "stale: dropped");
        assert_eq!(c.list_overrides().iter().map(|o| o.id).collect::<Vec<_>>(), [a.id, b.id]);
    }

    #[test]
    fn expired_overrides_are_abandoned() {
        let c = StationControl::new_in_memory();
        c.set_clock(Some(Epoch(1000)));
        let mut r = media("late.mp3");
        r.expiry = Some("1m".into());
        c.push_override(r, "cli").unwrap();
        c.push_override(media("fresh.mp3"), "cli").unwrap();
        // Within the window: the first one airs.
        assert_eq!(
            c.next_override(Epoch(1060)).unwrap().content,
            OverrideContent::Media("late.mp3".into())
        );
        // Past it: dropped, the next one comes up.
        assert_eq!(
            c.next_override(Epoch(1061)).unwrap().content,
            OverrideContent::Media("fresh.mp3".into())
        );
        assert_eq!(c.list_overrides().len(), 1);
    }

    #[test]
    fn consume_counts_tracks_and_drop_removes() {
        let c = StationControl::new_in_memory();
        let id = c
            .push_override(
                OverrideRequest {
                    content: OverrideContent::Playlist("p".into()),
                    mode: OverrideMode::Soft,
                    expiry: None,
                    tracks: Some(2),
                },
                "cli",
            )
            .unwrap()
            .id;
        c.consume_override(id);
        assert_eq!(c.list_overrides()[0].remaining, 1);
        c.consume_override(id);
        assert!(c.list_overrides().is_empty());
        let id = c.push_override(media("a.mp3"), "cli").unwrap().id;
        c.drop_override(id, "missing");
        assert!(c.list_overrides().is_empty());
    }

    #[test]
    fn queue_is_capped() {
        let c = StationControl::new_in_memory();
        for i in 0..MAX_PENDING_OVERRIDES {
            c.push_override(media(&format!("{i}.mp3")), "cli").unwrap();
        }
        assert!(matches!(
            c.push_override(media("one-more.mp3"), "cli"),
            Err(ControlError::Refused(_))
        ));
        assert_eq!(c.clear_overrides(None), MAX_PENDING_OVERRIDES);
    }

    #[tokio::test]
    async fn state_is_persisted_and_restored() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        let c = StationControl::load(pool.clone()).await.unwrap();
        assert_eq!(c.state(), Running, "fresh install → running");
        c.sleep_now();
        // The writer is async: poll briefly until the row lands.
        for _ in 0..50 {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT state FROM broadcast_state WHERE id = 1")
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            if row.map(|r| r.0) == Some("sleeping".into()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let again = StationControl::load(pool).await.unwrap();
        assert_eq!(again.state(), Sleeping, "an idle stop survives a restart");
    }

    #[tokio::test]
    async fn migration_0021_turns_a_stopped_row_into_sleeping() {
        // Replay 0021 on a pre-0021 table holding the old value.
        // One connection: every `:memory:` connection is its own database.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0014_broadcast_state.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO broadcast_state (id, state, updated_at) VALUES (1, 'stopped', 7)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0021_broadcast_sleeping.sql"))
            .execute(&pool)
            .await
            .unwrap();
        let row: (String, i64) = sqlx::query_as("SELECT state, updated_at FROM broadcast_state WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row, ("sleeping".to_string(), 7));
        // The new CHECK refuses the old value.
        assert!(sqlx::query("UPDATE broadcast_state SET state = 'stopped' WHERE id = 1")
            .execute(&pool)
            .await
            .is_err());
    }

    #[test]
    fn incidents_merge_per_rule_and_kind() {
        let c = StationControl::new_in_memory();
        c.record_incident(IncidentKind::SourceEmpty, Some("jingle"), "jingles", "Every", Epoch(10));
        c.record_incident(IncidentKind::SourceEmpty, Some("jingle"), "jingles", "Every", Epoch(20));
        c.record_incident(IncidentKind::HardNotCut, Some("toph"), "toph", "AtClockHard", Epoch(30));
        let all = c.incidents_since(Epoch(0));
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].count, all[0].first_at, all[0].at), (2, Epoch(10), Epoch(20)));
        assert_eq!(all[1].rule_id.as_deref(), Some("toph"));
        assert_eq!(c.incidents_since(Epoch(25)).len(), 1);
        assert!(c.simulation_copy().incidents_since(Epoch(0)).is_empty(), "a simulation starts clean");
    }
}
