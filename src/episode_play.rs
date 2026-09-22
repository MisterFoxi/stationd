//! Family (B) — play-once history for `unplayed_only`, against the
//! `episode_play` table of migration 0012.
//!
//! An INFINITE-expiry cooldown (unlike `broadcast_log`, which expires by a time
//! window): "this episode has already aired in full for THIS playlist". Written
//! at the END of a full playout by the grid engine (`on_episode_finished`) —
//! never on start, so an interrupted episode stays eligible.
//!
//! Episode identity = path + a (size_bytes, mtime_ns) guard. `played_matching`
//! joins `media` and keeps only marks whose guard STILL matches the current
//! file: a changed file (size/mtime diverged) makes its mark stale and the
//! episode eligible again — the NFS-safe compromise from the docs.
//!
//! Durability contract (family B): keyed by (canonical playlist_ref, rel_path),
//! no FK, never reset by a scan or an apply.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::resolver::Epoch;

/// Mark an episode as fully played for `playlist_ref` (canonical key). Upsert:
/// re-playing refreshes the guard and timestamp. `size_bytes`/`mtime_ns` are
/// the media's values captured at completion, so a later file change is
/// detectable at read time.
pub async fn mark(
    pool: &SqlitePool,
    playlist_ref: &str,
    rel_path: &str,
    size_bytes: i64,
    mtime_ns: i64,
    played_at: Epoch,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO episode_play (playlist_ref, rel_path, size_bytes, mtime_ns, played_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(playlist_ref, rel_path) DO UPDATE SET
             size_bytes = ?3, mtime_ns = ?4, played_at = ?5",
    )
    .bind(playlist_ref)
    .bind(rel_path)
    .bind(size_bytes)
    .bind(mtime_ns)
    .bind(played_at.0)
    .execute(pool)
    .await?;
    Ok(())
}

/// The set of `rel_path`s already played for `playlist_ref` AND whose guard
/// still matches the current `media` row (same size + mtime). A mark whose file
/// changed is dropped from this set — the episode is eligible again. This is
/// the set to EXCLUDE from an `unplayed_only` pool.
pub async fn played_matching(
    pool: &SqlitePool,
    playlist_ref: &str,
) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT ep.rel_path
         FROM episode_play ep
         JOIN media m ON m.rel_path = ep.rel_path
         WHERE ep.playlist_ref = ?1
           AND ep.size_bytes = m.size_bytes
           AND ep.mtime_ns = m.mtime_ns",
    )
    .bind(playlist_ref)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::media::ScannedMedia;
    use crate::media_index;

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    fn ep(rel: &str, size: u64, mtime: i64) -> ScannedMedia {
        ScannedMedia {
            rel_path: rel.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 1000,
            size_bytes: size,
            mtime_ns: mtime,
        }
    }

    #[tokio::test]
    async fn mark_then_matching_with_intact_guard() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[ep("a.mp3", 42, 7), ep("b.mp3", 9, 1)], 1000)
            .await
            .unwrap();
        mark(&pool, "feu", "a.mp3", 42, 7, Epoch(1000)).await.unwrap();

        let played = played_matching(&pool, "feu").await.unwrap();
        assert!(played.contains("a.mp3"));
        assert!(!played.contains("b.mp3"), "unmarked episode is not played");
        // Scoped per playlist: another ref sees nothing.
        assert!(played_matching(&pool, "other").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_changed_file_drops_the_stale_mark() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[ep("a.mp3", 42, 7)], 1000)
            .await
            .unwrap();
        // Marked with a guard that no longer matches the current media (mtime).
        mark(&pool, "feu", "a.mp3", 42, 999, Epoch(1000)).await.unwrap();
        assert!(
            played_matching(&pool, "feu").await.unwrap().is_empty(),
            "a diverged guard makes the episode eligible again"
        );
    }
}
