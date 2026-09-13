//! Persistence of the grid resolver's runtime state (family B), against the
//! `every_state` and `at_clock_taken` tables of migration 0005.
//!
//! This is the SQL pendant of `resolver::PlaybackState`: `load_playback_state`
//! hydrates the in-memory working set the pure resolver reads, and the small
//! mutators below persist the side effects a decision produces. As with the
//! playlist view, these functions take a `&SqlitePool` and do one thing each;
//! no proto, no business logic — the resolver decides, this only records.
//!
//! Durability contract (family B): keyed by `rule_id` / token, never touched
//! by an apply/reload. A grid rebuild must not reset an `Every` counter.
//!
//! CALL ORDER (documented; the loop owns it, not this module):
//!   1. `ensure_every_rows` once per grid load, so every `Every` rule has a
//!      row that `bump_tracks_since` can increment even before its first play.
//!   2. On each completed station track: `bump_tracks_since` (+1 to all).
//!   3. When a decision fires an `Every` rule: `reset_every` (last_played =
//!      now, tracks_since = 0).
//!   4. When a decision carries `mark_taken` (an AtClock): `record_at_clock_taken`.

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use crate::resolver::{Epoch, EveryState, PlaybackState};

/// Hydrate the full playback state from SQLite into the shape the pure
/// resolver consumes.
pub async fn load_playback_state(pool: &SqlitePool) -> Result<PlaybackState, sqlx::Error> {
    let every_rows: Vec<(String, Option<i64>, i64)> =
        sqlx::query_as("SELECT rule_id, last_played, tracks_since FROM every_state")
            .fetch_all(pool)
            .await?;

    let mut every = HashMap::with_capacity(every_rows.len());
    for (rule_id, last_played, tracks_since) in every_rows {
        every.insert(
            rule_id,
            EveryState {
                last_played: last_played.map(Epoch),
                // Stored non-negative (CHECK) and bounded by u32 in the domain.
                tracks_since: tracks_since.max(0) as u32,
            },
        );
    }

    let token_rows: Vec<(String,)> = sqlx::query_as("SELECT token FROM at_clock_taken")
        .fetch_all(pool)
        .await?;
    let at_clock_taken: HashSet<String> = token_rows.into_iter().map(|(t,)| t).collect();

    Ok(PlaybackState { every, at_clock_taken })
}

/// Make sure a row exists for each `Every` rule id, so its `tracks_since` is
/// incremented from the first track on — not only after its first play. Called
/// once when the grid is (re)loaded. Idempotent; leaves existing counters
/// untouched (family B is never reset by a reload).
pub async fn ensure_every_rows(pool: &SqlitePool, rule_ids: &[String]) -> Result<(), sqlx::Error> {
    for rule_id in rule_ids {
        sqlx::query("INSERT OR IGNORE INTO every_state (rule_id) VALUES (?1)")
            .bind(rule_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Increment `tracks_since` for every `Every` rule by one completed station
/// track. Rows must already exist (see `ensure_every_rows`).
pub async fn bump_tracks_since(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE every_state SET tracks_since = tracks_since + 1")
        .execute(pool)
        .await?;
    Ok(())
}

/// Record that an `Every` rule just played: reset its cooldown. Upserts so a
/// never-seen rule is created in the played state.
pub async fn reset_every(
    pool: &SqlitePool,
    rule_id: &str,
    played_at: Epoch,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO every_state (rule_id, last_played, tracks_since) VALUES (?1, ?2, 0)
         ON CONFLICT(rule_id) DO UPDATE SET last_played = ?2, tracks_since = 0",
    )
    .bind(rule_id)
    .bind(played_at.0)
    .execute(pool)
    .await?;
    Ok(())
}

/// Persist that an `AtClock` occurrence was consumed, so it never fires twice.
/// The token is the resolver's `GridDecision::mark_taken`. Idempotent.
pub async fn record_at_clock_taken(
    pool: &SqlitePool,
    token: &str,
    taken_at: Epoch,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO at_clock_taken (token, taken_at) VALUES (?1, ?2)")
        .bind(token)
        .bind(taken_at.0)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.db");
        let pool = db::init(&path).await.expect("init + migrations");
        (dir, pool)
    }

    #[tokio::test]
    async fn empty_state_loads_empty() {
        let (_dir, pool) = fresh_db().await;
        let state = load_playback_state(&pool).await.unwrap();
        assert!(state.every.is_empty());
        assert!(state.at_clock_taken.is_empty());
    }

    #[tokio::test]
    async fn ensure_then_bump_counts_tracks() {
        let (_dir, pool) = fresh_db().await;
        ensure_every_rows(&pool, &["jingle".into(), "promo".into()])
            .await
            .unwrap();
        bump_tracks_since(&pool).await.unwrap();
        bump_tracks_since(&pool).await.unwrap();

        let state = load_playback_state(&pool).await.unwrap();
        assert_eq!(state.every.get("jingle").unwrap().tracks_since, 2);
        assert_eq!(state.every.get("promo").unwrap().tracks_since, 2);
        assert!(state.every.get("jingle").unwrap().last_played.is_none());
    }

    #[tokio::test]
    async fn ensure_is_idempotent_and_keeps_counters() {
        let (_dir, pool) = fresh_db().await;
        ensure_every_rows(&pool, &["jingle".into()]).await.unwrap();
        bump_tracks_since(&pool).await.unwrap();
        // A grid reload calls ensure again — the counter must survive.
        ensure_every_rows(&pool, &["jingle".into()]).await.unwrap();

        let state = load_playback_state(&pool).await.unwrap();
        assert_eq!(state.every.get("jingle").unwrap().tracks_since, 1, "reload must not reset");
    }

    #[tokio::test]
    async fn reset_every_clears_the_counter_and_stamps_last_played() {
        let (_dir, pool) = fresh_db().await;
        ensure_every_rows(&pool, &["jingle".into()]).await.unwrap();
        bump_tracks_since(&pool).await.unwrap();
        bump_tracks_since(&pool).await.unwrap();
        reset_every(&pool, "jingle", Epoch(1_700_000_000)).await.unwrap();

        let state = load_playback_state(&pool).await.unwrap();
        let s = state.every.get("jingle").unwrap();
        assert_eq!(s.tracks_since, 0);
        assert_eq!(s.last_played, Some(Epoch(1_700_000_000)));
    }

    #[tokio::test]
    async fn at_clock_taken_roundtrips_and_dedupes() {
        let (_dir, pool) = fresh_db().await;
        let token = "news@2026-03-15T08:00";
        record_at_clock_taken(&pool, token, Epoch(1000)).await.unwrap();
        // Same token again must be a no-op, not an error.
        record_at_clock_taken(&pool, token, Epoch(2000)).await.unwrap();

        let state = load_playback_state(&pool).await.unwrap();
        assert_eq!(state.at_clock_taken.len(), 1);
        assert!(state.at_clock_taken.contains(token));
    }
}
