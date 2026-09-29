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
    /// Rows still known but now absent (available = 0), all scans together.
    pub unavailable: usize,
    /// Rows available before this scan and not seen by it: what vanished NOW.
    pub vanished: usize,
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

    // 0. What was available before: the ones this scan does not see again
    //    vanished at this scan (the report tells them from older ones).
    let before: Vec<(String,)> = sqlx::query_as("SELECT rel_path FROM media WHERE available = 1")
        .fetch_all(&mut *tx)
        .await?;
    let seen: std::collections::HashSet<&str> = scanned.iter().map(|m| m.rel_path.as_str()).collect();
    let vanished = before.iter().filter(|(p,)| !seen.contains(p.as_str())).count();

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
        vanished,
    })
}

/// Forget the media that vanished from disk (`available = 0`): their rows
/// and genres. `seen_before` = only those last seen by a scan before this
/// epoch (s); `None` = all of them. The play history keeps its own path and
/// artist (titles of forgotten media are no longer shown). Returns how many
/// were forgotten.
pub async fn prune_unavailable(pool: &SqlitePool, seen_before: Option<i64>) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let cond = "available = 0 AND (?1 IS NULL OR scanned_at < ?1)";
    sqlx::query(&format!("DELETE FROM media_genre WHERE rel_path IN (SELECT rel_path FROM media WHERE {cond})"))
        .bind(seen_before)
        .execute(&mut *tx)
        .await?;
    let r = sqlx::query(&format!("DELETE FROM media WHERE {cond}")).bind(seen_before).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(r.rows_affected())
}

/// Case-folding key for genre comparison: trimmed, Unicode lowercase. Done in
/// Rust rather than with SQLite `COLLATE NOCASE` / `lower()`, which only fold
/// ASCII — "Électro" and "électro" must land in the same bucket. Stored at
/// scan time in `media_genre.genre_key` (migration 0015), which the playlist
/// `genre` filters compare against: single source of truth for the fold.
pub fn genre_key(genre: &str) -> String {
    genre.trim().to_lowercase()
}

/// Lowercase, every non-alphanumeric run (`_`, `-`, punctuation, spaces)
/// folded to one space, trimmed. Unicode-aware (`é` stays a letter).
fn fold_words(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// The keys identifying a media's SONG for `no_same_title_within`: its title
/// tag (folded), and its file name without the extension nor a copy suffix
/// `_<n>` / ` (<n>)` (folded). Two media are the same song when their key sets
/// meet — so `Ballad/x_1.mp3`, `EpicBallad/x.mp3` and a file tagged like
/// either count as one. Empty keys are dropped.
pub fn song_keys(rel_path: &str, title: Option<&str>) -> Vec<String> {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let stem = match name.rsplit_once('.') {
        Some((s, ext)) if !s.is_empty() && !ext.contains(' ') => s,
        _ => name,
    };
    let stem = stem.trim_end();
    let strip_copy = |s: &str| -> Option<String> {
        // `name_2`
        if let Some((head, n)) = s.rsplit_once('_') {
            if !head.is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                return Some(head.to_string());
            }
        }
        // `name (2)`
        if let Some(inner) = s.strip_suffix(')') {
            if let Some((head, n)) = inner.rsplit_once('(') {
                if !head.trim().is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                    return Some(head.trim_end().to_string());
                }
            }
        }
        None
    };
    let base = strip_copy(stem).unwrap_or_else(|| stem.to_string());
    let mut keys = Vec::with_capacity(2);
    for k in [title.map(fold_words), Some(fold_words(&base))].into_iter().flatten() {
        if !k.is_empty() && !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
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

// ---------------------------------------------------------------------------
// Search (paginated): the Media screen and the media picker
// ---------------------------------------------------------------------------

/// A sort key or a "missing metadata" criterion of [`search`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchField {
    #[default]
    Path,
    Title,
    Artist,
    Album,
    Year,
    Duration,
    /// For `missing` only (not a sort key: sorts by path).
    Genre,
}

/// Where the previous page ended, in sort order: opaque to clients
/// ([`SearchCursor::encode`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchCursor {
    num: i64,
    text: String,
    path: String,
}

impl SearchCursor {
    pub fn encode(&self) -> String {
        use base64::Engine;
        let json = serde_json::to_vec(&(self.num, &self.text, &self.path)).unwrap_or_default();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
    }

