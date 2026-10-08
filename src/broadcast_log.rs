//! Family (B) — station-wide broadcast history, against the `broadcast_log`
//! table of migration 0011. The SQL pendant of "a track started at T".
//!
//! Written when the grid engine chooses a track (`next_media`, overrides,
//! hard rendez-vous), with its provenance (rule, origin, playlist, leaf) and
//! stamped `aired_at` when Liquidsoap really starts it (migration 0020); read by the
//! selection stage (`apply_constraints`) to drop candidates that played within
//! an anti-repetition window (`no_same_track_within` / `no_same_artist_within`).
//!
//! Durability contract (family B): append-only, no FK, addressed by identity
//! (media UUID / artist); rel_path remains the historical locator. Never reset by a scan or apply. Epoch UTC.
//! Like the other family-B modules, each function does one thing, holds no
//! business logic, and knows nothing of the proto.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::resolver::Epoch;

/// Where a logged track came from (migration 0020). `origin` is the short
/// label: `AtClockHard`, `AtClockSoft`, `Every`, `DayPart`, `BaseRotation`,
/// `Override`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Provenance<'a> {
    pub rule_id: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub playlist_ref: Option<&'a str>,
    pub leaf_ref: Option<&'a str>,
}

/// Record that a track was CHOSEN to start at `played_at` (the anti-repetition
/// windows count it from then). `artist` is the media's artist tag (`None` =
/// untagged — it will never match an artist window). Returns the row id, to
/// stamp the real air start later (`mark_aired`).
pub async fn record(
    pool: &SqlitePool,
    rel_path: &str,
    artist: Option<&str>,
    played_at: Epoch,
    from: Provenance<'_>,
) -> Result<i64, sqlx::Error> {
    let id = crate::media_identity::ensure(pool, rel_path).await?;
    let r = sqlx::query(
        "INSERT INTO broadcast_log (rel_path, artist, played_at, rule_id, origin, playlist_ref, leaf_ref, media_uuid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(rel_path)
    .bind(artist)
    .bind(played_at.0)
    .bind(from.rule_id)
    .bind(from.origin)
    .bind(from.playlist_ref)
    .bind(from.leaf_ref)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(r.last_insert_rowid())
}

/// Song keys (`media_index::song_keys`) of everything chosen at or after
/// `cutoff` — the set to avoid for a `no_same_title_within` window. The title
/// comes from the media index (a media no longer indexed keeps its file-name
/// key).
pub async fn song_keys_since(pool: &SqlitePool, cutoff: i64) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT coalesce(i.uri, b.rel_path), m.title FROM broadcast_log b
         LEFT JOIN media m ON m.media_uuid = b.media_uuid
         LEFT JOIN media_identity i ON i.uuid = b.media_uuid
         WHERE b.played_at >= ?1",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .flat_map(|(p, t)| crate::media_index::song_keys(p, t.as_deref()))
        .collect())
}

/// Liquidsoap really started the track logged as `id` at `at`.
pub async fn mark_aired(pool: &SqlitePool, id: i64, at: Epoch) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE broadcast_log SET aired_at = ?2 WHERE id = ?1 AND aired_at IS NULL")
        .bind(id)
        .bind(at.0)
        .execute(pool)
        .await?;
    Ok(())
}

/// Our track logged as `id` left the air at `at` (migration 0022):
/// `played_to_end` = `Some(true)` it went to its end, `Some(false)` it was
/// cut, `None` unknown (duration not indexed). First report wins — a track
/// leaves the air once.
pub async fn record_left(
    pool: &SqlitePool,
    id: i64,
    at: Epoch,
    played_to_end: Option<bool>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE broadcast_log SET left_at = ?2, played_to_end = ?3 WHERE id = ?1 AND left_at IS NULL")
        .bind(id)
        .bind(at.0)
        .bind(played_to_end)
        .execute(pool)
        .await?;
    Ok(())
}

/// One logged track with its provenance, its air stamps and what the media
/// index knows of it (`title`… `None` = not indexed / tag absent).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LogRow {
    pub id: i64,
    pub rel_path: String,
    pub played_at: i64,
    pub aired_at: Option<i64>,
    pub left_at: Option<i64>,
    pub played_to_end: Option<bool>,
    pub rule_id: Option<String>,
    pub origin: Option<String>,
    pub playlist_ref: Option<String>,
    pub leaf_ref: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
}

const LOG_ROW_SELECT: &str = "SELECT b.id, b.rel_path, b.played_at, b.aired_at, b.left_at, b.played_to_end,
            b.rule_id, b.origin, b.playlist_ref, b.leaf_ref,
            m.title, coalesce(m.artist, b.artist) AS artist, m.album, m.duration_ms
     FROM broadcast_log b LEFT JOIN media m ON m.media_uuid = b.media_uuid";

