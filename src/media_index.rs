//! Media library view persistence (family A), against the `media` and
//! `media_genre` tables of migration 0007.
//!
//! The SQL pendant of `media::scan_library`: the pure scanner produces
//! `ScannedMedia`, this module reconciles that snapshot into the DB. Like the
//! other `*_index`/`store` modules, each function takes a `&SqlitePool`, holds
//! no business logic and knows nothing of the proto — the gRPC handler is a
//! thin translator on top.
//!
//! Reconciliation model (not DROP+rebuild): a scan flips every row to
//! `available = 0`, then re-affirms `available = 1` for each file it saw, all
//! in one transaction. A file that vanished stays known but unavailable — we
//! keep the distinction between "never seen" and "seen then gone", which a
//! plain rebuild would lose. Family (A): rebuildable, and the durable history
//! (family B) addresses media by identity, never by FK, so a rebuild is safe.

use sqlx::SqlitePool;

use crate::media::ScannedMedia;

/// One row of the media view, as returned by [`list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRow {
    pub rel_path: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub duration_ms: i64,
    pub size_bytes: i64,
    pub available: bool,
    pub genres: Vec<String>,
}

/// Summary of a reconciliation, for the scan report / CLI output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaceStats {
    /// Rows available after this scan (files present on disk).
    pub present: usize,
    /// Rows still known but now absent (available = 0).
    pub unavailable: usize,
}

/// Reconcile a full scan snapshot into the view, in one transaction:
/// 1. every row → `available = 0`;
/// 2. upsert each scanned file (re-affirm `available = 1`, refresh tags,
///    replace its genre set);
/// 3. count the two populations.
///
/// A file that was in the DB but not in `scanned` is left at `available = 0`
/// (marked unavailable, not deleted). Returns the two counts.
pub async fn replace_library(
    pool: &SqlitePool,
    scanned: &[ScannedMedia],
    scanned_at: i64,
) -> Result<ReplaceStats, sqlx::Error> {
    let mut tx = pool.begin().await?;

    // 1. Nothing is available until this scan re-affirms it.
    sqlx::query("UPDATE media SET available = 0")
        .execute(&mut *tx)
        .await?;

    // 2. Upsert every file the scan saw.
    for m in scanned {
        sqlx::query(
            "INSERT INTO media
                 (rel_path, title, artist, album, year, duration_ms, size_bytes, mtime_ns, available, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9)
             ON CONFLICT(rel_path) DO UPDATE SET
                 title = ?2, artist = ?3, album = ?4, year = ?5,
                 duration_ms = ?6, size_bytes = ?7, mtime_ns = ?8,
                 available = 1, scanned_at = ?9",
        )
        .bind(&m.rel_path)
        .bind(&m.title)
        .bind(&m.artist)
        .bind(&m.album)
        .bind(m.year.map(|y| y as i64))
        .bind(m.duration_ms as i64)
        .bind(m.size_bytes as i64)
        .bind(m.mtime_ns)
        .bind(scanned_at)
        .execute(&mut *tx)
        .await?;

        // Replace the genre set for this path (no FK cascade relied upon).
        sqlx::query("DELETE FROM media_genre WHERE rel_path = ?1")
            .bind(&m.rel_path)
            .execute(&mut *tx)
            .await?;
        for g in &m.genres {
            sqlx::query(
                "INSERT OR IGNORE INTO media_genre (rel_path, genre, genre_key) VALUES (?1, ?2, ?3)",
            )
                .bind(&m.rel_path)
                .bind(g)
                .bind(genre_key(g))
                .execute(&mut *tx)
                .await?;
        }
    }

    // 3. Populations for the report.
    let (present,): (i64,) = sqlx::query_as("SELECT count(*) FROM media WHERE available = 1")
        .fetch_one(&mut *tx)
        .await?;
    let (unavailable,): (i64,) = sqlx::query_as("SELECT count(*) FROM media WHERE available = 0")
        .fetch_one(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(ReplaceStats {
        present: present as usize,
        unavailable: unavailable as usize,
    })
}

/// Case-folding key for genre comparison: trimmed, Unicode lowercase. Done in
/// Rust rather than with SQLite `COLLATE NOCASE` / `lower()`, which only fold
/// ASCII — "Électro" and "électro" must land in the same bucket. Stored at
/// scan time in `media_genre.genre_key` (migration 0015), which the playlist
/// `genre` filters compare against: single source of truth for the fold.
pub fn genre_key(genre: &str) -> String {
    genre.trim().to_lowercase()
}

/// List media rows ordered by `rel_path`. When `only_available` is true, rows
/// marked unavailable (vanished from disk) are excluded. `genres` filters
/// case-insensitively (see [`genre_key`]): a row is kept when it carries AT
/// LEAST ONE of them (`any`); an empty slice means no genre filter. Genres are
/// fetched per row (N+1, fine for a CLI listing; a JOIN can replace it if it
/// matters). Blank filter entries are the caller's job to reject.
pub async fn list(
    pool: &SqlitePool,
    only_available: bool,
    genres: &[String],
) -> Result<Vec<MediaRow>, sqlx::Error> {
    let sql = if only_available {
        "SELECT rel_path, title, artist, album, year, duration_ms, size_bytes, available
         FROM media WHERE available = 1 ORDER BY rel_path"
    } else {
        "SELECT rel_path, title, artist, album, year, duration_ms, size_bytes, available
         FROM media ORDER BY rel_path"
    };

    let wanted: std::collections::HashSet<String> = genres.iter().map(|g| genre_key(g)).collect();

    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(sql).fetch_all(pool).await?;

    let mut out = Vec::with_capacity(rows.len());
    for (rel_path, title, artist, album, year, duration_ms, size_bytes, available) in rows {
        let genres: Vec<String> =
            sqlx::query_as::<_, (String,)>("SELECT genre FROM media_genre WHERE rel_path = ?1 ORDER BY genre")
                .bind(&rel_path)
                .fetch_all(pool)
                .await?
                .into_iter()
                .map(|(g,)| g)
                .collect();

        if !wanted.is_empty() && !genres.iter().any(|g| wanted.contains(&genre_key(g))) {
            continue;
        }

        out.push(MediaRow {
            rel_path,
            title,
            artist,
            album,
            year,
            duration_ms,
            size_bytes,
            available: available != 0,
            genres,
        });
    }
    Ok(out)
}

/// One genre bucket of the inventory: case-folded, with every spelling seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreCount {
    /// Display label: the most frequent spelling (ties → smallest string).
    pub genre: String,
    /// Distinct media carrying this genre (any spelling), counted once each.
    pub count: usize,
    /// Every distinct spelling folded into this bucket, sorted. More than one
    /// = inconsistent tags ("Jazz" / "jazz") worth fixing at the source.
    pub spellings: Vec<String>,
}

