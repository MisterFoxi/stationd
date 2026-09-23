//! Station runtime control — the mechanism behind the plugin host surface (A2)
//! and the first-class `stationctl station …` / `stationctl override …`.
//!
//! « Mécanisme au core, politique au plugin » (Doc/plugin-host.md): this module
//! owns the *capabilities* — the broadcast state machine, the override queue,
//! the last listener sample — and holds no business condition. A plugin (or a
//! human via the CLI) decides *when* to use them.
//!
//! - **Broadcast state** `running | paused | stopped | draining`. `draining`
//!   is an armed graceful stop: it becomes `stopped` at the next track
//!   boundary (`gate`) once the last listener sample is 0. Persisted (family
//!   B, migration 0014) through an ordered writer task, so the synchronous
//!   API stays callable from a plugin hook.
//! - **Override queue**: content pushed ahead of the grid (`next_media`
//!   consults it before `resolve_next`). In memory, capped, with per-entry
//!   expiry (a missed override is dropped, never replayed late). `hard` is
//!   accepted but degraded to `soft` (warned) until Liquidsoap is wired.
//! - **Manual clock** (testing): the frozen instant shared with `GridEngine`,
//!   so override expiry and grid resolution ride the same clock.
//!
//! Every state change emits `BroadcastStateChanged`; every listener sample
//! emits `ListenersSampled` (best-effort, via the plugin handle once
//! attached). The API is synchronous and lock-short: no lock is ever held
//! across an await or an event emission.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::mpsc;

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
    Stopped,
    /// Armed graceful stop: stops at the next clean occasion (track boundary
    /// AND zero listeners). Not a kill.
    Draining,
}

impl BroadcastState {
    pub fn as_str(self) -> &'static str {
        match self {
            BroadcastState::Running => "running",
            BroadcastState::Paused => "paused",
            BroadcastState::Stopped => "stopped",
            BroadcastState::Draining => "draining",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "running" => BroadcastState::Running,
            "paused" => BroadcastState::Paused,
            "stopped" => BroadcastState::Stopped,
            "draining" => BroadcastState::Draining,
            _ => return None,
        })
    }
}

/// A control command, from the CLI or a plugin (`host.control`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Stop,
    Pause,
    Resume,
    StopWhenIdle,
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
    /// Cut/duck the current track. Needs Liquidsoap: until then it is
    /// accepted and degraded to `soft`, with a warning.
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
    /// As requested. A `Hard` is played as `soft` until LS is wired.
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
    /// A `hard` was requested and degraded to `soft` (no Liquidsoap yet).
    pub degraded: bool,
    /// Pending overrides after the push.
    pub pending: usize,
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
}

