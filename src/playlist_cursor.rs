//! Playlist traversal cursor (family B — durable playback state).
//!
//! Records the last file a playlist handed out, so a `sequential`/`newest`/
//! `oldest` playlist advances across calls — and across restarts — instead of
//! replaying the same track. Same store shape as `grid_store`: plain SQL over
//! a `&SqlitePool`, no business logic (that's in `selection`).
//!
//! Keyed by playlist ref (the grid's `playlist_ref`, i.e. the view's
//! `rel_path`). The stored identity is the last media UUID handed out, not an
//! index, so the cursor survives a pool that grows or shrinks between turns.

use sqlx::SqlitePool;

/// The last file this playlist handed out, if any.
pub async fn get(pool: &SqlitePool, reference: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT i.uri FROM playlist_cursor c JOIN media_identity i ON i.uuid = c.media_uuid WHERE c.playlist_ref = ?1")
            .bind(reference)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(p,)| p))
}

/// Record `rel_path` as the last file handed out by `reference`.
pub async fn set(pool: &SqlitePool, reference: &str, rel_path: &str) -> Result<(), sqlx::Error> {
    let now = now_epoch_seconds();
    let id = crate::media_identity::ensure(pool, rel_path).await?;
    sqlx::query(
        "INSERT INTO playlist_cursor (playlist_ref, last_rel_path, updated_at, media_uuid)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(playlist_ref) DO UPDATE SET last_rel_path = ?2, updated_at = ?3, media_uuid = ?4",
    )
    .bind(reference)
    .bind(rel_path)
    .bind(now)
    .bind(id)
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
/// Internal cursor identity, independent of its current locator.
pub async fn get_id(pool: &SqlitePool, reference: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT media_uuid FROM playlist_cursor WHERE playlist_ref = ?1")
        .bind(reference).fetch_optional(pool).await
}
