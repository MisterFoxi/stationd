//! Liquidsoap bridge (Liquidsoap → stationd), loopback HTTP/JSON.
//!
//! Liquidsoap has no gRPC client, hence this thin adapter ("A" of the A+C
//! decision). Two routes, both behind the shared token:
//!
//! - `POST /ls/v1/next`  — the pull: resolve the next track NOW (broadcast
//!   gate, override queue, grid — `GridEngine::next_media`) and answer
//!   `{kind, uri, state, reason}` (all four always present, empty when not
//!   relevant — Liquidsoap parses a fixed record):
//!   - `file`   → `uri` = `annotate:stationd_rid="N":/abs/path`;
//!   - `halted` → `state` = paused|stopped: Liquidsoap airs the halted noise,
//!     NOT the safety fallback;
//!   - `none`   → `reason` = fallback|pool_empty|stream_unsupported|error:
//!     Liquidsoap airs its safety fallback.
//! - `POST /ls/v1/track` — `{rid, kind}`: what REALLY started airing (the
//!   post-crossfade air chain). A known `rid` = one of our tracks: the station
//!   track counter (`Every` by tracks) advances. `kind` = fallback|halted for
//!   Liquidsoap's own sources.
//!
//! Liquidsoap only reports STARTS. The end of a track is inferred here: a
//! track leaves the air when something else starts (next track, halted
//! noise after a stop, fallback, unknown). The bridge counts the time each of
//! our tracks really spent on air — a pause freezes the count, a resume
//! restarts it — and hands it to the engine when the track leaves
//! (`GridEngine::on_track_left`: played to the end → `unplayed_only` mark of
//! its leaf playlist). A skip, a hard override or a Liquidsoap restart leave
//! a short count: never marked.
//!
//! The bridge itself holds no business logic: resolution is the engine's; the
//! bridge only numbers requests, keeps what is on air for `stationctl ls
//! status`, and forwards the track-start signal.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::grid_engine::{EngineError, GridEngine};
use crate::ls_script::TOKEN_HEADER;
use crate::selection::SelectionError;

/// How many resolved-but-not-yet-started requests are remembered. Liquidsoap
/// prefetches one; a few more cover skips/restarts. Oldest dropped first.
const PENDING_CAP: usize = 16;

/// Reply to `POST /next`. All fields always serialised (fixed record on the
/// Liquidsoap side).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextReply {
    pub kind: String,
    pub uri: String,
    pub state: String,
    pub reason: String,
}

impl NextReply {
    fn file(uri: String) -> Self {
        Self { kind: "file".into(), uri, state: String::new(), reason: String::new() }
    }
    fn halted(state: &str) -> Self {
        Self { kind: "halted".into(), uri: String::new(), state: state.into(), reason: String::new() }
    }
    fn none(reason: &str) -> Self {
        Self { kind: "none".into(), uri: String::new(), state: String::new(), reason: reason.into() }
    }
}

/// Body of `POST /track`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TrackEvent {
    #[serde(default)]
    pub rid: String,
    #[serde(default)]
    pub kind: String,
}

/// A track stationd handed out, not yet reported as started.
#[derive(Debug, Clone)]
struct Pending {
    rid: u64,
    media_path: String,
    playlist_ref: Option<String>,
    /// Leaf playlist that produced it (see `ResolvedDecision::leaf_ref`).
    leaf_ref: Option<String>,
}

/// What Liquidsoap last reported as starting on air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnAirKind {
    /// One of our tracks (known rid).
    Track,
    /// Liquidsoap's safety fallback.
    Fallback,
    /// The halted background noise.
    Halted,
    /// Something we can't place (unknown rid, source without tag).
    Unknown,
}

impl OnAirKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OnAirKind::Track => "track",
            OnAirKind::Fallback => "fallback",
            OnAirKind::Halted => "halted",
            OnAirKind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OnAir {
    pub kind: OnAirKind,
    pub media_path: Option<String>,
    pub playlist_ref: Option<String>,
    /// When this airing began (display; unchanged by a pause / resume).
    pub since: i64,
    /// Leaf playlist of our track (`None` for Liquidsoap's own sources or a
    /// media override).
    pub leaf_ref: Option<String>,
    /// Seconds really on air before `counting_since` (earlier stretches, a
    /// pause in between).
    pub aired_s: i64,
    /// Start of the current on-air stretch; `None` while frozen by a pause.
    pub counting_since: Option<i64>,
}

