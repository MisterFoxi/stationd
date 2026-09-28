//! The on-air view: one coherent snapshot of the air — what plays, what is
//! prepared, what should follow (simulated, `onair_sim`), what played, which
//! playlist leads and which follow — recomputed when the air changes and
//! pushed to every watcher (`OnAirService.Watch`, `stationctl onair`, TUI).
//!
//! One task computes for everyone, and only while someone watches. It wakes
//! on the station's change counters (`StationControl::watch_air` / `_meta`),
//! debounces a burst of changes, and re-runs the simulation on an AIR change
//! only (an audience sample just refreshes the rest). A 30 s tick keeps the
//! clock-bound parts fresh.
//!
//! Read-only: this module changes nothing in the station. Like the other
//! domain modules it knows nothing of the proto (`onair_grpc` translates).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::{watch, Notify};

use crate::broadcast_log::{self, LogRow};
use crate::grid_engine::GridEngine;
use crate::ls_bridge::{LsBridge, OnAirKind};
use crate::onair_sim::{self, SimStart, SimTrack};
pub use crate::onair_sim::Note;
use crate::plugin::PluginHandle;
use crate::resolver::Epoch;
use crate::station_control::{BroadcastState, Incident, IncidentKind, StationControl};

/// What one snapshot holds at most; each watcher gets its own cut.
pub const UPCOMING_MAX: usize = 30;
pub const HISTORY_MAX: usize = 100;
pub const PLAYLISTS_MAX: usize = 20;
/// How far ahead the playlists to come are projected.
const PLAYLISTS_WINDOW_S: i64 = 24 * 3600;
const DEBOUNCE: Duration = Duration::from_millis(200);
const TICK: Duration = Duration::from_secs(30);

/// How a track ended (history only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    /// Not history, or not ended yet.
    #[default]
    None,
    Aired,
    Cut,
    Unknown,
}

/// One track of the view (on air, prepared, simulated or played).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    pub rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub started_at: Option<i64>,
    pub estimated_at: Option<i64>,
    pub playlist_ref: Option<String>,
    pub leaf_ref: Option<String>,
    pub rule_id: Option<String>,
    pub origin: Option<String>,
    pub override_source: Option<String>,
    pub outcome: Outcome,
    pub stream: bool,
    pub cut_at: Option<i64>,
}

/// A playlist leading now or later.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Slot {
    pub playlist_ref: String,
    pub rule_id: Option<String>,
    pub origin: Option<String>,
    pub from: Option<i64>,
    pub at_local: Option<String>,
    /// This slot will air nothing (the faulty line).
    pub issue: SlotIssue,
}

/// Why a slot to come will air nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlotIssue {
    #[default]
    None,
    /// No media at all in its pool (seen by the projection).
    PoolEmpty,
    /// A pool, but nothing playable then (seen by the simulation:
    /// constraints, plugins, unavailable files…).
    NothingPlayable,
}

/// How far back incidents seen on the air are shown.
const INCIDENTS_SHOWN_S: i64 = 3600;

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub revision: u64,
    pub observed_at: i64,
    pub state: String,
    pub listeners: Option<u32>,
    pub on_air_kind: String,
    pub on_air: Option<Track>,
    pub prefetched: Option<Track>,
    pub upcoming: Vec<Track>,
    pub notes: Vec<Note>,
    pub history: Vec<Track>,
    pub current_playlist: Option<Slot>,
    pub next_playlists: Vec<Slot>,
    pub indicative: Vec<Slot>,
    pub live_dj: Option<String>,
    pub pending_overrides: u32,
    pub liquidsoap: bool,
    pub timezone: String,
}

/// Everything the view reads from. All cheap clones of shared handles.
#[derive(Clone)]
pub struct Sources {
    pub pool: SqlitePool,
    /// The station database file (copied for each simulation).
    pub db_path: PathBuf,
    pub tz: String,
    pub engine: GridEngine,
    pub control: StationControl,
    pub bridge: Option<LsBridge>,
    pub plugins: Option<PluginHandle>,
}

/// Handle on the view task.
#[derive(Clone)]
pub struct OnAirHub {
    tx: watch::Sender<Option<Arc<Snapshot>>>,
    wake: Arc<Notify>,
    sources: Sources,
}

impl OnAirHub {
    /// Start the task. It computes nothing until someone subscribes.
    pub fn spawn(sources: Sources) -> Self {
        let (tx, _) = watch::channel(None);
        let hub = OnAirHub { tx, wake: Arc::new(Notify::new()), sources };
        let task = hub.clone();
        tokio::spawn(async move { task.run().await });
        hub
    }

