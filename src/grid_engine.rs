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

use sqlx::SqlitePool;

use crate::clock::{self, ClockError};
use crate::grid_index::{self, GridLoadError};
use crate::grid_store;
use crate::resolver::{resolve_next, Epoch, GridDecision, Origin, RuleKind};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Grid(#[from] GridLoadError),
    #[error(transparent)]
    Clock(#[from] ClockError),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Live resolver over a SQLite-backed grid, in the station timezone.
pub struct GridEngine {
    pool: SqlitePool,
    /// IANA name of the station timezone (config), e.g. "Europe/Paris".
    tz: String,
}

impl GridEngine {
    pub fn new(pool: SqlitePool, tz: impl Into<String>) -> Self {
        Self { pool, tz: tz.into() }
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
}