impl OnAir {
    fn source(kind: OnAirKind, now: i64) -> Self {
        OnAir {
            kind,
            media_path: None,
            playlist_ref: None,
            since: now,
            leaf_ref: None,
            aired_s: 0,
            counting_since: None,
        }
    }

    /// Seconds really on air at `now` (pauses excluded).
    pub fn aired_at(&self, now: i64) -> i64 {
        self.aired_s + self.counting_since.map_or(0, |t| (now - t).max(0))
    }
}

/// One of our tracks leaving the air, judged outside the lock.
#[derive(Debug)]
struct Left {
    media: String,
    leaf: Option<String>,
    aired_s: i64,
}

impl Left {
    /// `Some` for one of our tracks (a file we handed out), else `None`.
    fn of(a: &OnAir, now: i64) -> Option<Left> {
        if a.kind != OnAirKind::Track {
            return None;
        }
        Some(Left { media: a.media_path.clone()?, leaf: a.leaf_ref.clone(), aired_s: a.aired_at(now) })
    }
}

/// The track handed to Liquidsoap and not started yet ("à suivre").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextUp {
    pub rid: u64,
    pub media_path: String,
    pub playlist_ref: Option<String>,
    /// Came from the override queue (already consumed there): a flush must
    /// never drop it, or the override would be lost.
    pub from_override: bool,
}

/// Snapshot for `stationctl ls status`.
#[derive(Debug, Clone, Default)]
pub struct BridgeStatus {
    pub pulls: u64,
    pub last_pull_at: Option<i64>,
    pub last_reply: Option<NextReply>,
    pub on_air: Option<OnAir>,
    /// Liquidsoap prefetches one track: the last `file` handed out, until it
    /// starts. Cleared by a `halted`/`none` reply (nothing queued behind).
    pub next: Option<NextUp>,
    pub tracks_started: u64,
}

#[derive(Debug, Default)]
struct BridgeState {
    next_rid: u64,
    pending: VecDeque<Pending>,
    status: BridgeStatus,
    /// The track that was on air when the halted noise took over: a resume
    /// from pause continues it (frozen, not restarted), so it is on air again.
    before_halt: Option<OnAir>,
}

#[derive(Clone)]
pub struct LsBridge {
    engine: GridEngine,
    /// Absolute media root: Liquidsoap does not share stationd's CWD, so a
    /// relative `library_path` is made absolute once, here.
    media_root: PathBuf,
    inner: Arc<Mutex<BridgeState>>,
}