    /// Watch the view: the receiver sees the next snapshot soon (a fresh
    /// computation is triggered), then every new one.
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Snapshot>>> {
        let rx = self.tx.subscribe();
        self.wake.notify_one();
        rx
    }

    /// Played tracks strictly before `before` (epoch s), most recent first.
    pub async fn history(&self, before: i64, limit: usize) -> Result<Vec<Track>, sqlx::Error> {
        let rows = broadcast_log::aired_before(&self.sources.pool, before, limit as u32).await?;
        Ok(rows.into_iter().map(history_track).collect())
    }

    async fn run(self) {
        let mut air = self.sources.control.watch_air();
        let mut meta = self.sources.control.watch_meta();
        let mut revision: u64 = 0;
        let mut sim_for: Option<u64> = None;
        let mut sim = SimPart::default();
        loop {
            tokio::select! {
                r = air.changed() => if r.is_err() { return },
                r = meta.changed() => if r.is_err() { return },
                _ = self.wake.notified() => {},
                _ = tokio::time::sleep(TICK) => {},
            }
            // A burst of changes (a track start = several) → one snapshot.
            tokio::time::sleep(DEBOUNCE).await;
            let air_rev = *air.borrow_and_update();
            meta.borrow_and_update();
            if self.tx.receiver_count() == 0 {
                sim_for = None; // nobody watched: recompute on the next watcher
                continue;
            }
            let now = self.sources.engine.effective_now(None);
            let mut snap = observe(&self.sources, now).await;
            if sim_for != Some(air_rev) {
                sim = simulate_part(&self.sources, &snap, now).await;
                sim_for = Some(air_rev);
            }
            snap.upcoming = sim.upcoming.clone();
            let seen = std::mem::take(&mut snap.notes);
            snap.notes = sim.notes.clone();
            snap.notes.extend(seen);
            snap.next_playlists = sim.next_playlists.clone();
            snap.indicative = sim.indicative.clone();
            revision += 1;
            snap.revision = revision;
            self.tx.send_replace(Some(Arc::new(snap)));
        }
    }
}

/// The air-dependent, costlier part (simulation + projection), cached until
/// the next air change.
#[derive(Default, Clone)]
struct SimPart {
    upcoming: Vec<Track>,
    notes: Vec<Note>,
    next_playlists: Vec<Slot>,
    indicative: Vec<Slot>,
}

/// The cheap, always-fresh part: state, audience, on air, prepared, history,
/// current playlist.
pub async fn observe(src: &Sources, now: Epoch) -> Snapshot {
    let control = &src.control;
    let mut snap = Snapshot {
        observed_at: now.0,
        state: control.state().as_str().to_string(),
        listeners: control.listeners(),
        live_dj: control.live_dj(),
        pending_overrides: control.list_overrides().len() as u32,
        liquidsoap: src.bridge.is_some(),
        timezone: src.tz.clone(),
        ..Default::default()
    };
    let status = src.bridge.as_ref().map(LsBridge::status);
    let mut current_log: Option<i64> = None;
    if let Some(a) = status.as_ref().and_then(|s| s.on_air.clone()) {
        snap.on_air_kind = a.kind.as_str().to_string();
        match a.kind {
            OnAirKind::Track => {
                let mut t = described(src, a.media_path.clone().unwrap_or_default(), a.log_id).await;
                t.started_at = Some(a.since);
                t.playlist_ref = t.playlist_ref.or(a.playlist_ref.clone());
                t.leaf_ref = t.leaf_ref.or(a.leaf_ref.clone());
                current_log = a.log_id;
                snap.current_playlist = t.playlist_ref.clone().map(|p| Slot {
                    playlist_ref: p,
                    rule_id: t.rule_id.clone(),
                    origin: t.origin.clone(),
                    ..Default::default()
                });
                snap.on_air = Some(t);
            }
            OnAirKind::Relay => {
                snap.on_air = Some(Track {
                    rel_path: a.media_path.clone().unwrap_or_default(),
                    started_at: Some(a.since),
                    stream: true,
                    ..Default::default()
                });
            }
            _ => {}
        }
    }
    if let Some(n) = status.as_ref().and_then(|s| s.next.clone()) {
        let mut t = described(src, n.media_path.clone(), n.log_id).await;
        t.playlist_ref = t.playlist_ref.or(n.playlist_ref.clone());
        t.leaf_ref = t.leaf_ref.or(n.leaf_ref.clone());
        if n.from_override && t.origin.is_none() {
            t.origin = Some("Override".into());
        }
        snap.prefetched = Some(t);
    }
    snap.notes.extend(seen_notes(&control.incidents_since(Epoch(now.0 - INCIDENTS_SHOWN_S))));
    match broadcast_log::aired_before(&src.pool, now.0 + 1, HISTORY_MAX as u32 + 1).await {
        Ok(rows) => {
            snap.history = rows
                .into_iter()
                .filter(|r| Some(r.id) != current_log)
                .take(HISTORY_MAX)
                .map(history_track)
                .collect();
        }
        Err(e) => snap.notes.push(Note::HistoryUnreadable { reason: e.to_string() }),
    }
    snap
}