    pub fn decode(s: &str) -> Option<Self> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim()).ok()?;
        let (num, text, path): (i64, String, String) = serde_json::from_slice(&bytes).ok()?;
        Some(Self { num, text, path })
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Words, each of which must appear (case-insensitive, Unicode) in the
    /// title, artist, album or path.
    pub query: String,
    /// At least one of these genres (case-insensitive). Empty = any.
    pub genres: Vec<String>,
    /// Path prefix (a folder), case-insensitive. Empty = everything.
    pub folder: String,
    pub include_unavailable: bool,
    /// Keep media missing ALL of these.
    pub missing: Vec<SearchField>,
    pub sort: SearchField,
    pub descending: bool,
    pub limit: usize,
    pub cursor: Option<SearchCursor>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchPage {
    pub media: Vec<MediaRow>,
    /// Matches across every page.
    pub total: u64,
    /// `None` = last page.
    pub next: Option<SearchCursor>,
}

/// rel_path, title, artist, album, year, duration_ms, size_bytes,
/// available, genres (joined by U+001F).
type SearchRow = (String, Option<String>, Option<String>, Option<String>, Option<i64>, i64, i64, i64, Option<String>);

pub const SEARCH_LIMIT_DEFAULT: usize = 50;
pub const SEARCH_LIMIT_MAX: usize = 500;

fn fold(s: &str) -> String {
    s.to_lowercase()
}

fn sort_key(m: &MediaRow, by: SearchField) -> (i64, String) {
    let text = |v: &Option<String>| v.as_deref().map(fold).unwrap_or_default();
    match by {
        SearchField::Path | SearchField::Genre => (0, fold(&m.rel_path)),
        SearchField::Title => (0, text(&m.title)),
        SearchField::Artist => (0, text(&m.artist)),
        SearchField::Album => (0, text(&m.album)),
        SearchField::Year => (m.year.unwrap_or(0), String::new()),
        SearchField::Duration => (m.duration_ms, String::new()),
    }
}

/// Search the media view. Filtering and folding are done in Rust (SQLite
/// only folds ASCII: « Électro » must match « électro »), over one query
/// that fetches every row with its genres. Order: the sort key, then the
/// path — stable, so a cursor (the last key seen) resumes exactly after it.
pub async fn search(pool: &SqlitePool, q: &SearchQuery) -> Result<SearchPage, sqlx::Error> {
    let rows: Vec<SearchRow> =
        sqlx::query_as(
            "SELECT m.rel_path, m.title, m.artist, m.album, m.year, m.duration_ms, m.size_bytes, m.available,
                    GROUP_CONCAT(g.genre, char(31))
             FROM media m LEFT JOIN media_genre g ON g.rel_path = m.rel_path
             GROUP BY m.rel_path",
        )
        .fetch_all(pool)
        .await?;

    let words: Vec<String> = q.query.split_whitespace().map(fold).collect();
    let wanted: std::collections::HashSet<String> = q.genres.iter().map(|g| genre_key(g)).collect();
    let folder = {
        let f = fold(q.folder.trim().trim_matches('/'));
        if f.is_empty() { f } else { format!("{f}/") }
    };

    let mut hits: Vec<MediaRow> = rows
        .into_iter()
        .map(|(rel_path, title, artist, album, year, duration_ms, size_bytes, available, genres)| {
            let mut genres: Vec<String> =
                genres.map(|g| g.split('\u{1f}').map(str::to_string).collect()).unwrap_or_default();
            genres.sort();
            MediaRow { rel_path, title, artist, album, year, duration_ms, size_bytes, available: available != 0, genres }
        })
        .filter(|m| q.include_unavailable || m.available)
        .filter(|m| folder.is_empty() || fold(&m.rel_path).starts_with(&folder))
        .filter(|m| wanted.is_empty() || m.genres.iter().any(|g| wanted.contains(&genre_key(g))))
        .filter(|m| {
            q.missing.iter().all(|f| match f {
                SearchField::Title => m.title.as_deref().is_none_or(|s| s.trim().is_empty()),
                SearchField::Artist => m.artist.as_deref().is_none_or(|s| s.trim().is_empty()),
                SearchField::Album => m.album.as_deref().is_none_or(|s| s.trim().is_empty()),
                SearchField::Year => m.year.is_none(),
                SearchField::Genre => m.genres.is_empty(),
                SearchField::Path | SearchField::Duration => false,
            })
        })
        .filter(|m| {
            if words.is_empty() {
                return true;
            }
            let hay = [m.title.as_deref(), m.artist.as_deref(), m.album.as_deref(), Some(m.rel_path.as_str())]
                .into_iter()
                .flatten()
                .map(fold)
                .collect::<Vec<_>>()
                .join("\u{1f}");
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect();

    let key = |m: &MediaRow| {
        let (num, text) = sort_key(m, q.sort);
        (num, text, m.rel_path.clone())
    };
    hits.sort_by_cached_key(key);
    if q.descending {
        hits.reverse();
    }
    let total = hits.len() as u64;
    let start = match &q.cursor {
        None => 0,
        Some(c) => {
            let at = (c.num, c.text.clone(), c.path.clone());
            // First row strictly after the cursor, in the page order.
            hits.partition_point(|m| if q.descending { key(m) >= at } else { key(m) <= at })
        }
    };
    let limit = if q.limit == 0 { SEARCH_LIMIT_DEFAULT } else { q.limit.min(SEARCH_LIMIT_MAX) };
    let page: Vec<MediaRow> = hits.into_iter().skip(start).take(limit).collect();
    let next = if start + page.len() < total as usize {
        page.last().map(|m| {
            let (num, text, path) = key(m);
            SearchCursor { num, text, path }
        })
    } else {
        None
    };
    Ok(SearchPage { media: page, total, next })
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

/// The indexed duration of a media row in milliseconds (`None` = path
/// unknown). Used to decide whether a track that left the air played to its
/// end.
pub async fn duration_ms_of(pool: &SqlitePool, rel_path: &str) -> Result<Option<i64>, sqlx::Error> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT duration_ms FROM media WHERE rel_path = ?1")
        .bind(rel_path)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|(d,)| d))
}