/// Genre inventory of the library, for `library genres`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GenreInventory {
    /// Buckets sorted by case-folded key.
    pub genres: Vec<GenreCount>,
    /// Media carrying no genre at all.
    pub untagged: usize,
}

/// Count media per genre, case-insensitively (see [`genre_key`]). Same
/// `only_available` scope as [`list`].
pub async fn genres(pool: &SqlitePool, only_available: bool) -> Result<GenreInventory, sqlx::Error> {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    let (pairs_sql, untagged_sql) = if only_available {
        (
            "SELECT g.genre, g.rel_path FROM media_genre g
             JOIN media m ON m.rel_path = g.rel_path WHERE m.available = 1",
            "SELECT count(*) FROM media m WHERE m.available = 1
             AND NOT EXISTS (SELECT 1 FROM media_genre g WHERE g.rel_path = m.rel_path)",
        )
    } else {
        (
            "SELECT g.genre, g.rel_path FROM media_genre g
             JOIN media m ON m.rel_path = g.rel_path",
            "SELECT count(*) FROM media m
             WHERE NOT EXISTS (SELECT 1 FROM media_genre g WHERE g.rel_path = m.rel_path)",
        )
    };

    let pairs: Vec<(String, String)> = sqlx::query_as(pairs_sql).fetch_all(pool).await?;
    let (untagged,): (i64,) = sqlx::query_as(untagged_sql).fetch_one(pool).await?;

    // key → (media set, spelling → occurrences)
    let mut buckets: BTreeMap<String, (BTreeSet<String>, HashMap<String, usize>)> = BTreeMap::new();
    for (genre, rel_path) in pairs {
        let entry = buckets.entry(genre_key(&genre)).or_default();
        entry.0.insert(rel_path);
        *entry.1.entry(genre).or_default() += 1;
    }

    let genres = buckets
        .into_values()
        .map(|(media, spellings)| {
            let label = spellings
                .iter()
                .max_by(|(a, na), (b, nb)| na.cmp(nb).then_with(|| b.cmp(a)))
                .map(|(s, _)| s.clone())
                .unwrap_or_default();
            let mut all: Vec<String> = spellings.into_keys().collect();
            all.sort();
            GenreCount { genre: label, count: media.len(), spellings: all }
        })
        .collect();

    Ok(GenreInventory { genres, untagged: untagged as usize })
}