/// Cheap, clonable handle. One per station; shared by the grid engine, the
/// gRPC services and the plugin host surface.
#[derive(Clone)]
pub struct StationControl {
    inner: Arc<Mutex<Inner>>,
    plugins: Arc<OnceLock<PluginHandle>>,
    /// Ordered persistence of state changes (single consumer → writes land in
    /// order). `None` = in-memory only (tests).
    persist: Option<mpsc::UnboundedSender<(BroadcastState, Epoch)>>,
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
            })),
            plugins: Arc::new(OnceLock::new()),
            persist,
        }
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
                (ControlAction::Stop, _) => Stopped,
                (ControlAction::Resume, _) => Running,
                (ControlAction::Pause, Running | Draining | Paused) => Paused,
                (ControlAction::Pause, Stopped) => {
                    return Err(ControlError::Refused(
                        "cannot pause a stopped station (resume first)".into(),
                    ))
                }
                (ControlAction::StopWhenIdle, Running | Draining) => Draining,
                (ControlAction::StopWhenIdle, Stopped) => Stopped,
                (ControlAction::StopWhenIdle, Paused) => {
                    return Err(ControlError::Refused(
                        "cannot arm stop-when-idle on a paused station (resume first)".into(),
                    ))
                }
            };
            if to == from {
                return Ok(None);
            }
            g.state = to;
            Transition { from, to }
        };
        self.changed(transition, by);
        Ok(Some(transition))
    }

    fn changed(&self, t: Transition, by: &str) {
        let at = self.now();
        tracing::info!(from = t.from.as_str(), to = t.to.as_str(), by, "broadcast state changed");
        if let Some(tx) = &self.persist {
            let _ = tx.send((t.to, at));
        }
        self.emit(PluginEvent::BroadcastStateChanged {
            from: t.from.as_str().to_string(),
            to: t.to.as_str().to_string(),
            by: by.to_string(),
        });
    }

    /// Record an audience sample (Icecast later; test injection today) and
    /// notify plugins. Never stops anything by itself: a drain completes only
    /// at a track boundary (`gate`).
    pub fn sample_listeners(&self, count: u32) {
        let at = self.now();
        self.lock().listeners = Some((count, at));
        self.emit(PluginEvent::ListenersSampled { count, at: at.0 });
    }

    /// Last listener count, if ever sampled.
    pub fn listeners(&self) -> Option<u32> {
        self.lock().listeners.map(|(c, _)| c)
    }

    /// Called at a track boundary, before anything is resolved. `draining`
    /// with a last sample of 0 completes the graceful stop here.
    pub fn gate(&self) -> Gate {
        let completed = {
            let mut g = self.lock();
            match g.state {
                BroadcastState::Running => return Gate::Play,
                s @ (BroadcastState::Paused | BroadcastState::Stopped) => return Gate::Halt(s),
                BroadcastState::Draining => {
                    if g.listeners.map(|(c, _)| c) == Some(0) {
                        g.state = BroadcastState::Stopped;
                        Transition { from: BroadcastState::Draining, to: BroadcastState::Stopped }
                    } else {
                        return Gate::Play;
                    }
                }
            }
        };
        self.changed(completed, "stop-when-idle");
        Gate::Halt(BroadcastState::Stopped)
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
        let degraded = req.mode == OverrideMode::Hard;
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
                "hard override requested without Liquidsoap: degraded to soft (next track boundary)"
            );
        } else {
            tracing::info!(id = outcome.id, source, "override queued");
        }
        Ok(outcome)
    }

    /// Snapshot of the pending overrides, in play order.
    pub fn list_overrides(&self) -> Vec<OverrideEntry> {
        self.lock().overrides.iter().cloned().collect()
    }

    /// Remove one override (`Some(id)`) or all (`None`). Returns how many.
    pub fn clear_overrides(&self, id: Option<u64>) -> usize {
        let mut g = self.lock();
        let before = g.overrides.len();
        match id {
            Some(id) => g.overrides.retain(|e| e.id != id),
            None => g.overrides.clear(),
        }
        before - g.overrides.len()
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

    /// One track of override `id` aired: decrement, drop when exhausted.
    pub fn consume_override(&self, id: u64) {
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

    /// Override `id` could not air (missing file, empty pool, bad ref): drop
    /// it, loudly. The grid takes over — never a silent gap.
    pub fn drop_override(&self, id: u64, reason: &str) {
        let mut g = self.lock();
        if let Some(pos) = g.overrides.iter().position(|e| e.id == id) {
            let e = g.overrides.remove(pos).expect("position is valid");
            tracing::warn!(id, source = %e.source, reason, "override could not air; dropped");
        }
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
        c.apply(ControlAction::Stop, "cli").unwrap();
        assert_eq!(c.state(), Stopped);
        assert!(c.apply(ControlAction::Pause, "cli").is_err(), "stopped → refused");
        assert!(c.apply(ControlAction::StopWhenIdle, "cli").unwrap().is_none(), "already stopped");
        c.apply(ControlAction::Resume, "cli").unwrap();
        assert_eq!(c.state(), Running);
    }

    #[test]
    fn gate_halts_when_paused_or_stopped() {
        let c = StationControl::new_in_memory();
        assert_eq!(c.gate(), Gate::Play);
        c.apply(ControlAction::Pause, "cli").unwrap();
        assert_eq!(c.gate(), Gate::Halt(Paused));
        c.apply(ControlAction::Stop, "cli").unwrap();
        assert_eq!(c.gate(), Gate::Halt(Stopped));
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
        assert_eq!(c.gate(), Gate::Halt(Stopped));
        assert_eq!(c.state(), Stopped);
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
        c.apply(ControlAction::Stop, "cli").unwrap();
        // The writer is async: poll briefly until the row lands.
        for _ in 0..50 {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT state FROM broadcast_state WHERE id = 1")
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            if row.map(|r| r.0) == Some("stopped".into()) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let again = StationControl::load(pool).await.unwrap();
        assert_eq!(again.state(), Stopped, "an operator's stop survives a restart");
    }
}