/// A track we logged, described from its log row (provenance) and the
/// media index (title, duration…); falls back to the bare path.
async fn described(src: &Sources, media: String, log_id: Option<i64>) -> Track {
    let row = match log_id {
        Some(id) => broadcast_log::row(&src.pool, id).await.ok().flatten(),
        None => None,
    };
    let mut t = match row {
        Some(r) => Track { outcome: Outcome::None, ..history_track(r) },
        None => Track { rel_path: media.clone(), ..Default::default() },
    };
    if t.duration_ms.is_none() || t.title.is_none() {
        if let Ok(Some((title, artist, album, d))) = crate::media_index::brief(&src.pool, &media).await {
            t.title = t.title.or(title);
            t.artist = t.artist.or(artist);
            t.album = t.album.or(album);
            t.duration_ms = t.duration_ms.or(Some(d));
        }
    }
    t.started_at = None;
    t
}

fn history_track(r: LogRow) -> Track {
    let outcome = match (r.left_at, r.played_to_end) {
        (_, Some(true)) => Outcome::Aired,
        (_, Some(false)) => Outcome::Cut,
        _ => Outcome::Unknown,
    };
    let origin = r.origin.clone();
    Track {
        rel_path: r.rel_path,
        title: r.title,
        artist: r.artist,
        album: r.album,
        duration_ms: r.duration_ms,
        started_at: r.aired_at,
        playlist_ref: r.playlist_ref,
        leaf_ref: r.leaf_ref,
        rule_id: r.rule_id,
        override_source: None,
        origin,
        outcome,
        ..Default::default()
    }
}

/// Simulation of what follows the prepared track, and the playlists to come.
async fn simulate_part(src: &Sources, snap: &Snapshot, now: Epoch) -> SimPart {
    let mut part = SimPart::default();
    let state = BroadcastState::parse(&snap.state);

    // Playlists to come: the grid's own projection (read-only).
    match src.engine.preview(now, PLAYLISTS_WINDOW_S).await {
        Ok(p) => {
            let mut last = snap.current_playlist.as_ref().map(|s| s.playlist_ref.clone());
            for o in p.occurrences.iter().filter(|o| o.epoch.0 > now.0) {
                if o.playlist_ref.is_empty() || last.as_deref() == Some(o.playlist_ref.as_str()) {
                    continue;
                }
                last = Some(o.playlist_ref.clone());
                part.next_playlists.push(Slot {
                    playlist_ref: o.playlist_ref.clone(),
                    rule_id: Some(o.rule_id.clone()).filter(|r| !r.is_empty()),
                    origin: Some(format!("{:?}", o.origin)),
                    from: Some(o.epoch.0),
                    at_local: Some(o.at_local.clone()),
                    issue: if o.pool.selected_count == Some(0) { SlotIssue::PoolEmpty } else { SlotIssue::None },
                });
                if part.next_playlists.len() >= PLAYLISTS_MAX {
                    break;
                }
            }
            part.indicative = p
                .indicative
                .iter()
                .map(|i| Slot {
                    playlist_ref: i.playlist_ref.clone(),
                    rule_id: Some(i.rule_id.clone()),
                    origin: Some("Every".into()),
                    ..Default::default()
                })
                .collect();
        }
        Err(e) => part.notes.push(Note::GridProjectionFailed { reason: e.to_string() }),
    }

    // What follows the prepared track. Nothing to simulate when the station
    // resolves nothing at the next boundary.
    match state {
        Some(BroadcastState::Paused) => {
            part.notes.push(Note::StationPaused);
            return part;
        }
        Some(BroadcastState::Sleeping) => {
            part.notes.push(Note::StationSleeping);
            return part;
        }
        Some(BroadcastState::Draining) if snap.listeners == Some(0) => {
            part.notes.push(Note::SleepAtTrackEnd);
            return part;
        }
        Some(BroadcastState::Draining) => {
            part.notes.push(Note::SleepArmed);
        }
        _ => {}
    }
    if let Some(dj) = &snap.live_dj {
        part.notes.push(Note::LiveOnAir { dj: dj.clone() });
        return part;
    }

    let (at, known) = start_of_simulation(snap, now);
    if !snap.liquidsoap {
        part.notes.push(Note::NoLiquidsoap);
    }
    let want = UPCOMING_MAX.saturating_sub(usize::from(snap.prefetched.is_some()));
    let out = onair_sim::simulate(SimStart {
        live_db: &src.db_path,
        tz: &src.tz,
        control: &src.control,
        plugins: src.plugins.as_ref(),
        at,
        at_known: known,
        count: want,
    })
    .await;
    part.upcoming = out.tracks.into_iter().map(sim_track).collect();
    mark_faulty_slots(&mut part.next_playlists, &out.incidents);
    part.notes.extend(predicted_notes(&out.incidents));
    if !part.upcoming.is_empty() {
        part.notes.push(Note::Simulated);
    }
    part.notes.extend(out.notes);
    part
}