/// Flip a media row to unavailable — e.g. its file vanished from disk between
/// scans and we found out at resolution time. Idempotent; a no-op if the path
/// is unknown. Keeps the index honest without waiting for the next full scan.
pub async fn mark_unavailable(pool: &SqlitePool, rel_path: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE media SET available = 0 WHERE rel_path = ?1")
        .bind(rel_path)
        .execute(pool)
        .await?;
    Ok(())
}

/// The stored artist tag of a media row (`None` = untagged, or the path is
/// unknown). Used to stamp a track start into the broadcast history so the
/// anti-repetition `no_same_artist_within` window has an artist to match.
pub async fn artist_of(pool: &SqlitePool, rel_path: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT artist FROM media WHERE rel_path = ?1")
            .bind(rel_path)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(a,)| a))
}

/// The `(size_bytes, mtime_ns)` of a media row, or `None` if the path is
/// unknown. Captured at episode completion as the `unplayed_only` play-once
/// guard (a later file change invalidates the mark).
pub async fn size_mtime_of(
    pool: &SqlitePool,
    rel_path: &str,
) -> Result<Option<(i64, i64)>, sqlx::Error> {
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT size_bytes, mtime_ns FROM media WHERE rel_path = ?1")
            .bind(rel_path)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::media::ScannedMedia;

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("test.db");
        let pool = db::init(&path).await.expect("init + migrations");
        (dir, pool)
    }

    fn sample(rel: &str, genres: &[&str]) -> ScannedMedia {
        ScannedMedia {
            rel_path: rel.to_string(),
            title: Some("T".into()),
            artist: Some("A".into()),
            album: None,
            year: Some(2020),
            genres: genres.iter().map(|s| s.to_string()).collect(),
            duration_ms: 180_000,
            size_bytes: 4_200_000,
            mtime_ns: 1_700_000_000_000_000_000,
        }
    }

    #[tokio::test]
    async fn replace_then_list_roundtrip() {
        let (_dir, pool) = fresh_db().await;
        let scanned = vec![
            sample("a/one.mp3", &["pop", "dance"]),
            sample("b/two.flac", &[]),
        ];
        let stats = replace_library(&pool, &scanned, 1000).await.unwrap();
        assert_eq!(stats.present, 2);
        assert_eq!(stats.unavailable, 0);

        let rows = list(&pool, true, &[]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].rel_path, "a/one.mp3");
        assert_eq!(rows[0].genres, vec!["dance".to_string(), "pop".to_string()]);
        assert_eq!(rows[0].year, Some(2020));
        assert!(rows[1].genres.is_empty());
    }

    #[tokio::test]
    async fn vanished_file_is_marked_unavailable_not_deleted() {
        let (_dir, pool) = fresh_db().await;
        replace_library(&pool, &[sample("keep.mp3", &[]), sample("gone.mp3", &[])], 1000)
            .await
            .unwrap();

        // Second scan no longer sees gone.mp3.
        let stats = replace_library(&pool, &[sample("keep.mp3", &[])], 2000)
            .await
            .unwrap();
        assert_eq!(stats.present, 1);
        assert_eq!(stats.unavailable, 1, "the vanished file is kept, marked unavailable");

        // It is filtered out of the available listing but still on record.
        assert_eq!(list(&pool, true, &[]).await.unwrap().len(), 1);
        assert_eq!(list(&pool, false, &[]).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn reappearing_file_flips_back_to_available() {
        let (_dir, pool) = fresh_db().await;
        replace_library(&pool, &[sample("x.mp3", &[])], 1000).await.unwrap();
        replace_library(&pool, &[], 2000).await.unwrap(); // gone
        let stats = replace_library(&pool, &[sample("x.mp3", &[])], 3000).await.unwrap(); // back
        assert_eq!(stats.present, 1);
        assert_eq!(stats.unavailable, 0);
    }

    #[tokio::test]
    async fn genre_set_is_replaced_not_accumulated() {
        let (_dir, pool) = fresh_db().await;
        replace_library(&pool, &[sample("g.mp3", &["rock", "metal"])], 1000)
            .await
            .unwrap();
        // Re-scan with a different genre set: the old ones must not linger.
        replace_library(&pool, &[sample("g.mp3", &["jazz"])], 2000)
            .await
            .unwrap();

        let rows = list(&pool, true, &[]).await.unwrap();
        assert_eq!(rows[0].genres, vec!["jazz".to_string()]);
    }

    #[tokio::test]
    async fn list_filters_by_genre_case_insensitively_any() {
        let (_dir, pool) = fresh_db().await;
        replace_library(
            &pool,
            &[
                sample("a.mp3", &["Jazz"]),
                sample("b.mp3", &["jazz"]),
                sample("c.mp3", &["Électro"]),
                sample("d.mp3", &["Rock"]),
                sample("e.mp3", &[]),
            ],
            1000,
        )
        .await
        .unwrap();

        let paths = |rows: Vec<MediaRow>| rows.into_iter().map(|r| r.rel_path).collect::<Vec<_>>();

        assert_eq!(paths(list(&pool, true, &["JAZZ".into()]).await.unwrap()), vec!["a.mp3", "b.mp3"]);
        // Unicode fold, not just ASCII.
        assert_eq!(paths(list(&pool, true, &["électro".into()]).await.unwrap()), vec!["c.mp3"]);
        // Several genres = any.
        assert_eq!(
            paths(list(&pool, true, &["rock".into(), " jazz ".into()]).await.unwrap()),
            vec!["a.mp3", "b.mp3", "d.mp3"]
        );
        assert!(list(&pool, true, &["polka".into()]).await.unwrap().is_empty());
        // No filter = everything, untagged included; genres keep their spelling.
        let all = list(&pool, true, &[]).await.unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].genres, vec!["Jazz".to_string()]);
    }

    #[tokio::test]
    async fn genre_inventory_folds_case_and_counts_untagged() {
        let (_dir, pool) = fresh_db().await;
        replace_library(
            &pool,
            &[
                sample("a.mp3", &["Jazz"]),
                sample("b.mp3", &["jazz"]),
                sample("c.mp3", &["Jazz"]),
                sample("d.mp3", &["Rock"]),
                sample("e.mp3", &[]),
                sample("f.mp3", &[]),
            ],
            1000,
        )
        .await
        .unwrap();

        let inv = genres(&pool, true).await.unwrap();
        assert_eq!(inv.untagged, 2);
        assert_eq!(inv.genres.len(), 2);
        assert_eq!(inv.genres[0].genre, "Jazz", "most frequent spelling is the label");
        assert_eq!(inv.genres[0].count, 3);
        assert_eq!(inv.genres[0].spellings, vec!["Jazz".to_string(), "jazz".to_string()]);
        assert_eq!(inv.genres[1].genre, "Rock");
        assert_eq!(inv.genres[1].spellings, vec!["Rock".to_string()]);
    }

    #[tokio::test]
    async fn genre_inventory_respects_availability() {
        let (_dir, pool) = fresh_db().await;
        replace_library(&pool, &[sample("a.mp3", &["pop"]), sample("b.mp3", &[])], 1000)
            .await
            .unwrap();
        replace_library(&pool, &[], 2000).await.unwrap(); // both vanish

        let avail = genres(&pool, true).await.unwrap();
        assert!(avail.genres.is_empty());
        assert_eq!(avail.untagged, 0);

        let all = genres(&pool, false).await.unwrap();
        assert_eq!(all.genres.len(), 1);
        assert_eq!(all.untagged, 1);
    }
}