impl LsBridge {
    pub fn new(engine: GridEngine, media_root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            engine,
            media_root: std::path::absolute(media_root)?,
            inner: Arc::new(Mutex::new(BridgeState { next_rid: 1, ..Default::default() })),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BridgeState> {
        // A poisoned lock only means a panic mid-update of plain counters:
        // keep serving rather than taking the air down.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The pull: resolve now and translate the decision for Liquidsoap.
    pub async fn next(&self) -> NextReply {
        let now = self.engine.effective_now(None);
        let reply = match self.engine.next_media(now).await {
            Ok(r) => {
                if let Some(state) = r.halted {
                    NextReply::halted(state.as_str())
                } else {
                    match r.media_path {
                        Some(url) if r.stream => {
                            // The relay (input.http driven by stationd) is not
                            // wired yet: say so loudly, air the safety net.
                            tracing::warn!(
                                %url,
                                playlist = r.decision.playlist_ref.as_deref().unwrap_or("-"),
                                "remote stream resolved but the Liquidsoap relay is not wired yet: fallback"
                            );
                            NextReply::none("stream_unsupported")
                        }
                        Some(media) => {
                            let (rid, uri) = self.hand_out(
                                &media,
                                r.decision.playlist_ref.clone(),
                                r.leaf_ref.clone(),
                            );
                            self.lock().status.next = Some(NextUp {
                                rid,
                                media_path: media,
                                playlist_ref: r.decision.playlist_ref.clone(),
                                from_override: r.override_source.is_some(),
                            });
                            NextReply::file(uri)
                        }
                        None => NextReply::none("fallback"),
                    }
                }
            }
            Err(EngineError::Selection(SelectionError::PoolEmpty)) => {
                tracing::warn!("every applicable source is empty: Liquidsoap safety fallback");
                NextReply::none("pool_empty")
            }
            Err(e) => {
                tracing::error!(error = %e, "resolution failed for Liquidsoap: safety fallback");
                NextReply::none("error")
            }
        };
        let mut st = self.lock();
        st.status.pulls += 1;
        st.status.last_pull_at = Some(now.0);
        if reply.kind != "file" {
            st.status.next = None;
        }
        st.status.last_reply = Some(reply.clone());
        reply
    }

    /// Number a track handed to Liquidsoap and remember it until it starts:
    /// returns (rid, annotated absolute uri).
    fn hand_out(
        &self,
        media: &str,
        playlist_ref: Option<String>,
        leaf_ref: Option<String>,
    ) -> (u64, String) {
        let mut st = self.lock();
        let rid = st.next_rid;
        st.next_rid += 1;
        let abs = self.media_root.join(media);
        let uri = format!("annotate:stationd_rid=\"{rid}\":{}", abs.to_string_lossy());
        st.pending.push_back(Pending { rid, media_path: media.to_string(), playlist_ref, leaf_ref });
        while st.pending.len() > PENDING_CAP {
            st.pending.pop_front();
        }
        (rid, uri)
    }

    /// A hard override: resolve override `id` now (one track consumed) and
    /// return the uri Liquidsoap interrupts with. `None` = nothing to cut in
    /// with (already aired by a pull, expired, unplayable — each logged).
    pub async fn interrupt_uri(&self, id: u64) -> Option<String> {
        let now = self.engine.effective_now(None);
        match self.engine.air_override_now(id, now).await {
            Ok(Some(r)) => match r.media_path {
                Some(url) if r.stream => {
                    tracing::warn!(id, %url, "hard override resolved to a remote stream: relay not wired yet, not aired");
                    None
                }
                Some(media) => {
                    Some(self.hand_out(&media, r.decision.playlist_ref.clone(), r.leaf_ref.clone()).1)
                }
                None => None,
            },
            Ok(None) => {
                tracing::info!(id, "hard override no longer pending (aired, expired or dropped)");
                None
            }
            Err(e) => {
                tracing::error!(id, error = %e, "hard override could not be resolved");
                None
            }
        }
    }

    /// Liquidsoap reports a track starting on air. Whatever was on air before
    /// leaves it (except a track frozen by a pause, which waits for its
    /// resume): each of our tracks that leaves is judged by the engine
    /// (`on_track_left`) with the time it really spent on air.
    pub async fn track_started(&self, ev: &TrackEvent) {
        let now = self.engine.effective_now(None).0;
        let mut leaving: Vec<Left> = Vec::new();
        let on_air = if !ev.rid.is_empty() {
            let found = ev.rid.parse::<u64>().ok().and_then(|rid| {
                let mut st = self.lock();
                let idx = st.pending.iter().position(|p| p.rid == rid)?;
                st.pending.remove(idx)
            });
            {
                // Something new starts: the track on air (if ours) left, and
                // a track frozen by a pause was abandoned (skip while paused).
                let mut st = self.lock();
                leaving.extend(st.status.on_air.as_ref().and_then(|a| Left::of(a, now)));
                leaving.extend(st.before_halt.take().as_ref().and_then(|a| Left::of(a, now)));
            }
            match found {
                Some(p) => {
                    tracing::info!(media = %p.media_path, rid = p.rid, "on air");
                    // One station track: advances the `Every` track counters.
                    if let Err(e) = self.engine.on_track_completed().await {
                        tracing::error!(error = %e, "could not advance the track counters");
                    }
                    {
                        let mut st = self.lock();
                        st.status.tracks_started += 1;
                        if st.status.next.as_ref().is_some_and(|n| n.rid == p.rid) {
                            st.status.next = None;
                        }
                    }
                    OnAir {
                        kind: OnAirKind::Track,
                        media_path: Some(p.media_path),
                        playlist_ref: p.playlist_ref,
                        since: now,
                        leaf_ref: p.leaf_ref,
                        aired_s: 0,
                        counting_since: Some(now),
                    }
                }
                None => {
                    tracing::warn!(rid = %ev.rid, "Liquidsoap reported an unknown request id");
                    OnAir::source(OnAirKind::Unknown, now)
                }
            }
        } else {
            // The noise / fallback loop: one event per loop — log the
            // transition only.
            let was = self.lock().status.on_air.as_ref().map(|a| a.kind.clone());
            let kind = match ev.kind.as_str() {
                "fallback" => {
                    if was != Some(OnAirKind::Fallback) {
                        tracing::warn!("Liquidsoap safety fallback on air");
                    }
                    OnAirKind::Fallback
                }
                "halted" => {
                    if was != Some(OnAirKind::Halted) {
                        tracing::info!("halted noise on air");
                    }
                    OnAirKind::Halted
                }
                other => {
                    tracing::warn!(kind = %other, "Liquidsoap reported an untagged track");
                    OnAirKind::Unknown
                }
            };
            if was.as_ref() == Some(&kind) {
                return; // same loop continuing: keep `since` = when it began
            }
            let paused = self.engine.control().state()
                == crate::station_control::BroadcastState::Paused;
            let mut st = self.lock();
            if let Some(track) = st.status.on_air.clone().filter(|a| a.kind == OnAirKind::Track) {
                if kind == OnAirKind::Halted && paused {
                    // A pause freezes the track (a resume continues it): stop
                    // counting, keep it aside.
                    st.before_halt = Some(OnAir {
                        aired_s: track.aired_at(now),
                        counting_since: None,
                        ..track
                    });
                } else {
                    // Its end (noise after a stop), or cut (fallback, unknown).
                    leaving.extend(Left::of(&track, now));
                }
            }
            OnAir::source(kind, now)
        };
        self.lock().status.on_air = Some(on_air);
        for left in leaving {
            let at = crate::resolver::Epoch(now);
            if let Err(e) =
                self.engine.on_track_left(&left.media, left.leaf.as_deref(), left.aired_s, at).await
            {
                tracing::error!(media = %left.media, error = %e, "could not record the end of a track");
            }
        }
    }

    /// Liquidsoap acknowledged a resume from pause: the frozen track plays on
    /// (no new track start will be reported for it) — put it back on air.
    pub fn resumed_from_pause(&self) {
        let now = self.engine.effective_now(None).0;
        let mut st = self.lock();
        let halted = st.status.on_air.as_ref().is_some_and(|a| a.kind == OnAirKind::Halted);
        if halted {
            if let Some(mut track) = st.before_halt.take() {
                tracing::info!(media = track.media_path.as_deref().unwrap_or("-"), "resumed: back on air");
                track.counting_since = Some(now); // on-air time counts again
                st.status.on_air = Some(track);
            }
        }
    }

    /// What the air is doing, from the broadcast state and what Liquidsoap
    /// last reported: `playing`, `paused`, `stop armed` (stop-when-idle),
    /// `stopping` (stop requested, the current track plays to its end) or
    /// `stopped` (halted noise on air).
    pub fn air_state(&self) -> &'static str {
        use crate::station_control::BroadcastState::*;
        match self.engine.control().state() {
            Running => "playing",
            Paused => "paused",
            Draining => "stop armed",
            Stopped => {
                let track_on_air = self
                    .lock()
                    .status
                    .on_air
                    .as_ref()
                    .is_some_and(|a| a.kind == OnAirKind::Track);
                if track_on_air {
                    "stopping"
                } else {
                    "stopped"
                }
            }
        }
    }

    /// Is the track Liquidsoap has prepared an override? Then it must not be
    /// flushed (it was consumed from the queue when handed out).
    pub fn prepared_is_override(&self) -> bool {
        self.lock().status.next.as_ref().is_some_and(|n| n.from_override)
    }

    /// The station control the engine answers to (tests / wiring).
    pub fn engine_control(&self) -> crate::station_control::StationControl {
        self.engine.control().clone()
    }

    pub fn status(&self) -> BridgeStatus {
        self.lock().status.clone()
    }
}

#[derive(Clone)]
struct AppState {
    bridge: LsBridge,
    token: Arc<str>,
}

fn authorized(state: &AppState, headers: &HeaderMap) -> bool {
    headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == &*state.token)
}