/// The logged track `id`, if any.
pub async fn row(pool: &SqlitePool, id: i64) -> Result<Option<LogRow>, sqlx::Error> {
    sqlx::query_as(&format!("{LOG_ROW_SELECT} WHERE b.id = ?1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Tracks REALLY aired (`aired_at` set) strictly before `before`, most recent
/// first, at most `limit` — the on-air history. A track chosen but never
/// started (prepared then flushed) is not history.
pub async fn aired_before(pool: &SqlitePool, before: i64, limit: u32) -> Result<Vec<LogRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "{LOG_ROW_SELECT} WHERE b.aired_at IS NOT NULL AND b.aired_at < ?1
         ORDER BY b.aired_at DESC, b.id DESC LIMIT ?2"
    ))
    .bind(before)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// What `plays` groups the history by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaysBy {
    /// The rule's / override's playlist (a group counts as the group).
    Playlist,
    /// The leaf playlist that produced the file (a group's member).
    Leaf,
    Rule,
    Origin,
    Media,
    Artist,
}

impl PlaysBy {
    fn column(self) -> &'static str {
        match self {
            PlaysBy::Playlist => "playlist_ref",
            PlaysBy::Leaf => "leaf_ref",
            PlaysBy::Rule => "rule_id",
            PlaysBy::Origin => "origin",
            PlaysBy::Media => "rel_path",
            PlaysBy::Artist => "artist",
        }
    }
}

/// One line of `plays`: `key` `None` = unknown (rows older than migration
/// 0020, an override without a rule, an untagged artist…).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaysRow {
    pub key: Option<String>,
    /// Chosen by stationd (every logged row).
    pub picked: u64,
    /// Really started by Liquidsoap (`aired_at` set).
    pub aired: u64,
    /// Last choice (epoch UTC).
    pub last_at: i64,
}

/// Plays since `cutoff` (epoch UTC), grouped `by`, most aired first (then
/// most picked, then key); at most `limit` lines (0 = all).
pub async fn plays(
    pool: &SqlitePool,
    cutoff: i64,
    by: PlaysBy,
    limit: u32,
) -> Result<Vec<PlaysRow>, sqlx::Error> {
    let col = by.column();
    let (key, group, source) = if matches!(by, PlaysBy::Media) {
        ("i.uri", "b.media_uuid", "broadcast_log b JOIN media_identity i ON i.uuid = b.media_uuid")
    } else {
        (col, col, "broadcast_log")
    };
    let limit = if limit == 0 { -1 } else { i64::from(limit) };
    let rows: Vec<(Option<String>, i64, i64, i64)> = sqlx::query_as(&format!(
        "SELECT {key}, count(*), count(aired_at), max(played_at)
         FROM {source} WHERE played_at >= ?1
         GROUP BY {group}
         ORDER BY count(aired_at) DESC, count(*) DESC, {key}
         LIMIT ?2"
    ))
    .bind(cutoff)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(key, picked, aired, last_at)| PlaysRow {
            key,
            picked: picked as u64,
            aired: aired as u64,
            last_at,
        })
        .collect())
}

/// Plays of ONE key of the `by` grouping since `cutoff` (e.g. one media with
/// `PlaysBy::Media`): `None` when it was never picked in the window.
pub async fn plays_of(
    pool: &SqlitePool,
    cutoff: i64,
    by: PlaysBy,
    key: &str,
) -> Result<Option<PlaysRow>, sqlx::Error> {
    let col = by.column();
    let predicate = if matches!(by, PlaysBy::Media) {
        "media_uuid = (SELECT uuid FROM media_identity WHERE uri = ?2 OR uuid = ?2)".to_string()
    } else {
        format!("{col} = ?2")
    };
    let row: (i64, i64, Option<i64>) = sqlx::query_as(&format!(
        "SELECT count(*), count(aired_at), max(played_at)
         FROM broadcast_log WHERE played_at >= ?1 AND {predicate}"
    ))
    .bind(cutoff)
    .bind(key)
    .fetch_one(pool)
    .await?;
    Ok(match row {
        (0, ..) | (_, _, None) => None,
        (picked, aired, Some(last_at)) => Some(PlaysRow {
            key: Some(key.to_string()),
            picked: picked as u64,
            aired: aired as u64,
            last_at,
        }),
    })
}