/// Display metadata of an indexed media: `(title, artist, album, duration_ms)`,
/// or `None` if the path is unknown to the index.
pub async fn brief(
    pool: &SqlitePool,
    rel_path: &str,
) -> Result<Option<(Option<String>, Option<String>, Option<String>, i64)>, sqlx::Error> {
    sqlx::query_as("SELECT title, artist, album, duration_ms FROM media WHERE rel_path = ?1")
        .bind(rel_path)
        .fetch_optional(pool)
        .await
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

    #[test]
    fn song_keys_fold_titles_file_names_and_copy_suffixes() {
        assert_eq!(
            song_keys("Ballad/the_caverns_of_asperiche_liams_rescue_1.mp3", Some("the caverns of asperiche liams")),
            vec!["the caverns of asperiche liams", "the caverns of asperiche liams rescue"]
        );
        assert_eq!(
            song_keys("EpicBallad/the_caverns_of_asperiche_liams_rescue.mp3", Some("the_caverns_of_asperiche_liams_rescue")),
            vec!["the caverns of asperiche liams rescue"]
        );
        // Same song, different spelling of the file name.
        assert_eq!(song_keys("Rock/bitchy_betty.mp3", None), song_keys("ToSort/joh/Bitchy Betty.mp3", None));
        assert_eq!(song_keys("ToSort/joh/In the Low Caste_1.mp3", None), vec!["in the low caste"]);
        assert_eq!(song_keys("x/Song (2).mp3", None), vec!["song"]);
        // A number that is part of the name, not a copy suffix, stays.
        assert_eq!(song_keys("Trance/Passang 2.mp3", None), vec!["passang 2"]);
        assert_eq!(song_keys("a/_1.mp3", Some("  ")), vec!["1"]);
        assert_eq!(song_keys("a/Électro Été.mp3", None), vec!["électro été"]);
    }

    async fn searchable() -> (tempfile::TempDir, SqlitePool) {
        let (dir, pool) = fresh_db().await;
        let m = |rel: &str, title: Option<&str>, artist: Option<&str>, year: Option<u32>, genres: &[&str], ms: u64| ScannedMedia {
            rel_path: rel.into(),
            title: title.map(Into::into),
            artist: artist.map(Into::into),
            album: None,
            year,
            genres: genres.iter().map(|s| s.to_string()).collect(),
            duration_ms: ms,
            size_bytes: 1,
            mtime_ns: 0,
        };
        let lib = vec![
            m("Rock/Été indien.mp3", Some("Été indien"), Some("Joe Dassin"), Some(1975), &["Chanson"], 200_000),
            m("Rock/b.mp3", Some("Bohemian"), Some("Queen"), Some(1975), &["Rock"], 350_000),
            m("Électro/c.mp3", None, Some("Daft Punk"), None, &[], 300_000),
            m("Électro/d.mp3", Some("Da Funk"), Some("Daft Punk"), Some(1995), &["électro"], 330_000),
            m("Jingles/j1.mp3", Some("ID"), None, None, &[], 8_000),
        ];
        replace_library(&pool, &lib, 1000).await.unwrap();
        (dir, pool)
    }

    fn paths(p: &SearchPage) -> Vec<&str> {
        p.media.iter().map(|m| m.rel_path.as_str()).collect()
    }

    #[tokio::test]
    async fn search_folds_case_and_accents_on_every_word() {
        let (_d, pool) = searchable().await;
        let q = |s: &str| SearchQuery { query: s.into(), ..Default::default() };
        assert_eq!(paths(&search(&pool, &q("été DASSIN")).await.unwrap()), ["Rock/Été indien.mp3"]);
        assert_eq!(paths(&search(&pool, &q("daft")).await.unwrap()), ["Électro/c.mp3", "Électro/d.mp3"]);
        assert_eq!(search(&pool, &q("daft queen")).await.unwrap().total, 0, "every word must match");
        let g = SearchQuery { genres: vec!["ÉLECTRO".into()], ..Default::default() };
        assert_eq!(paths(&search(&pool, &g).await.unwrap()), ["Électro/d.mp3"]);
        let f = SearchQuery { folder: "électro/".into(), ..Default::default() };
        assert_eq!(search(&pool, &f).await.unwrap().total, 2);
    }

    #[tokio::test]
    async fn search_finds_missing_metadata() {
        let (_d, pool) = searchable().await;
        let q = SearchQuery { missing: vec![SearchField::Title], ..Default::default() };
        assert_eq!(paths(&search(&pool, &q).await.unwrap()), ["Électro/c.mp3"]);
        let q = SearchQuery { missing: vec![SearchField::Artist, SearchField::Genre], ..Default::default() };
        assert_eq!(paths(&search(&pool, &q).await.unwrap()), ["Jingles/j1.mp3"]);
    }

    #[tokio::test]
    async fn search_pages_follow_a_stable_order_with_a_cursor() {
        let (_d, pool) = searchable().await;
        for descending in [false, true] {
            let mut seen = Vec::new();
            let mut cursor = None;
            loop {
                let q = SearchQuery { sort: SearchField::Year, descending, limit: 2, cursor: cursor.clone(), ..Default::default() };
                let page = search(&pool, &q).await.unwrap();
                assert_eq!(page.total, 5);
                seen.extend(page.media.iter().map(|m| m.rel_path.clone()));
                match page.next {
                    Some(c) => cursor = Some(SearchCursor::decode(&c.encode()).unwrap()),
                    None => break,
                }
            }
            // Year, then path: same-year rows keep a stable order.
            let mut want =
                vec!["Jingles/j1.mp3", "Électro/c.mp3", "Rock/b.mp3", "Rock/Été indien.mp3", "Électro/d.mp3"];
            if descending {
                want.reverse();
            }
            assert_eq!(seen, want, "descending = {descending}");
        }
        assert!(SearchCursor::decode("pas un curseur").is_none());
    }

    #[tokio::test]
    async fn a_scan_reports_what_vanished_now_and_prune_forgets_it() {
        let (_dir, pool) = fresh_db().await;
        replace_library(&pool, &[sample("a.mp3", &["x"]), sample("b.mp3", &[])], 100).await.unwrap();
        let s = replace_library(&pool, &[sample("b.mp3", &[])], 200).await.unwrap();
        assert_eq!((s.present, s.unavailable, s.vanished), (1, 1, 1));
        // Nothing changed: nothing vanished now, one still known as gone.
        let s = replace_library(&pool, &[sample("b.mp3", &[])], 300).await.unwrap();
        assert_eq!((s.present, s.unavailable, s.vanished), (1, 1, 0));
        // Last seen at 100: kept by a prune of what was seen before 50…
        assert_eq!(prune_unavailable(&pool, Some(50)).await.unwrap(), 0);
        // …forgotten by a full one, genres included.
        assert_eq!(prune_unavailable(&pool, None).await.unwrap(), 1);
        let s = replace_library(&pool, &[sample("b.mp3", &[])], 400).await.unwrap();
        assert_eq!((s.present, s.unavailable, s.vanished), (1, 0, 0));
        let (g,): (i64,) = sqlx::query_as("SELECT count(*) FROM media_genre").fetch_one(&pool).await.unwrap();
        assert_eq!(g, 0);
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
