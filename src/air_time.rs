//! Family (B) — air time: wall time minus the station's halts.
//!
//! The anti-repetition windows (`no_same_track_within`, `no_same_title_within`,
//! `no_same_artist_within`) are about what a LISTENER heard. While the station
//! is halted — `sleeping` (no one listens) or `paused` (operator) — nothing of
//! the rotation airs, so the windows must not run either: a station that
//! sleeps from 23:00 to 20:00 must not wake with its 24 h windows almost spent
//! and replay yesterday's songs. The windows are therefore counted in air
//! time, and frozen during a halt.
//!
//! Mechanism: the halts are kept as intervals (`broadcast_halt`, migration
//! 0029), written by the broadcast-state writer (`station_control`) in the
//! same transaction as the state. A window of `w` seconds at `now` becomes a
//! WALL cutoff found by walking back from `now` until `w` seconds of
//! non-halted time are covered: the history queries (`broadcast_log::*_since`)
//! are unchanged, they just get an earlier cutoff when halts sit inside the
//! window. `played_at` stays wall time (history, stats, agenda).
//!
//! Not counted as a halt: `draining` (still on air) and stationd being down
//! (operator stop, crash) — only the broadcast states are tracked here.

use sqlx::{SqliteExecutor, SqlitePool};

/// Record the halt status at `at` (epoch UTC): `halted` opens a halt if none
/// is open; otherwise the open halt (if any) is closed at `at` (never before
/// its start). Idempotent both ways.
pub async fn mark<'e>(ex: impl SqliteExecutor<'e>, halted: bool, at: i64) -> Result<(), sqlx::Error> {
    if halted {
        sqlx::query(
            "INSERT INTO broadcast_halt (start_at)
             SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM broadcast_halt WHERE end_at IS NULL)",
        )
        .bind(at)
        .execute(ex)
        .await?;
    } else {
        sqlx::query("UPDATE broadcast_halt SET end_at = max(?1, start_at) WHERE end_at IS NULL")
            .bind(at)
            .execute(ex)
            .await?;
    }
    Ok(())
}

/// The wall instant from which plays count against a window of `window`
/// seconds of AIR time ending at `now` (see the module doc). Without any halt
/// in the window: `now - window`, as plain wall time.
pub async fn cutoff(pool: &SqlitePool, now: i64, window: i64) -> Result<i64, sqlx::Error> {
    let halts: Vec<(i64, Option<i64>)> = sqlx::query_as(
        "SELECT start_at, end_at FROM broadcast_halt WHERE start_at < ?1 ORDER BY start_at DESC",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(cutoff_from(&halts, now, window))
}

/// Pure core of [`cutoff`]. `halts` = `(start, end)` most recent first; an
/// open halt (`end` = `None`) runs up to `now`. Halts after `now` are ignored
/// (a manual clock set in the past).
pub fn cutoff_from(halts: &[(i64, Option<i64>)], now: i64, window: i64) -> i64 {
    let mut cursor = now;
    let mut left = window.max(0);
    for &(start, end) in halts {
        if start >= cursor {
            continue;
        }
        let end = end.unwrap_or(now).min(cursor).max(start);
        let aired = cursor - end;
        if aired >= left {
            return cursor - left;
        }
        left -= aired;
        cursor = start;
    }
    cursor.saturating_sub(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_halts_it_is_wall_time() {
        assert_eq!(cutoff_from(&[], 1000, 100), 900);
        assert_eq!(cutoff_from(&[], 1000, 0), 1000);
    }

    #[test]
    fn a_halt_inside_the_window_pushes_the_cutoff_back() {
        // Window 100 at 1000; halted 940..960 → 20 s more.
        assert_eq!(cutoff_from(&[(940, Some(960))], 1000, 100), 880);
        // Two halts.
        assert_eq!(cutoff_from(&[(940, Some(960)), (900, Some(910))], 1000, 100), 870);
    }

    #[test]
    fn a_halt_older_than_the_window_changes_nothing() {
        assert_eq!(cutoff_from(&[(100, Some(200))], 1000, 100), 900);
    }

    #[test]
    fn the_window_never_ends_inside_a_halt() {
        // 40 s of air after the halt, window 40 → exactly at the halt's end.
        assert_eq!(cutoff_from(&[(800, Some(960))], 1000, 40), 960);
        // 41 → one second before its start.
        assert_eq!(cutoff_from(&[(800, Some(960))], 1000, 41), 799);
    }

    #[test]
    fn an_open_halt_runs_to_now() {
        // Sleeping since 900: no air time since, the window is frozen.
        assert_eq!(cutoff_from(&[(900, None)], 1000, 100), 800);
        assert_eq!(cutoff_from(&[(900, None)], 5000, 100), 800);
    }

    #[test]
    fn halts_after_now_are_ignored() {
        assert_eq!(cutoff_from(&[(2000, Some(3000))], 1000, 100), 900);
        // A halt straddling `now` counts up to `now` only.
        assert_eq!(cutoff_from(&[(950, Some(3000))], 1000, 100), 850);
    }

    #[tokio::test]
    async fn mark_opens_once_and_closes() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&dir.path().join("t.db")).await.unwrap();
        mark(&pool, true, 100).await.unwrap();
        mark(&pool, true, 150).await.unwrap(); // already open: no second row
        mark(&pool, false, 200).await.unwrap();
        mark(&pool, false, 250).await.unwrap(); // nothing open: no-op
        mark(&pool, true, 300).await.unwrap();
        let rows: Vec<(i64, Option<i64>)> =
            sqlx::query_as("SELECT start_at, end_at FROM broadcast_halt ORDER BY start_at")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, [(100, Some(200)), (300, None)]);
        // Closing before the start (manual clock set back) clamps.
        mark(&pool, false, 250).await.unwrap();
        let last: (i64, Option<i64>) =
            sqlx::query_as("SELECT start_at, end_at FROM broadcast_halt ORDER BY start_at DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(last, (300, Some(300)));
        // Window 100 at 400 over halt 100..200 (300..300 is empty): 300.
        assert_eq!(cutoff(&pool, 400, 100).await.unwrap(), 300);
        assert_eq!(cutoff(&pool, 400, 250).await.unwrap(), 50);
    }
}