/// Distinct `rel_path`s that started at or after `cutoff` (epoch UTC) — the set
/// to exclude for a `no_same_track_within` window (`cutoff = now - window`).
pub async fn tracks_since(pool: &SqlitePool, cutoff: i64) -> Result<HashSet<String>, sqlx::Error> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT i.uri FROM broadcast_log b JOIN media_identity i ON i.uuid = b.media_uuid WHERE b.played_at >= ?1")
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
        record(&pool, "a.mp3", Some("X"), Epoch(1_000), Provenance::default()).await.unwrap();
        record(&pool, "b.mp3", None, Epoch(2_000), Provenance::default()).await.unwrap();

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
        record(&pool, "a.mp3", Some("X"), Epoch(1_000), Provenance::default()).await.unwrap();
        record(&pool, "b.mp3", None, Epoch(1_000), Provenance::default()).await.unwrap(); // untagged
        let artists = artists_since(&pool, 0).await.unwrap();
        assert_eq!(artists.len(), 1, "NULL artist is not a constraint");
        assert!(artists.contains("X"));
    }

    #[tokio::test]
    async fn plays_group_by_provenance_and_count_aired_apart() {
        let (_d, pool) = fresh_db().await;
        let from = |rule, origin, pl, leaf| Provenance {
            rule_id: rule,
            origin: Some(origin),
            playlist_ref: Some(pl),
            leaf_ref: Some(leaf),
        };
        let a = record(&pool, "j.mp3", None, Epoch(1_000), from(Some("OneHit"), "Every", "one_hit", "onehit/jingleshit"))
            .await
            .unwrap();
        let b = record(&pool, "h.mp3", Some("X"), Epoch(1_010), from(Some("OneHit"), "Every", "one_hit", "onehit/hit"))
            .await
            .unwrap();
        record(&pool, "m.mp3", Some("X"), Epoch(1_020), from(Some("floor"), "BaseRotation", "rotation", "rotation"))
            .await
            .unwrap();
        record(&pool, "old.mp3", None, Epoch(1_030), Provenance::default()).await.unwrap();
        mark_aired(&pool, a, Epoch(1_001)).await.unwrap();
        mark_aired(&pool, b, Epoch(1_011)).await.unwrap();
        mark_aired(&pool, b, Epoch(9_999)).await.unwrap(); // first stamp wins

        let by_pl = plays(&pool, 0, PlaysBy::Playlist, 0).await.unwrap();
        assert_eq!(
            by_pl,
            vec![
                PlaysRow { key: Some("one_hit".into()), picked: 2, aired: 2, last_at: 1_010 },
                PlaysRow { key: None, picked: 1, aired: 0, last_at: 1_030 },
                PlaysRow { key: Some("rotation".into()), picked: 1, aired: 0, last_at: 1_020 },
            ]
        );
        let by_leaf = plays(&pool, 0, PlaysBy::Leaf, 0).await.unwrap();
        assert_eq!(by_leaf.iter().filter(|r| r.key.as_deref().is_some_and(|k| k.starts_with("onehit/"))).count(), 2);
        let by_rule = plays(&pool, 1_015, PlaysBy::Rule, 0).await.unwrap();
        assert_eq!(by_rule.len(), 2, "window: floor + unknown only");
        assert_eq!(plays(&pool, 0, PlaysBy::Artist, 1).await.unwrap().len(), 1, "limit");

        // One key only: the media `h.mp3`, and a key never picked.
        let h = plays_of(&pool, 0, PlaysBy::Media, "h.mp3").await.unwrap();
        assert_eq!(h, Some(PlaysRow { key: Some("h.mp3".into()), picked: 1, aired: 1, last_at: 1_010 }));
        assert_eq!(plays_of(&pool, 1_015, PlaysBy::Media, "h.mp3").await.unwrap(), None, "outside the window");
        assert_eq!(plays_of(&pool, 0, PlaysBy::Media, "nope.mp3").await.unwrap(), None);
        let aired_at: Option<i64> = sqlx::query_scalar("SELECT aired_at FROM broadcast_log WHERE id = ?1")
            .bind(b)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(aired_at, Some(1_011));
    }

    #[tokio::test]
    async fn history_lists_aired_tracks_with_their_end() {
        let (_d, pool) = fresh_db().await;
        let pv = Provenance { rule_id: Some("r"), origin: Some("DayPart"), playlist_ref: Some("soir"), leaf_ref: Some("soir") };
        let a = record(&pool, "a.mp3", Some("A"), Epoch(100), pv).await.unwrap();
        let b = record(&pool, "b.mp3", None, Epoch(200), pv).await.unwrap();
        let _never = record(&pool, "c.mp3", None, Epoch(300), pv).await.unwrap(); // chosen, never aired
        mark_aired(&pool, a, Epoch(101)).await.unwrap();
        mark_aired(&pool, b, Epoch(201)).await.unwrap();
        record_left(&pool, a, Epoch(199), Some(true)).await.unwrap();
        record_left(&pool, a, Epoch(500), Some(false)).await.unwrap(); // first report wins
        record_left(&pool, b, Epoch(250), Some(false)).await.unwrap();

        let h = aired_before(&pool, i64::MAX, 10).await.unwrap();
        assert_eq!(h.iter().map(|r| r.rel_path.as_str()).collect::<Vec<_>>(), vec!["b.mp3", "a.mp3"]);
        assert_eq!((h[1].left_at, h[1].played_to_end), (Some(199), Some(true)));
        assert_eq!(h[0].played_to_end, Some(false));
        assert_eq!(h[1].artist.as_deref(), Some("A"), "artist from the log when not indexed");
        assert_eq!(h[0].rule_id.as_deref(), Some("r"));
        assert_eq!(aired_before(&pool, 201, 10).await.unwrap().len(), 1, "strictly before");
        assert_eq!(row(&pool, b).await.unwrap().unwrap().aired_at, Some(201));
    }
}
/// Current locator of a logged UUID (historical rel_path remains untouched).
pub async fn current_uri(pool: &SqlitePool, id: i64) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT i.uri FROM broadcast_log b JOIN media_identity i ON i.uuid = b.media_uuid WHERE b.id = ?1")
        .bind(id).fetch_optional(pool).await
}
