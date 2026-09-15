//! Grid engine: the loop that turns the pure resolver into a live decision.
//!
//! `resolver::resolve_next` is pure and timezone-free; this is the I/O shell
//! around it. On each call it hydrates the grid (family A) and the playback
//! state (family B) from SQLite, converts `now` to civil local time via
//! `clock`, asks the resolver, then persists the decision's side effects.
//!
//! The resolver being catch-up by construction (it computes from `now` + state,
//! never from a running timer), start-up reconciliation is just `sync_grid`
//! followed by normal `next` calls — no special replay path.
//!
//! Threading note (single writer): today this reloads from SQLite each call,
//! which is correct and simplest. The eventual shape is an owning tokio task
//! holding the grid in memory, invalidated on apply/reload, mutations sent over
//! an mpsc channel — same actor model as the rest of stationd.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::clock::{self, ClockError};
use crate::grid_index::{self, GridLoadError};
use crate::grid_store;
use crate::grid_toml;
use crate::resolver::{
    resolve_next, Epoch, Grid, GridDecision, LocalNow, Origin, PlaybackState, Rule, RuleKind,
};
use crate::store;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Grid(#[from] GridLoadError),
    #[error(transparent)]
    Clock(#[from] ClockError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Selection(#[from] crate::selection::SelectionError),
}

/// Splits infrastructure failure (→ gRPC `internal`) from a rejected grid
/// (→ `invalid_argument`, and — no-silent-failure — never replaces the last
/// valid grid).
#[derive(Debug, thiserror::Error)]
pub enum GridOpError {
    #[error(transparent)]
    Infra(#[from] EngineError),
    #[error("grid rejected: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

/// Live resolver over a SQLite-backed grid, in the station timezone.
pub struct GridEngine {
    pool: SqlitePool,
    /// IANA name of the station timezone (config), e.g. "Europe/Paris".
    tz: String,
    /// Plugins to notify of decisions (best-effort, fire-and-forget). `None`
    /// when no plugin system is wired (e.g. in unit tests).
    plugins: Option<crate::plugin::PluginHandle>,
}

/// One entry of a grid projection ([`GridEngine::preview`]): the instant a
/// resolved decision *starts*, in epoch UTC and rendered in station-local
/// time. Empty `rule_id`/`playlist_ref` = the fallback filled in.
#[derive(Debug, Clone)]
pub struct PreviewOccurrence {
    pub epoch: Epoch,
    pub at_local: String,
    pub origin: Origin,
    pub rule_id: String,
    pub playlist_ref: String,
}

/// A grid decision plus the concrete media it resolves to (see
/// [`GridEngine::next_media`]). `media_path` is `None` only for a fallback
/// decision (no active source → Liquidsoap's safety net fills the air).
#[derive(Debug, Clone)]
pub struct ResolvedDecision {
    pub decision: GridDecision,
    pub media_path: Option<String>,
}

impl GridEngine {
    pub fn new(pool: SqlitePool, tz: impl Into<String>) -> Self {
        Self { pool, tz: tz.into(), plugins: None }
    }

    /// Attach the plugin system so decisions are broadcast to plugins.
    pub fn with_plugins(mut self, plugins: crate::plugin::PluginHandle) -> Self {
        self.plugins = Some(plugins);
        self
    }

    /// Read the configured rules without resolving or touching playback state.
    pub async fn list_rules(&self) -> Result<Vec<crate::resolver::Rule>, EngineError> {
        Ok(grid_index::load_grid(&self.pool).await?.rules)
    }

    /// Ensure every `Every` rule has a counter row, so its cadence advances
    /// from the first track on. Call at start-up and after each grid apply /
    /// reload. Idempotent; never resets an existing counter (family B).
    pub async fn sync_grid(&self) -> Result<(), EngineError> {
        let grid = grid_index::load_grid(&self.pool).await?;
        let every_ids: Vec<String> = grid
            .rules
            .iter()
            .filter(|r| matches!(r.kind, RuleKind::Every { .. }))
            .map(|r| r.id.clone())
            .collect();
        grid_store::ensure_every_rows(&self.pool, &every_ids).await?;
        Ok(())
    }

    /// One completed station track: advance every `Every` cooldown by one.
    /// Called on a track-end event, distinct from asking for the next source.
    pub async fn on_track_completed(&self) -> Result<(), EngineError> {
        grid_store::bump_tracks_since(&self.pool).await?;
        Ok(())
    }

    fn parse_all(files: &[(String, String)]) -> Result<Vec<Rule>, Vec<String>> {
        let mut rules = Vec::new();
        let mut errors = Vec::new();
        for (path, toml) in files {
            match grid_toml::parse_grid(toml) {
                Ok(mut rs) => rules.append(&mut rs),
                Err(e) => errors.push(format!("{path}: {e}")),
            }
        }
        if !errors.is_empty() { return Err(errors); }
        let mut seen: HashSet<&str> = HashSet::new();
        for r in &rules {
            if !seen.insert(r.id.as_str()) {
                errors.push(format!("duplicate rule id {:?} across grid files", r.id));
            }
        }
        let bases = rules.iter().filter(|r| matches!(r.kind, RuleKind::BaseRotation { .. })).count();
        if bases > 1 {
            errors.push(format!("{bases} base_rotation rules across grid files; at most one is allowed"));
        }
        if errors.is_empty() { Ok(rules) } else { Err(errors) }
    }

    async fn known_playlist_keys(&self) -> Result<HashSet<String>, EngineError> {
        Ok(store::list(&self.pool).await?.into_iter().filter_map(|r| r.rel_path).collect())
    }

    pub async fn validate_grid(&self, files: &[(String, String)]) -> Result<(), GridOpError> {
        let rules = Self::parse_all(files).map_err(GridOpError::Invalid)?;
        let known = self.known_playlist_keys().await?;
        let ref_errors = grid_toml::validate_refs(&rules, &known);
        if !ref_errors.is_empty() { return Err(GridOpError::Invalid(ref_errors)); }
        Ok(())
    }

    pub async fn apply_grid(&self, files: &[(String, String)]) -> Result<Vec<String>, GridOpError> {
        let rules = Self::parse_all(files).map_err(GridOpError::Invalid)?;
        let known = self.known_playlist_keys().await?;
        let ref_errors = grid_toml::validate_refs(&rules, &known);
        if !ref_errors.is_empty() { return Err(GridOpError::Invalid(ref_errors)); }
        grid_index::replace_grid(&self.pool, &rules).await.map_err(EngineError::from)?;
        self.sync_grid().await?;
        Ok(rules.iter().map(|r| r.id.clone()).collect())
    }

    pub async fn export_grid(&self, rule_ids: &[String]) -> Result<String, GridOpError> {
        let grid = grid_index::load_grid(&self.pool).await.map_err(EngineError::from)?;
        let rules: Vec<Rule> = if rule_ids.is_empty() {
            grid.rules
        } else {
            let want: HashSet<&str> = rule_ids.iter().map(String::as_str).collect();
            grid.rules.into_iter().filter(|r| want.contains(r.id.as_str())).collect()
        };
        grid_toml::to_toml(&rules).map_err(|m| GridOpError::Invalid(vec![m]))
    }

    /// Project the grid over `[from, from + window)` **without** touching
    /// playback state or the wall clock — the `stationctl schedule preview`
    /// path, and the DST test (a window over a transition night reveals a
    /// hole or a doubled hour in the local column).
    ///
    /// It is a *projection*, not a track-by-track simulation: track durations
    /// are dynamic and unknown here, so we report the clock-driven structure
    /// only. We walk minute by minute (every DayPart/AtClock boundary is
    /// minute-aligned) and emit an occurrence whenever the resolved decision
    /// changes. Consumed AtClock marks are carried forward exactly as the live
    /// loop persists them, so a mark punctuates a single instant instead of
    /// swallowing its whole slot. `Every` rules are omitted: their cadence is
    /// driven by real playback (tracks/elapsed), not by the calendar, so they
    /// have no meaning in a clock projection.
    pub async fn preview(
        &self,
        from: Epoch,
        window_secs: i64,
    ) -> Result<Vec<PreviewOccurrence>, EngineError> {
        let loaded = grid_index::load_grid(&self.pool).await?;
        // Drop Every (playback-driven, not projectable on the clock).
        let grid = Grid {
            rules: loaded
                .rules
                .into_iter()
                .filter(|r| !matches!(r.kind, RuleKind::Every { .. }))
                .collect(),
        };

        // Bound the walk: ignore a non-positive window, cap at 31 days so a
        // pathological request can't spin for ages (minute granularity).
        let window = window_secs.clamp(0, 31 * 86_400);
        let end = from.0.saturating_add(window);

        let mut sim = PlaybackState::default();
        let mut out: Vec<PreviewOccurrence> = Vec::new();
        let mut prev_key: Option<(Origin, String, String)> = None;

        let mut secs = from.0;
        while secs < end {
            let epoch = Epoch(secs);
            let local = clock::to_local_now(epoch, &self.tz)?;
            let decision = resolve_next(local, &grid, &sim);

            // Consume the mark so the next minute in this slot yields the base,
            // mirroring the live loop (a mark is an instant, not a segment).
            if let Some(token) = &decision.mark_taken {
                sim.at_clock_taken.insert(token.clone());
            }

            let rule_id = decision.rule_id.clone().unwrap_or_default();
            let playlist_ref = decision.playlist_ref.clone().unwrap_or_default();
            let key = (decision.origin.clone(), rule_id.clone(), playlist_ref.clone());
            if prev_key.as_ref() != Some(&key) {
                out.push(PreviewOccurrence {
                    epoch,
                    at_local: fmt_local(&local, &self.tz),
                    origin: decision.origin.clone(),
                    rule_id,
                    playlist_ref,
                });
                prev_key = Some(key);
            }
            secs += 60;
        }
        Ok(out)
    }

    /// Resolve the source to pull at a track boundary for instant `now`, and
    /// persist the decision's side effects (a consumed AtClock occurrence, an
    /// `Every` cooldown reset). Pure resolution, durable bookkeeping.
    pub async fn next(&self, now: Epoch) -> Result<GridDecision, EngineError> {
        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;

        let decision = resolve_next(local, &grid, &state);

        if let Some(token) = &decision.mark_taken {
            grid_store::record_at_clock_taken(&self.pool, token, now).await?;
        }
        if decision.origin == Origin::Every {
            if let Some(rule_id) = &decision.rule_id {
                grid_store::reset_every(&self.pool, rule_id, now).await?;
            }
        }
        Ok(decision)
    }

    /// Resolve a full decision AND the concrete media to pull: `next` decides
    /// the source, the selection stage turns that `playlist_ref` into a media
    /// file. A `Fallback` decision (no ref) yields `media_path = None` — the
    /// Liquidsoap safety fallback fills the air, never a silent gap.
    ///
    /// Ordering note: `next` persists its side effects (a consumed AtClock
    /// mark, an Every reset) before selection runs; if selection then fails,
    /// the grid state has already advanced. Acceptable at this milestone (a
    /// dev/CLI path); revisit when the live loop drives Liquidsoap.
    pub async fn next_media(&self, now: Epoch) -> Result<ResolvedDecision, EngineError> {
        let decision = self.next(now).await?;
        let media_path = match &decision.playlist_ref {
            Some(r) => Some(
                crate::selection::resolve_ref_with_plugins(&self.pool, self.plugins.as_ref(), r)
                    .await?,
            ),
            None => None,
        };

        // Notify plugins (best-effort, never blocks this path). Observation
        // only — a plugin cannot change the decision from here.
        if let Some(plugins) = &self.plugins {
            plugins.emit(crate::plugin::PluginEvent::TrackResolved {
                media_path: media_path.clone(),
                playlist_ref: decision.playlist_ref.clone(),
                rule_id: decision.rule_id.clone(),
                origin: format!("{:?}", decision.origin),
            });
        }

        Ok(ResolvedDecision { decision, media_path })
    }
}

/// Render a `LocalNow` as `YYYY-MM-DD HH:MM <IANA zone>` — named zone, not a
/// frozen offset (per the time doc). Enough to read a preview and to spot a
/// DST hole/doubling when paired with the epoch-UTC column.
fn fmt_local(local: &LocalNow, tz: &str) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} {tz}",
        local.date.year, local.date.month, local.date.day, local.wall.hour, local.wall.minute
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::grid_index::insert_rule;
    use crate::resolver::{Cadence, ClockAnchor, Mode, Rule, RuleKind, Validity};

    async fn engine() -> (tempfile::TempDir, GridEngine) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("test.db")).await.unwrap();
        (dir, GridEngine::new(pool, "UTC")) // UTC → epoch maps straight to wall time
    }

    fn rule(id: &str, kind: RuleKind) -> Rule {
        Rule { id: id.into(), enabled: true, validity: Validity::default(), kind }
    }

    /// epoch for a given UTC hour:minute (tz is "UTC" in these tests).
    fn at(h: i64, m: i64) -> Epoch {
        Epoch(h * 3600 + m * 60)
    }

    #[tokio::test]
    async fn unknown_timezone_surfaces_as_error() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        let eng = GridEngine::new(pool, "Nowhere/Nope");
        assert!(matches!(eng.next(at(9, 0)).await, Err(EngineError::Clock(_))));
    }

    #[tokio::test]
    async fn every_cadence_survives_across_calls() {
        let (_dir, eng) = engine().await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule("jingle", RuleKind::Every { playlist_ref: "ids".into(), cadence: Cadence::Tracks(2) }),
        )
        .await
        .unwrap();
        eng.sync_grid().await.unwrap();

        // tracks_since = 0 → not due → base.
        assert_eq!(eng.next(at(9, 0)).await.unwrap().origin, Origin::BaseRotation);
        eng.on_track_completed().await.unwrap(); // 1
        assert_eq!(eng.next(at(9, 0)).await.unwrap().origin, Origin::BaseRotation);
        eng.on_track_completed().await.unwrap(); // 2
        // Now due → Every fires and resets the counter.
        let d = eng.next(at(9, 0)).await.unwrap();
        assert_eq!(d.origin, Origin::Every);
        assert_eq!(d.playlist_ref.as_deref(), Some("ids"));
        // Reset persisted → immediately after, back to base.
        eng.on_track_completed().await.unwrap(); // 1
        assert_eq!(eng.next(at(9, 0)).await.unwrap().origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn atclock_fires_once_then_is_consumed() {
        let (_dir, eng) = engine().await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "top",
                RuleKind::AtClock {
                    playlist_ref: "jingle".into(),
                    anchor: ClockAnchor::EveryMinutes(15),
                    mode: Mode::Soft,
                    expiry_secs: None,
                },
            ),
        )
        .await
        .unwrap();

        // 09:15 → the :15 mark fires.
        let first = eng.next(at(9, 15)).await.unwrap();
        assert_eq!(first.origin, Origin::AtClockSoft);
        // Same mark again (09:16 still floors to :15) → already consumed → base.
        let second = eng.next(at(9, 16)).await.unwrap();
        assert_eq!(second.origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn preview_projects_base_daypart_and_marks() {
        let (_dir, eng) = engine().await; // tz = UTC
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "mid",
                RuleKind::DayPart {
                    playlist_ref: "jazz".into(),
                    start: crate::resolver::WallClock { hour: 9, minute: 0 },
                    end: crate::resolver::WallClock { hour: 10, minute: 0 },
                },
            ),
        )
        .await
        .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "top",
                RuleKind::AtClock {
                    playlist_ref: "jingle".into(),
                    anchor: ClockAnchor::EveryMinutes(30),
                    mode: Mode::Soft,
                    expiry_secs: None,
                },
            ),
        )
        .await
        .unwrap();
        // An Every rule must NOT appear in a projection.
        insert_rule(
            &eng.pool,
            &rule("cool", RuleKind::Every { playlist_ref: "never".into(), cadence: Cadence::Tracks(1) }),
        )
        .await
        .unwrap();

        // Window 08:00 → 11:00 (UTC, so wall == epoch hour).
        let occ = eng.preview(at(8, 0), 3 * 3600).await.unwrap();

        // First occurrence starts exactly at `from`.
        assert_eq!(occ.first().unwrap().epoch, at(8, 0));
        // No two consecutive occurrences share the same decision (change-only).
        for w in occ.windows(2) {
            assert_ne!(
                (&w[0].origin, &w[0].playlist_ref),
                (&w[1].origin, &w[1].playlist_ref),
                "consecutive occurrences must differ"
            );
        }
        // The DayPart shows up as jazz, the marks as jingle, and Every never.
        assert!(occ.iter().any(|o| o.origin == Origin::DayPart && o.playlist_ref == "jazz"));
        assert!(occ.iter().any(|o| o.origin == Origin::AtClockSoft && o.playlist_ref == "jingle"));
        assert!(occ.iter().all(|o| o.origin != Origin::Every));
        // A mark is an instant, not a segment: the 08:30 jingle is followed by
        // a return to the floor at 08:31.
        let mark = occ.iter().position(|o| o.epoch == at(8, 30)).expect("08:30 mark");
        assert_eq!(occ[mark].origin, Origin::AtClockSoft);
        assert_eq!(occ[mark + 1].epoch, at(8, 31));
        assert_eq!(occ[mark + 1].origin, Origin::BaseRotation);
    }

    #[tokio::test]
    async fn preview_of_a_bare_floor_is_a_single_segment() {
        let (_dir, eng) = engine().await;
        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "general".into() }))
            .await
            .unwrap();
        let occ = eng.preview(at(0, 0), 6 * 3600).await.unwrap();
        assert_eq!(occ.len(), 1, "nothing changes → one segment");
        assert_eq!(occ[0].origin, Origin::BaseRotation);
        assert_eq!(occ[0].playlist_ref, "general");
    }
}