/// A hard-not-cut and a source-empty for the same rule are one problem (the
/// soft retry after the missed cut): keep the hard one.
fn dedup_incidents(incidents: &[Incident]) -> Vec<&Incident> {
    incidents
        .iter()
        .filter(|i| {
            i.kind == IncidentKind::HardNotCut
                || !incidents
                    .iter()
                    .any(|h| h.kind == IncidentKind::HardNotCut && h.rule_id == i.rule_id && h.playlist_ref == i.playlist_ref)
        })
        .collect()
}

/// Incidents the simulation ran into → notes « will not … ».
fn predicted_notes(incidents: &[Incident]) -> Vec<Note> {
    dedup_incidents(incidents)
        .into_iter()
        .map(|i| {
            let (rule, playlist, at) = (i.rule_id.clone().unwrap_or_default(), i.playlist_ref.clone(), i.first_at.0);
            match i.kind {
                IncidentKind::HardNotCut => Note::RendezvousWillNotCut { rule, playlist, at },
                IncidentKind::SourceEmpty => Note::SourceWillBeEmpty { rule, playlist, at },
            }
        })
        .collect()
}

/// Incidents seen on the air → notes « did not … ».
fn seen_notes(incidents: &[Incident]) -> Vec<Note> {
    dedup_incidents(incidents)
        .into_iter()
        .map(|i| {
            let (rule, playlist, at, count) =
                (i.rule_id.clone().unwrap_or_default(), i.playlist_ref.clone(), i.at.0, i.count);
            match i.kind {
                IncidentKind::HardNotCut => Note::RendezvousNotCut { rule, playlist, at, count },
                IncidentKind::SourceEmpty => Note::SourceWasEmpty { rule, playlist, at, count },
            }
        })
        .collect()
}

/// A slot to come is faulty when the simulation met an incident of ITS rule
/// while that slot was in force (from its start to the next slot's).
fn mark_faulty_slots(slots: &mut [Slot], incidents: &[Incident]) {
    let bounds: Vec<(Option<i64>, Option<i64>)> = (0..slots.len())
        .map(|k| (slots[k].from, slots.get(k + 1).and_then(|n| n.from)))
        .collect();
    for (slot, (from, until)) in slots.iter_mut().zip(bounds) {
        if slot.issue != SlotIssue::None {
            continue;
        }
        let (Some(rule), Some(from)) = (slot.rule_id.as_deref(), from) else { continue };
        let hit = incidents.iter().any(|i| {
            i.rule_id.as_deref() == Some(rule)
                && i.playlist_ref == slot.playlist_ref
                && i.at.0 >= from
                && until.is_none_or(|u| i.first_at.0 < u)
        });
        if hit {
            slot.issue = SlotIssue::NothingPlayable;
        }
    }
}

