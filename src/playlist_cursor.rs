//! Playlist traversal cursor (family B — durable playback state).
//!
//! Records the last file a playlist handed out, so a `sequential`/`newest`/
//! `oldest` playlist advances across calls — and across restarts — instead of
//! replaying the same track. Same store shape as `grid_store`: plain SQL over
//! a `&SqlitePool`, no business logic (that's in `selection`).
//!
//! Keyed by playlist ref (the grid's `playlist_ref`, i.e. the view's
//! `rel_path`). The stored value is the last `rel_path` handed out, not an
//! index, so the cursor survives a pool that grows or shrinks between turns.

use sqlx::SqlitePool;

/// The last file this playlist handed out, if any.
pub async fn get(pool: &SqlitePool, reference: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT last_rel_path FROM playlist_cursor WHERE playlist_ref = ?1")
            .bind(reference)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(p,)| p))
}

/// Record `rel_path` as the last file handed out by `reference`.
pub async fn set(pool: &SqlitePool, reference: &str, rel_path: &str) -> Result<(), sqlx::Error> {
    let now = now_epoch_seconds();
    sqlx::query(
        "INSERT INTO playlist_cursor (playlist_ref, last_rel_path, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(playlist_ref) DO UPDATE SET last_rel_path = ?2, updated_at = ?3",
    )
    .bind(reference)
    .bind(rel_path)
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
    async fn get_set_roundtrips_and_upserts() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();

        assert_eq!(get(&pool, "rot/x").await.unwrap(), None);
        set(&pool, "rot/x", "a.mp3").await.unwrap();
        assert_eq!(get(&pool, "rot/x").await.unwrap(), Some("a.mp3".into()));
        set(&pool, "rot/x", "b.mp3").await.unwrap();
        assert_eq!(get(&pool, "rot/x").await.unwrap(), Some("b.mp3".into()));
    }
}
