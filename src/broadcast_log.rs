//! Family (B) — station-wide broadcast history, against the `broadcast_log`
//! table of migration 0011. The SQL pendant of "a track started at T".
//!
//! Written at each track start by the grid engine (`next_media`); read by the
//! selection stage (`apply_constraints`) to drop candidates that played within
//! an anti-repetition window (`no_same_track_within` / `no_same_artist_within`).
//!
//! Durability contract (family B): append-only, no FK, addressed by identity
//! (rel_path / artist), never reset by a scan or an apply. Epoch UTC.
//! Like the other family-B modules, each function does one thing, holds no
//! business logic, and knows nothing of the proto.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::resolver::Epoch;

/// Record that a track STARTED at `played_at`. `artist` is the media's artist
/// tag (`None` = untagged — it will never match an artist window).
pub async fn record(
    pool: &SqlitePool,
    rel_path: &str,
    artist: Option<&str>,
    played_at: Epoch,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO broadcast_log (rel_path, artist, played_at) VALUES (?1, ?2, ?3)")
        .bind(rel_path)
        .bind(artist)
        .bind(played_at.0)
        .execute(pool)
        .await?;
    Ok(())
}

/// Distinct `rel_path`s that started at or after `cutoff` (epoch UTC) — the set
/// to exclude for a `no_same_track_within` window (`cutoff = now - window`).
pub async fn tracks_since(pool: &SqlitePool, cutoff: i64) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT rel_path FROM broadcast_log WHERE played_at >= ?1")
            .bind(cutoff)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
}

/// Distinct non-null artists that started at or after `cutoff` — the set to
/// exclude for a `no_same_artist_within` window. Untagged plays (NULL artist)
/// are ignored: they never constrain anything.
pub async fn artists_since(pool: &SqlitePool, cutoff: i64) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT artist FROM broadcast_log WHERE artist IS NOT NULL AND played_at >= ?1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(a,)| a).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn records_and_windows_by_time() {
        let (_d, pool) = fresh_db().await;
        record(&pool, "a.mp3", Some("X"), Epoch(1_000)).await.unwrap();
        record(&pool, "b.mp3", None, Epoch(2_000)).await.unwrap();

        // cutoff 1_500 → only b.mp3 (played at 2_000) is "recent".
        let tracks = tracks_since(&pool, 1_500).await.unwrap();
        assert_eq!(tracks.len(), 1);
        assert!(tracks.contains("b.mp3"));

        // cutoff 500 → both.
        assert_eq!(tracks_since(&pool, 500).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn artists_ignore_untagged_plays() {
        let (_d, pool) = fresh_db().await;
        record(&pool, "a.mp3", Some("X"), Epoch(1_000)).await.unwrap();
        record(&pool, "b.mp3", None, Epoch(1_000)).await.unwrap(); // untagged
        let artists = artists_since(&pool, 0).await.unwrap();
        assert_eq!(artists.len(), 1, "NULL artist is not a constraint");
        assert!(artists.contains("X"));
    }
}