/// When the first simulated track would start: after the prepared track,
/// itself after the track on air. `known = false` as soon as one of those
/// lengths is unknown (then `now` is a nominal start).
fn start_of_simulation(snap: &Snapshot, now: Epoch) -> (Epoch, bool) {
    let mut t = now.0;
    let mut known = true;
    match &snap.on_air {
        Some(a) if !a.stream => match (a.started_at, a.duration_ms) {
            (Some(s), Some(d)) => t = (s + (d + 999) / 1000).max(now.0),
            _ => known = false,
        },
        Some(_) => known = false, // a relay: its end is unknown
        None => {}
    }
    if let Some(p) = &snap.prefetched {
        match p.duration_ms {
            Some(d) => t += (d + 999) / 1000,
            None => known = false,
        }
    }
    (Epoch(t), known)
}

fn sim_track(s: SimTrack) -> Track {
    Track {
        rel_path: s.media,
        title: s.title,
        artist: s.artist,
        album: s.album,
        duration_ms: s.duration_ms,
        estimated_at: s.starts_at.map(|e| e.0),
        playlist_ref: s.playlist_ref,
        leaf_ref: s.leaf_ref,
        rule_id: s.rule_id,
        origin: Some(s.origin),
        override_source: s.override_source,
        stream: s.stream,
        cut_at: s.cut_at.map(|e| e.0),
        ..Default::default()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn snap_with(on_air: Option<Track>, prefetched: Option<Track>) -> Snapshot {
        Snapshot { on_air, prefetched, ..Default::default() }
    }

    #[test]
    fn simulation_starts_after_the_prepared_track() {
        let on_air = Track { started_at: Some(1_000), duration_ms: Some(180_000), ..Default::default() };
        let next = Track { duration_ms: Some(60_500), ..Default::default() };
        let (at, known) = start_of_simulation(&snap_with(Some(on_air), Some(next)), Epoch(1_050));
        assert_eq!((at, known), (Epoch(1_000 + 180 + 61), true));
    }

    #[test]
    fn an_unknown_length_makes_the_start_unknown() {
        let on_air = Track { started_at: Some(1_000), duration_ms: None, ..Default::default() };
        let (_, known) = start_of_simulation(&snap_with(Some(on_air), None), Epoch(1_050));
        assert!(!known);
        let relay = Track { stream: true, ..Default::default() };
        assert!(!start_of_simulation(&snap_with(Some(relay), None), Epoch(1)).1);
    }

    #[test]
    fn nothing_on_air_starts_now() {
        assert_eq!(start_of_simulation(&snap_with(None, None), Epoch(77)), (Epoch(77), true));
    }

    #[test]
    fn history_outcome_from_the_log() {
        let row = |left, end| LogRow {
            id: 1,
            rel_path: "a.mp3".into(),
            played_at: 1,
            aired_at: Some(2),
            left_at: left,
            played_to_end: end,
            rule_id: None,
            origin: None,
            playlist_ref: None,
            leaf_ref: None,
            title: None,
            artist: None,
            album: None,
            duration_ms: None,
        };
        assert_eq!(history_track(row(Some(9), Some(true))).outcome, Outcome::Aired);
        assert_eq!(history_track(row(Some(9), Some(false))).outcome, Outcome::Cut);
        assert_eq!(history_track(row(Some(9), None)).outcome, Outcome::Unknown);
        assert_eq!(history_track(row(None, None)).outcome, Outcome::Unknown);
    }

    /// A station with only a music floor (60 s tracks), no Liquidsoap.
    pub(crate) async fn hub() -> (tempfile::TempDir, OnAirHub, StationControl) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.db");
        let pool = crate::db::init(&path).await.unwrap();
        let lib: Vec<_> = (1..=4)
            .map(|i| crate::media::ScannedMedia {
                rel_path: format!("music/{i}.mp3"),
                title: Some(format!("Titre {i}")),
                artist: None,
                album: None,
                year: None,
                genres: vec![],
                duration_ms: 60_000,
                size_bytes: 1,
                mtime_ns: 0,
            })
            .collect();
        crate::media_index::replace_library(&pool, &lib, 1000).await.unwrap();
        let toml = "name = \"music\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                    [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"music/\"\n";
        let pl = crate::playlist::Playlist::parse(toml).unwrap();
        crate::store::upsert(&pool, "music", &pl, toml, Some("music")).await.unwrap();
        crate::grid_index::insert_rule(
            &pool,
            &crate::resolver::Rule {
                id: "floor".into(),
                enabled: true,
                validity: Default::default(),
                kind: crate::resolver::RuleKind::BaseRotation { playlist_ref: "music".into() },
            },
        )
        .await
        .unwrap();
        let control = StationControl::new_in_memory();
        let engine = GridEngine::new(pool.clone(), "UTC").with_control(control.clone());
        engine.sync_grid().await.unwrap();
        let hub = OnAirHub::spawn(Sources {
            pool,
            db_path: path,
            tz: "UTC".into(),
            engine,
            control: control.clone(),
            bridge: None,
            plugins: None,
        });
        (dir, hub, control)
    }

    async fn next(rx: &mut watch::Receiver<Option<Arc<Snapshot>>>) -> Arc<Snapshot> {
        tokio::time::timeout(Duration::from_secs(10), rx.changed()).await.expect("a snapshot").unwrap();
        rx.borrow_and_update().clone().expect("some")
    }

    #[tokio::test]
    async fn a_watcher_gets_a_full_snapshot_then_one_per_burst() {
        let (_d, hub, control) = hub().await;
        let mut rx = hub.subscribe();
        let first = next(&mut rx).await;
        assert_eq!(first.state, "running");
        assert!(!first.liquidsoap);
        assert_eq!(first.upcoming.len(), UPCOMING_MAX, "notes: {:?}", first.notes);
        assert!(first.upcoming[0].title.as_deref().is_some_and(|t| t.starts_with("Titre")));
        assert!(first.notes.contains(&Note::NoLiquidsoap));
        assert!(first.history.is_empty());

        // A burst of changes → one new snapshot, not five.
        for _ in 0..5 {
            control.bump_air();
        }
        let second = next(&mut rx).await;
        assert_eq!(second.revision, first.revision + 1);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!rx.has_changed().unwrap(), "no extra snapshot for the same burst");

        // Paused: nothing follows, and it says so.
        control.apply(crate::station_control::ControlAction::Pause, "test").unwrap();
        let paused = next(&mut rx).await;
        assert_eq!(paused.state, "paused");
        assert!(paused.upcoming.is_empty());
        assert!(paused.notes.contains(&Note::StationPaused));
    }

    #[tokio::test]
    async fn an_audience_sample_refreshes_without_resimulating() {
        let (_d, hub, control) = hub().await;
        let mut rx = hub.subscribe();
        let first = next(&mut rx).await;
        control.sample_listeners(7);
        let second = next(&mut rx).await;
        assert_eq!(second.listeners, Some(7));
        assert_eq!(second.upcoming, first.upcoming, "same simulation reused");
    }

    fn incident(kind: IncidentKind, rule: &str, playlist: &str, first: i64, last: i64) -> Incident {
        Incident {
            kind,
            rule_id: Some(rule.into()),
            playlist_ref: playlist.into(),
            origin: String::new(),
            at: Epoch(last),
            first_at: Epoch(first),
            count: 1,
        }
    }

    fn slot(pl: &str, rule: &str, from: i64) -> Slot {
        Slot { playlist_ref: pl.into(), rule_id: Some(rule.into()), from: Some(from), ..Default::default() }
    }

    #[test]
    fn the_faulty_slot_is_the_one_whose_rule_failed_while_in_force() {
        let mut slots = vec![slot("toph", "toph", 1000), slot("rotation", "floor", 1060), slot("toph", "toph", 4600)];
        let inc = [incident(IncidentKind::HardNotCut, "toph", "toph", 1000, 1000)];
        mark_faulty_slots(&mut slots, &inc);
        assert_eq!(slots[0].issue, SlotIssue::NothingPlayable);
        assert_eq!(slots[1].issue, SlotIssue::None);
        assert_eq!(slots[2].issue, SlotIssue::None, "the next hour was not reached by the simulation");
    }

    #[test]
    fn a_missed_cut_and_its_soft_retry_are_one_note() {
        let inc = [
            incident(IncidentKind::HardNotCut, "toph", "toph", 1000, 1000),
            incident(IncidentKind::SourceEmpty, "toph", "toph", 1233, 1233),
            incident(IncidentKind::SourceEmpty, "jingle", "jingles", 1500, 1600),
        ];
        let notes = predicted_notes(&inc);
        assert_eq!(
            notes,
            vec![
                Note::RendezvousWillNotCut { rule: "toph".into(), playlist: "toph".into(), at: 1000 },
                Note::SourceWillBeEmpty { rule: "jingle".into(), playlist: "jingles".into(), at: 1500 },
            ]
        );
        assert!(matches!(seen_notes(&inc)[1], Note::SourceWasEmpty { at: 1600, .. }), "seen = latest occurrence");
    }
}