async fn next_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<NextReply>, StatusCode> {
    if !authorized(&state, &headers) {
        tracing::warn!("Liquidsoap bridge: /next refused (bad or missing token)");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(state.bridge.next().await))
}

async fn track_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(ev): Json<TrackEvent>,
) -> StatusCode {
    if !authorized(&state, &headers) {
        tracing::warn!("Liquidsoap bridge: /track refused (bad or missing token)");
        return StatusCode::UNAUTHORIZED;
    }
    state.bridge.track_started(&ev).await;
    StatusCode::OK
}

/// The bridge routes, behind the shared token.
pub fn router(bridge: LsBridge, token: &str) -> Router {
    Router::new()
        .route("/ls/v1/next", post(next_handler))
        .route("/ls/v1/track", post(track_handler))
        .with_state(AppState { bridge, token: Arc::from(token) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::grid_index::insert_rule;
    use crate::resolver::{Rule, RuleKind, Validity};
    use crate::station_control::ControlAction;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Engine with `music/a.mp3` under a `music` BaseRotation floor.
    async fn bridge() -> (tempfile::TempDir, LsBridge) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        let m = crate::media::ScannedMedia {
            rel_path: "music/a.mp3".into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 1000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        crate::media_index::replace_library(&pool, &[m], 1000).await.unwrap();
        let toml = "name = \"music\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&pool, "music", &pl, toml, Some("music")).await.unwrap();
        insert_rule(
            &pool,
            &Rule {
                id: "floor".into(),
                enabled: true,
                validity: Validity::default(),
                kind: RuleKind::BaseRotation { playlist_ref: "music".into() },
            },
        )
        .await
        .unwrap();
        let eng = GridEngine::new(pool, "UTC");
        (dir, LsBridge::new(eng, Path::new("/srv/media")).unwrap())
    }

    #[tokio::test]
    async fn next_hands_out_an_annotated_absolute_uri() {
        let (_d, b) = bridge().await;
        let r = b.next().await;
        assert_eq!(r.kind, "file");
        assert_eq!(r.uri, "annotate:stationd_rid=\"1\":/srv/media/music/a.mp3");
        let r2 = b.next().await;
        assert!(r2.uri.starts_with("annotate:stationd_rid=\"2\":"));
        assert_eq!(b.status().pulls, 2);
    }

    #[tokio::test]
    async fn halted_is_not_a_fallback() {
        let (_d, b) = bridge().await;
        b.engine.control().apply(ControlAction::Stop, "cli").unwrap();
        let r = b.next().await;
        assert_eq!(r, NextReply::halted("stopped"));
        b.engine.control().apply(ControlAction::Resume, "cli").unwrap();
        assert_eq!(b.next().await.kind, "file");
    }

    #[tokio::test]
    async fn no_rule_is_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        let b = LsBridge::new(GridEngine::new(pool, "UTC"), Path::new("/m")).unwrap();
        assert_eq!(b.next().await, NextReply::none("fallback"));
    }

    #[tokio::test]
    async fn track_start_puts_the_media_on_air_and_counts_it() {
        let (_d, b) = bridge().await;
        b.next().await;
        b.track_started(&TrackEvent { rid: "1".into(), kind: String::new() }).await;
        let st = b.status();
        let on_air = st.on_air.unwrap();
        assert_eq!(on_air.kind, OnAirKind::Track);
        assert_eq!(on_air.media_path.as_deref(), Some("music/a.mp3"));
        assert_eq!(on_air.playlist_ref.as_deref(), Some("music"));
        assert_eq!(st.tracks_started, 1);
        // The same rid twice is unknown (consumed), never double-counted.
        b.track_started(&TrackEvent { rid: "1".into(), kind: String::new() }).await;
        let st = b.status();
        assert_eq!(st.on_air.unwrap().kind, OnAirKind::Unknown);
        assert_eq!(st.tracks_started, 1);
    }

    #[tokio::test]
    async fn next_is_the_prefetched_track_until_it_starts() {
        let (_d, b) = bridge().await;
        b.next().await; // rid 1: starts right away
        b.track_started(&TrackEvent { rid: "1".into(), kind: String::new() }).await;
        assert!(b.status().next.is_none(), "started → no longer next");
        b.next().await; // rid 2: prefetched while 1 airs
        let n = b.status().next.unwrap();
        assert_eq!((n.rid, n.media_path.as_str(), n.playlist_ref.as_deref()), (2, "music/a.mp3", Some("music")));
        assert!(!b.prepared_is_override());
        // A halted reply: nothing queued behind the current track.
        b.engine.control().apply(ControlAction::Stop, "cli").unwrap();
        b.next().await;
        assert!(b.status().next.is_none());
    }

    #[tokio::test]
    async fn resume_from_pause_puts_the_frozen_track_back_on_air() {
        let (_d, b) = bridge().await;
        b.next().await;
        b.track_started(&TrackEvent { rid: "1".into(), kind: String::new() }).await;
        let before = b.status().on_air.unwrap();
        // The noise takes over BECAUSE of a pause (the state is Paused first):
        // only then is the track frozen rather than ended.
        b.engine.control().apply(ControlAction::Pause, "cli").unwrap();
        b.track_started(&TrackEvent { rid: String::new(), kind: "halted".into() }).await;
        assert_eq!(b.status().on_air.unwrap().kind, OnAirKind::Halted);
        b.engine.control().apply(ControlAction::Resume, "cli").unwrap();
        b.resumed_from_pause();
        let after = b.status().on_air.unwrap();
        assert_eq!(after.kind, OnAirKind::Track);
        assert_eq!(after.media_path, before.media_path);
        assert_eq!(after.since, before.since, "same airing, not a new start");
        // A second resume without a halt in between changes nothing.
        b.resumed_from_pause();
        assert_eq!(b.status().on_air.unwrap().kind, OnAirKind::Track);
    }

    #[tokio::test]
    async fn stop_is_stopping_until_the_track_ends() {
        let (_d, b) = bridge().await;
        b.next().await;
        b.track_started(&TrackEvent { rid: "1".into(), kind: String::new() }).await;
        assert_eq!(b.air_state(), "playing");
        b.engine.control().apply(ControlAction::Stop, "cli").unwrap();
        assert_eq!(b.air_state(), "stopping", "the current track plays to its end");
        b.track_started(&TrackEvent { rid: String::new(), kind: "halted".into() }).await;
        assert_eq!(b.air_state(), "stopped");
        b.engine.control().apply(ControlAction::Resume, "cli").unwrap();
        b.engine.control().apply(ControlAction::Pause, "cli").unwrap();
        assert_eq!(b.air_state(), "paused");
    }

    #[tokio::test]
    async fn a_hard_override_is_resolved_by_id_out_of_queue_order() {
        use crate::station_control::{OverrideContent, OverrideMode, OverrideRequest};
        let (_d, b) = bridge().await;
        let push = |p: &str, mode| OverrideRequest {
            content: OverrideContent::Media(p.into()),
            mode,
            expiry: None,
            tracks: None,
        };
        let c = b.engine.control();
        c.push_override(push("jingles/soft.mp3", OverrideMode::Soft), "cli").unwrap();
        let hard = c.push_override(push("news/flash.mp3", OverrideMode::Hard), "cli").unwrap();
        let uri = b.interrupt_uri(hard.id).await.unwrap();
        assert!(uri.ends_with(":/srv/media/news/flash.mp3"), "{uri}");
        // consumed: gone from the queue; the soft one is still first
        let left = c.list_overrides();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content, OverrideContent::Media("jingles/soft.mp3".into()));
        // its start is recognised (rid handed out)
        let rid = uri.split('"').nth(1).unwrap().to_string();
        b.track_started(&TrackEvent { rid, kind: String::new() }).await;
        assert_eq!(b.status().on_air.unwrap().media_path.as_deref(), Some("news/flash.mp3"));
        // already consumed → nothing to interrupt with
        assert!(b.interrupt_uri(hard.id).await.is_none());
    }

    #[tokio::test]
    async fn liquidsoap_own_sources_are_reported() {
        let (_d, b) = bridge().await;
        b.track_started(&TrackEvent { rid: String::new(), kind: "halted".into() }).await;
        let first = b.status().on_air.unwrap();
        assert_eq!(first.kind, OnAirKind::Halted);
        // The noise loops: `since` stays the start of the halted period.
        b.track_started(&TrackEvent { rid: String::new(), kind: "halted".into() }).await;
        assert_eq!(b.status().on_air.unwrap().since, first.since);
        b.track_started(&TrackEvent { rid: String::new(), kind: "fallback".into() }).await;
        assert_eq!(b.status().on_air.unwrap().kind, OnAirKind::Fallback);
    }

    // ----- end of track: time really on air → unplayed_only mark ---------

    /// Three 10-min episodes under `pod` (dynamic, oldest, unplayed_only),
    /// aired THROUGH a group `show` (rotate [pod]) on the floor: the mark must
    /// land on the leaf `pod`, never on the group.
    async fn pod_bridge() -> (tempfile::TempDir, LsBridge, sqlx::SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        let ep = |n: u32| crate::media::ScannedMedia {
            rel_path: format!("pod/ep{n}.mp3"),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 600_000,
            size_bytes: 1,
            mtime_ns: 0,
        };
        crate::media_index::replace_library(&pool, &[ep(1), ep(2), ep(3)], 1000).await.unwrap();
        let pod = "name = \"pod\"\n[selection]\nmode = \"dynamic\"\norder = \"oldest\"\n\
                   order_by = \"filename\"\nunplayed_only = true\n[[selection.filter]]\n\
                   field = \"path\"\nop = \"prefix\"\nvalue = \"pod/\"\n";
        let pl = crate::playlist::Playlist::parse(pod).unwrap();
        crate::store::upsert(&pool, "pod", &pl, pod, Some("pod")).await.unwrap();
        let show = "name = \"show\"\n[selection]\nmode = \"group\"\nstrategy = \"rotate\"\n\
                    members = [{ ref = \"pod\" }]\n";
        let pl = crate::playlist::Playlist::parse(show).unwrap();
        crate::store::upsert(&pool, "show", &pl, show, Some("show")).await.unwrap();
        insert_rule(
            &pool,
            &Rule {
                id: "floor".into(),
                enabled: true,
                validity: Validity::default(),
                kind: RuleKind::BaseRotation { playlist_ref: "show".into() },
            },
        )
        .await
        .unwrap();
        let eng = GridEngine::new(pool.clone(), "UTC");
        (dir, LsBridge::new(eng, Path::new("/srv/media")).unwrap(), pool)
    }

    async fn played(pool: &sqlx::SqlitePool, playlist: &str) -> std::collections::HashSet<String> {
        crate::episode_play::played_matching(pool, playlist).await.unwrap()
    }

    impl LsBridge {
        fn at(&self, t: i64) {
            self.engine.set_clock(Some(crate::resolver::Epoch(t)));
        }
        async fn start(&self, rid: u64) {
            self.track_started(&TrackEvent { rid: rid.to_string(), kind: String::new() }).await;
        }
        async fn source(&self, kind: &str) {
            self.track_started(&TrackEvent { rid: String::new(), kind: kind.into() }).await;
        }
    }

    #[tokio::test]
    async fn a_track_played_to_the_end_marks_its_leaf_not_the_group() {
        let (_d, b, pool) = pod_bridge().await;
        b.at(1000);
        b.next().await; // rid 1 = ep1
        b.start(1).await;
        assert_eq!(b.status().on_air.unwrap().leaf_ref.as_deref(), Some("pod"));
        b.at(1597); // 597 s on air: the next one starts 3 s early (crossfade)
        b.next().await; // rid 2
        b.start(2).await;
        assert_eq!(played(&pool, "pod").await, ["pod/ep1.mp3".to_string()].into());
        assert!(played(&pool, "show").await.is_empty(), "never keyed by the group");
    }

    #[tokio::test]
    async fn a_skipped_track_is_not_marked() {
        let (_d, b, pool) = pod_bridge().await;
        b.at(1000);
        b.next().await;
        b.start(1).await;
        b.at(1100); // skipped after 100 s
        b.next().await;
        b.start(2).await;
        assert!(played(&pool, "pod").await.is_empty());
    }

    #[tokio::test]
    async fn a_pause_does_not_count_as_air_time() {
        let (_d, b, pool) = pod_bridge().await;
        // Paused after 100 s, left frozen far longer than the episode, then
        // abandoned (a new track starts): 100 s on air → not marked.
        b.at(1000);
        b.next().await;
        b.start(1).await;
        b.at(1100);
        b.engine.control().apply(ControlAction::Pause, "cli").unwrap();
        b.source("halted").await;
        b.at(5000);
        b.engine.control().apply(ControlAction::Resume, "cli").unwrap();
        b.next().await;
        b.start(2).await;
        assert!(played(&pool, "pod").await.is_empty());
    }

    #[tokio::test]
    async fn a_paused_then_resumed_track_played_to_the_end_is_marked() {
        let (_d, b, pool) = pod_bridge().await;
        b.at(1000);
        b.next().await;
        b.start(1).await;
        b.at(1300); // 300 s on air
        b.engine.control().apply(ControlAction::Pause, "cli").unwrap();
        b.source("halted").await;
        b.at(9000); // a long pause: not air time
        b.engine.control().apply(ControlAction::Resume, "cli").unwrap();
        b.resumed_from_pause();
        let back = b.status().on_air.unwrap();
        assert_eq!((back.kind, back.since), (OnAirKind::Track, 1000), "same airing");
        b.at(9300); // 300 s more: 600 s in all
        b.next().await;
        b.start(2).await;
        assert_eq!(played(&pool, "pod").await, ["pod/ep1.mp3".to_string()].into());
    }

    #[tokio::test]
    async fn a_stop_lets_the_track_end_and_it_is_marked() {
        let (_d, b, pool) = pod_bridge().await;
        b.at(1000);
        b.next().await;
        b.start(1).await;
        b.engine.control().apply(ControlAction::Stop, "cli").unwrap();
        b.at(1600); // plays to its end, then the halted noise
        b.source("halted").await;
        assert_eq!(played(&pool, "pod").await, ["pod/ep1.mp3".to_string()].into());
    }

    #[tokio::test]
    async fn a_track_cut_by_the_fallback_is_not_marked() {
        let (_d, b, pool) = pod_bridge().await;
        b.at(1000);
        b.next().await;
        b.start(1).await;
        b.at(1200); // Liquidsoap lost the track: the safety net takes over
        b.source("fallback").await;
        assert!(played(&pool, "pod").await.is_empty());
    }

    #[tokio::test]
    async fn http_routes_require_the_token() {
        let (_d, b) = bridge().await;
        let app = router(b, "tok");
        let req = |tok: Option<&str>| {
            let mut r = Request::post("/ls/v1/next").header("content-type", "application/json");
            if let Some(t) = tok {
                r = r.header("X-Stationd-Token", t);
            }
            r.body(Body::from("{}")).unwrap()
        };
        let res = app.clone().oneshot(req(None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app.clone().oneshot(req(Some("nope"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app.clone().oneshot(req(Some("tok"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
        let reply: NextReply = serde_json::from_slice(&body).unwrap();
        assert_eq!(reply.kind, "file");
        // every field present on the wire (fixed record on the LS side)
        let raw: serde_json::Value = serde_json::from_slice(&body).unwrap();
        for k in ["kind", "uri", "state", "reason"] {
            assert!(raw.get(k).is_some(), "missing {k}");
        }

        let track = Request::post("/ls/v1/track")
            .header("content-type", "application/json")
            .header("X-Stationd-Token", "tok")
            .body(Body::from(r#"{"rid":"1","kind":""}"#))
            .unwrap();
        assert_eq!(app.oneshot(track).await.unwrap().status(), StatusCode::OK);
    }
}
