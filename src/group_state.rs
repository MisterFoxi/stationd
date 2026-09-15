//! Group traversal state (family B — durable playback state).
//!
//! A `sequence` group hands out one track per turn, so it must remember where
//! it is: the current member index and how many tracks it has already handed
//! out for that member (its `take` quota). Same store shape as
//! `playlist_cursor`/`grid_store`: plain SQL over a `&SqlitePool`, no logic.
//!
//! Keyed by the group's ref (the view's `rel_path`). Value is `(member_idx,
//! take_count)`; `(0, 0)` when unset (a fresh activation from the top).

use sqlx::SqlitePool;

/// Current `(member_idx, take_count)` for a group, or `(0, 0)` if unset.
pub async fn get(pool: &SqlitePool, group_ref: &str) -> Result<(usize, u32), sqlx::Error> {
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT member_idx, take_count FROM group_state WHERE group_ref = ?1")
            .bind(group_ref)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(i, c)| (i as usize, c as u32)).unwrap_or((0, 0)))
}

/// Persist the group's position for the next turn.
pub async fn set(
    pool: &SqlitePool,
    group_ref: &str,
    member_idx: usize,
    take_count: u32,
) -> Result<(), sqlx::Error> {
    let now = now_epoch_seconds();
    sqlx::query(
        "INSERT INTO group_state (group_ref, member_idx, take_count, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(group_ref) DO UPDATE SET member_idx = ?2, take_count = ?3, updated_at = ?4",
    )
    .bind(group_ref)
    .bind(member_idx as i64)
    .bind(take_count as i64)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn now_epoch_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn defaults_to_zero_then_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();

        assert_eq!(get(&pool, "show").await.unwrap(), (0, 0));
        set(&pool, "show", 1, 2).await.unwrap();
        assert_eq!(get(&pool, "show").await.unwrap(), (1, 2));
        set(&pool, "show", 0, 0).await.unwrap();
        assert_eq!(get(&pool, "show").await.unwrap(), (0, 0));
    }
}
