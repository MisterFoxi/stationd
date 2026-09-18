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
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sqlx::SqlitePool;

use crate::clock::{self, ClockError};
use crate::grid_index::{self, GridLoadError};
use crate::grid_store;
use crate::grid_toml;
use crate::resolver::{
    resolve_next, resolve_ranked, Epoch, Grid, GridDecision, LocalNow, Origin, PlaybackState,
    Rule, RuleKind,
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
    /// Media root for the on-resolution existence check. `None` (tests) turns
    /// the check off.
    media_root: Option<PathBuf>,
    /// Manual clock override for testing (`None` = real time).
    now_override: Arc<Mutex<Option<Epoch>>>,
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
        Self {
            pool,
            tz: tz.into(),
            plugins: None,
            media_root: None,
            now_override: Arc::new(Mutex::new(None)),
        }
    }

    /// Attach the plugin system so decisions are broadcast to plugins.
    pub fn with_plugins(mut self, plugins: crate::plugin::PluginHandle) -> Self {
        self.plugins = Some(plugins);
        self
    }

    /// Attach the media root so resolution can verify a chosen file still
    /// exists on disk (and flip it unavailable if not). Without it, the check
    /// is skipped.
    pub fn with_media_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.media_root = Some(root.into());
        self
    }

    /// Manual clock: freeze the instant `resolve_next` uses when the request
    /// gives no explicit `now` (`Some`), or return to real time (`None`).
    pub fn set_clock(&self, frozen: Option<Epoch>) {
        *self.now_override.lock().unwrap() = frozen;
    }

    /// The current manual-clock override, if any.
    pub fn clock_override(&self) -> Option<Epoch> {
        *self.now_override.lock().unwrap()
    }

    /// The instant to resolve at: an explicit request `now`, else the manual
    /// clock override, else real wall-clock time.
    pub fn effective_now(&self, explicit: Option<Epoch>) -> Epoch {
        explicit
            .or_else(|| *self.now_override.lock().unwrap())
            .unwrap_or_else(real_now)
    }

    /// Does the chosen media still exist under the media root? `true` when no
    /// root is configured (check off, e.g. in tests).
    fn media_exists(&self, rel_path: &str) -> bool {
        match &self.media_root {
            None => true,
            Some(root) => root.join(rel_path).exists(),
        }
    }

    /// Freeze the manual clock at a civil local time spec: "HH:MM" (today, in
    /// the station timezone) or "YYYY-MM-DD HH:MM". The daemon owns the zone,
    /// so the caller never computes an epoch.
    pub fn set_clock_civil(&self, spec: &str) -> Result<(), ClockError> {
        let epoch = self.parse_civil_spec(spec)?;
        self.set_clock(Some(epoch));
        Ok(())
    }

    fn parse_civil_spec(&self, spec: &str) -> Result<Epoch, ClockError> {
        let spec = spec.trim();
        let (date_part, time_part) = match spec.split_once(' ') {
            Some((d, t)) => (Some(d.trim()), t.trim()),
            None => (None, spec),
        };
        let (hour, minute) = parse_hh_mm(time_part)?;
        let (year, month, day) = match date_part {
            Some(dp) => parse_ymd(dp)?,
            None => {
                let today = clock::to_local_now(real_now(), &self.tz)?;
                (
                    today.date.year as i16,
                    today.date.month as i8,
                    today.date.day as i8,
                )
            }
        };
        clock::civil_to_epoch(&self.tz, year, month, day, hour, minute)
    }

    /// Render an epoch as station-local civil text (for display).
    pub fn render_local(&self, epoch: Epoch) -> Result<String, ClockError> {
        let ln = clock::to_local_now(epoch, &self.tz)?;
        Ok(fmt_local(&ln, &self.tz))
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
    /// `Every` cooldown reset). Pure resolution, durable bookkeeping. This is
    /// the source-only primitive (no media); the live path is `next_media`.
    pub async fn next(&self, now: Epoch) -> Result<GridDecision, EngineError> {
        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;

        let decision = resolve_next(local, &grid, &state);
        self.persist_effects(&decision, now).await?;
        Ok(decision)
    }

    /// Resolve the source AND the concrete media to pull, with **grid
    /// fallthrough**: ask the resolver for the applicable sources in priority
    /// order and keep the first that actually yields a media. A source whose
    /// pool is empty is skipped and we fall through to the next (down to the
    /// BaseRotation floor) — no silent gap. Side effects (a consumed AtClock
    /// mark, an `Every` reset) are persisted ONLY for the source that produced,
    /// so an empty AtClock does not burn its occurrence.
    ///
    /// - every applicable source (incl. the floor) empty → `PoolEmpty` surfaced
    ///   (legitimate dead air → Liquidsoap on-empty covers it);
    /// - no rule covers `now` at all → a `Fallback` decision, media `None`.
    pub async fn next_media(&self, now: Epoch) -> Result<ResolvedDecision, EngineError> {
        let local = clock::to_local_now(now, &self.tz)?;
        let grid = grid_index::load_grid(&self.pool).await?;
        let state = grid_store::load_playback_state(&self.pool).await?;

        let ranked = resolve_ranked(local, &grid, &state);
        let had_candidates = !ranked.is_empty();

        for decision in ranked {
            let Some(playlist_ref) = decision.playlist_ref.clone() else {
                continue;
            };
            // Bounded re-pick: a chosen file that vanished from disk (between
            // scans) is flipped unavailable in the index and we re-pick from
            // the same source. Capped so a pool of dead entries can't spin.
            const MAX_DEAD_PICKS: u32 = 32;
            let mut produced: Option<String> = None;
            for _ in 0..MAX_DEAD_PICKS {
                match crate::selection::resolve_ref_with_plugins(
                    &self.pool,
                    self.plugins.as_ref(),
                    &playlist_ref,
                )
                .await
                {
                    Ok(media) if self.media_exists(&media) => {
                        produced = Some(media);
                        break;
                    }
                    Ok(missing) => {
                        tracing::warn!(
                            media = %missing,
                            "resolved media missing on disk; marking unavailable and re-picking"
                        );
                        crate::media_index::mark_unavailable(&self.pool, &missing).await?;
                        continue;
                    }
                    Err(crate::selection::SelectionError::PoolEmpty) => break,
                    // A config error (unknown ref, unsupported order/mode, bad
                    // filter value) is not an empty pool — surface it.
                    Err(e) => return Err(EngineError::Selection(e)),
                }
            }

            if let Some(media) = produced {
                // Persist effects only now that this source actually produced.
                self.persist_effects(&decision, now).await?;
                self.emit_resolved(&decision, Some(&media));
                return Ok(ResolvedDecision { decision, media_path: Some(media) });
            }

            tracing::info!(
                rule = decision.rule_id.as_deref().unwrap_or("-"),
                playlist = %playlist_ref,
                origin = ?decision.origin,
                "grid source produced no usable media; falling through to lower priority"
            );
        }

        if had_candidates {
            // Everything applicable, down to the floor, produced nothing:
            // legitimate dead air → surface it (Liquidsoap on-empty covers).
            return Err(EngineError::Selection(
                crate::selection::SelectionError::PoolEmpty,
            ));
        }

        // No rule covered `now` at all → fallback (Liquidsoap safety net).
        let decision = GridDecision {
            origin: Origin::Fallback,
            rule_id: None,
            playlist_ref: None,
            mark_taken: None,
        };
        self.emit_resolved(&decision, None);
        Ok(ResolvedDecision { decision, media_path: None })
    }

    /// Persist a decision's side effects — a consumed AtClock occurrence and an
    /// `Every` cooldown reset. Called only once a source has actually produced
    /// (or, from `next`, for the chosen source).
    async fn persist_effects(
        &self,
        decision: &GridDecision,
        now: Epoch,
    ) -> Result<(), EngineError> {
        if let Some(token) = &decision.mark_taken {
            grid_store::record_at_clock_taken(&self.pool, token, now).await?;
        }
        if decision.origin == Origin::Every {
            if let Some(rule_id) = &decision.rule_id {
                grid_store::reset_every(&self.pool, rule_id, now).await?;
            }
        }
        Ok(())
    }

    /// Notify plugins of a decision (best-effort, fire-and-forget).
    fn emit_resolved(&self, decision: &GridDecision, media_path: Option<&str>) {
        if let Some(plugins) = &self.plugins {
            plugins.emit(crate::plugin::PluginEvent::TrackResolved {
                media_path: media_path.map(|s| s.to_string()),
                playlist_ref: decision.playlist_ref.clone(),
                rule_id: decision.rule_id.clone(),
                origin: format!("{:?}", decision.origin),
            });
        }
    }
}

fn real_now() -> Epoch {
    use std::time::{SystemTime, UNIX_EPOCH};
    Epoch(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
}

fn parse_hh_mm(s: &str) -> Result<(i8, i8), ClockError> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| ClockError::BadCivilTime(format!("expected HH:MM, got {s:?}")))?;
    let hour = h
        .trim()
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad hour in {s:?}")))?;
    let minute = m
        .trim()
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad minute in {s:?}")))?;
    Ok((hour, minute))
}

fn parse_ymd(s: &str) -> Result<(i16, i8, i8), ClockError> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(ClockError::BadCivilTime(format!("expected YYYY-MM-DD, got {s:?}")));
    }
    let year = parts[0]
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad year in {s:?}")))?;
    let month = parts[1]
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad month in {s:?}")))?;
    let day = parts[2]
        .parse()
        .map_err(|_| ClockError::BadCivilTime(format!("bad day in {s:?}")))?;
    Ok((year, month, day))
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

    #[tokio::test]
    async fn next_media_falls_through_empty_daypart_to_floor() {
        use crate::resolver::WallClock;
        let (_dir, eng) = engine().await; // tz = UTC

        // Media only under music/ — the floor can produce, the daypart cannot.
        crate::media_index::replace_library(
            &eng.pool,
            &[crate::media::ScannedMedia {
                rel_path: "music/a.mp3".into(),
                title: Some("t".into()),
                artist: None,
                album: None,
                year: None,
                genres: vec![],
                duration_ms: 1000,
                size_bytes: 1,
                mtime_ns: 0,
            }],
            1000,
        )
        .await
        .unwrap();

        for (r, prefix) in [("music", "music/"), ("jazz", "jazz/")] {
            let toml = format!(
                "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                 [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{prefix}\"\n"
            );
            let pl = crate::playlist::Playlist::parse(&toml).unwrap();
            crate::store::upsert(&eng.pool, r, &pl, &toml, Some(r)).await.unwrap();
        }

        insert_rule(&eng.pool, &rule("floor", RuleKind::BaseRotation { playlist_ref: "music".into() }))
            .await
            .unwrap();
        insert_rule(
            &eng.pool,
            &rule(
                "jazz",
                RuleKind::DayPart {
                    playlist_ref: "jazz".into(),
                    start: WallClock { hour: 8, minute: 0 },
                    end: WallClock { hour: 10, minute: 0 },
                },
            ),
        )
        .await
        .unwrap();

        // 09:00: the jazz daypart outranks the floor but its pool is empty →
        // fall through to the music floor rather than erroring.
        let r = eng.next_media(at(9, 0)).await.unwrap();
        assert_eq!(r.media_path.as_deref(), Some("music/a.mp3"));
        assert_eq!(r.decision.origin, Origin::BaseRotation);
    }
}
