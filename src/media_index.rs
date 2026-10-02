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
    replace_library_with_metadata(pool, scanned, &Default::default(), scanned_at).await
}

/// Persist metadata in the SAME transaction as media and genres. Each scan is
/// authoritative: removed tags or disabled plugins cannot leave stale values.
pub async fn replace_library_with_metadata(
    pool: &SqlitePool,
    scanned: &[ScannedMedia],
    metadata: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    scanned_at: i64,
) -> Result<ReplaceStats, sqlx::Error> {
    replace_library_with_writeback(pool, scanned, metadata, &Default::default(), scanned_at).await
}

/// Move play-once guards only for files actually changed by this scan's writer.
pub async fn replace_library_with_writeback(
    pool: &SqlitePool,
    scanned: &[ScannedMedia],
    metadata: &std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    originals: &crate::scan_writeback::Originals,
    scanned_at: i64,
) -> Result<ReplaceStats, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM media_meta").execute(&mut *tx).await?;

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
        if let Some((old_size, old_mtime)) = originals.get(&m.rel_path) {
            sqlx::query("UPDATE episode_play SET size_bytes = ?1, mtime_ns = ?2 WHERE rel_path = ?3 AND size_bytes = ?4 AND mtime_ns = ?5")
                .bind(m.size_bytes as i64).bind(m.mtime_ns).bind(&m.rel_path)
                .bind(*old_size as i64).bind(*old_mtime).execute(&mut *tx).await?;
        }
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

        if let Some(values) = metadata.get(&m.rel_path) {
            for (key, value) in values {
                // UTC with fixed fractional precision sorts chronologically,
                // including equal instants originally carrying different offsets.
                let value = if key == "creation" {
                    normalize_creation(value).map_err(sqlx::Error::Protocol)?
                } else { value.clone() };
                sqlx::query("INSERT INTO media_meta (rel_path, key, value) VALUES (?1, ?2, ?3)")
                    .bind(&m.rel_path).bind(key).bind(value)
                    .execute(&mut *tx).await?;
            }
        }

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

/// Reconstitue le cache typé `media_analysis` (migration 0030) depuis les
/// descripteurs lus dans les tags — la SOURCE DE VÉRITÉ. Autoritaire comme
/// `media_meta` : chaque scan efface tout puis réinsère, donc un fichier
/// disparu ou ré-encodé ne laisse pas de ligne périmée. Reconstructible : sur
/// une VM neuve, un simple scan des tags repeuple cette table sans relancer
/// l'analyse. `analysis` ne contient que les médias porteurs d'un marqueur
/// d'analyse valide (cf. `media_analysis::Analysis::from_tags`), tous présents
/// dans `media` (la FK est satisfaite).
pub async fn replace_analysis(
    pool: &SqlitePool,
    analysis: &std::collections::BTreeMap<String, (crate::media_analysis::Analysis, String)>,
    scanned_at: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM media_analysis").execute(&mut *tx).await?;
    for (rel_path, (a, version)) in analysis {
        sqlx::query(
            "INSERT INTO media_analysis
                 (rel_path, bpm, key, scale, loudness_lufs, replaygain_db,
                  danceability, genre_top, genre_prob, mood, mood_prob,
                  analyzer_version, analyzed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(rel_path)
        .bind(a.bpm)
        .bind(&a.key)
        .bind(&a.scale)
        .bind(a.loudness_lufs)
        .bind(a.replaygain_db)
        .bind(a.danceability)
        .bind(&a.genre_top)
        .bind(a.genre_prob)
        .bind(&a.mood)
        .bind(a.mood_prob)
        .bind(version)
        .bind(scanned_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// Refresh ONE media row from a fresh read of its file (after a tag edit):
/// tags, size, mtime, genres; the row is available. The `unplayed_only`
/// guards of this file that matched its previous size / mtime follow it (a
/// tag edit is not a new episode). Returns the row.
pub async fn refresh_one(pool: &SqlitePool, m: &ScannedMedia, scanned_at: i64) -> Result<Option<MediaRow>, sqlx::Error> {
    refresh_one_with(pool, m, None, scanned_at).await
}

/// [`refresh_one`], and when `metadata` is given, the file's `media_meta`
/// rows replaced by it (after a tag edit: tempo / creation re-derived).
pub async fn refresh_one_with(
    pool: &SqlitePool,
    m: &ScannedMedia,
    metadata: Option<&std::collections::BTreeMap<String, String>>,
    scanned_at: i64,
) -> Result<Option<MediaRow>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let before: Option<(i64, i64)> = sqlx::query_as("SELECT size_bytes, mtime_ns FROM media WHERE rel_path = ?1")
        .bind(&m.rel_path)
        .fetch_optional(&mut *tx)
        .await?;
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
    sqlx::query("DELETE FROM media_genre WHERE rel_path = ?1").bind(&m.rel_path).execute(&mut *tx).await?;
    for g in &m.genres {
        sqlx::query("INSERT OR IGNORE INTO media_genre (rel_path, genre, genre_key) VALUES (?1, ?2, ?3)")
            .bind(&m.rel_path)
            .bind(g)
            .bind(genre_key(g))
            .execute(&mut *tx)
            .await?;
    }
    if let Some((size, mtime)) = before {
        sqlx::query(
            "UPDATE episode_play SET size_bytes = ?1, mtime_ns = ?2
             WHERE rel_path = ?3 AND size_bytes = ?4 AND mtime_ns = ?5",
        )
        .bind(m.size_bytes as i64)
        .bind(m.mtime_ns)
        .bind(&m.rel_path)
        .bind(size)
        .bind(mtime)
        .execute(&mut *tx)
        .await?;
    }
    if let Some(values) = metadata {
        sqlx::query("DELETE FROM media_meta WHERE rel_path = ?1").bind(&m.rel_path).execute(&mut *tx).await?;
        for (key, value) in values {
            let value = if key == "creation" { normalize_creation(value).map_err(sqlx::Error::Protocol)? } else { value.clone() };
            sqlx::query("INSERT INTO media_meta (rel_path, key, value) VALUES (?1, ?2, ?3)")
                .bind(&m.rel_path)
                .bind(key)
                .bind(value)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    row(pool, &m.rel_path).await
}

/// The `media_meta` values of one media (tempo, creation…).
pub async fn meta_of(pool: &SqlitePool, rel_path: &str) -> Result<std::collections::BTreeMap<String, String>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM media_meta WHERE rel_path = ?1 ORDER BY key")
        .bind(rel_path)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Distinct values of a `media_meta` key across the library, most used
/// first (the tempo labels already in use…).
pub async fn meta_values(pool: &SqlitePool, key: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT value FROM media_meta WHERE key = ?1 GROUP BY value ORDER BY count(*) DESC, value")
            .bind(key)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(v,)| v).collect())
}

/// Columns of a media row as read by [`row`].
type RowColumns = (String, Option<String>, Option<String>, Option<String>, Option<i64>, i64, i64, i64);

/// One media row (available or not), `None` if the index does not know it.
pub async fn row(pool: &SqlitePool, rel_path: &str) -> Result<Option<MediaRow>, sqlx::Error> {
    let r: Option<RowColumns> = sqlx::query_as(
        "SELECT rel_path, title, artist, album, year, duration_ms, size_bytes, available
         FROM media WHERE rel_path = ?1",
    )
    .bind(rel_path)
    .fetch_optional(pool)
    .await?;
    let Some((rel_path, title, artist, album, year, duration_ms, size_bytes, available)) = r else {
        return Ok(None);
    };
    let genres: Vec<String> =
        sqlx::query_as::<_, (String,)>("SELECT genre FROM media_genre WHERE rel_path = ?1 ORDER BY genre")
            .bind(&rel_path)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(g,)| g)
            .collect();
    Ok(Some(MediaRow { rel_path, title, artist, album, year, duration_ms, size_bytes, available: available != 0, genres }))
}

/// Forget the media that vanished from disk (`available = 0`): their rows
/// and genres. `seen_before` = only those last seen by a scan before this
/// epoch (s); `None` = all of them. The play history keeps its own path and
/// artist (titles of forgotten media are no longer shown). Returns how many
/// were forgotten.
pub async fn prune_unavailable(pool: &SqlitePool, seen_before: Option<i64>) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let cond = "available = 0 AND (?1 IS NULL OR scanned_at < ?1)";
    for table in ["media_genre", "media_tag"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE rel_path IN (SELECT rel_path FROM media WHERE {cond})"))
            .bind(seen_before)
            .execute(&mut *tx)
            .await?;
    }
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
    pub directory: Option<String>,
    pub include_unavailable: bool,
    /// Keep media missing ALL of these.
    pub missing: Vec<SearchField>,
    pub sort: SearchField,
    pub descending: bool,
    pub limit: usize,
    pub cursor: Option<SearchCursor>,
    /// Creation-date bounds, resolved from ages by [`age_filter`]: `(op,
    /// instant)` on the stored form. A media without a creation date passes
    /// none of them.
    pub creation: Vec<(&'static str, String)>,
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
type SearchRow = (String, Option<String>, Option<String>, Option<String>, Option<i64>, i64, i64, i64, Option<String>, Option<String>);

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
                    GROUP_CONCAT(g.genre, char(31)),
                    (SELECT d.value FROM media_meta d WHERE d.rel_path = m.rel_path AND d.key = 'creation')
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

    let created = |c: &Option<String>| {
        q.creation.iter().all(|(op, bound)| {
            c.as_deref().is_some_and(|c| match *op {
                "<" => c < bound.as_str(),
                "<=" => c <= bound.as_str(),
                ">" => c > bound.as_str(),
                ">=" => c >= bound.as_str(),
                _ => false,
            })
        })
    };
    let mut hits: Vec<MediaRow> = rows
        .into_iter()
        .filter(|row| created(&row.9))
        .map(|(rel_path, title, artist, album, year, duration_ms, size_bytes, available, genres, _creation)| {
            let mut genres: Vec<String> =
                genres.map(|g| g.split('\u{1f}').map(str::to_string).collect()).unwrap_or_default();
            genres.sort();
            MediaRow { rel_path, title, artist, album, year, duration_ms, size_bytes, available: available != 0, genres }
        })
        .filter(|m| q.include_unavailable || m.available)
        .filter(|m| folder.is_empty() || fold(&m.rel_path).starts_with(&folder))
        .filter(|m| q.directory.as_ref().is_none_or(|d| m.rel_path.rsplit_once('/').map(|(p,_)| p).unwrap_or("") == d))
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

/// Where each value of a file comes from: `(origin, value)` per `rel_path`;
/// origin `""` = the file's genre (`TCON`), otherwise the user frame it was
/// read from (a `custom-tags` source, e.g. `Type`).
pub type TagValues = std::collections::BTreeMap<String, Vec<(String, String)>>;

/// Record the origins of the values (`media_tag`). `full` = a whole-library
/// scan: rows of files not in `values` go too; otherwise only the files
/// given are replaced (a tag edit).
pub async fn replace_tag_values(pool: &SqlitePool, values: &TagValues, full: bool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    if full {
        sqlx::query("DELETE FROM media_tag").execute(&mut *tx).await?;
    }
    for (rel_path, pairs) in values {
        if !full {
            sqlx::query("DELETE FROM media_tag WHERE rel_path = ?1").bind(rel_path).execute(&mut *tx).await?;
        }
        for (origin, value) in pairs {
            if value.trim().is_empty() {
                continue;
            }
            sqlx::query("INSERT OR IGNORE INTO media_tag (rel_path, origin, value, value_key) VALUES (?1, ?2, ?3, ?4)")
                .bind(rel_path)
                .bind(origin)
                .bind(value.trim())
                .bind(genre_key(value))
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await
}

/// One value of an origin, all spellings together (case folded).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagValueCount {
    /// The most frequent spelling.
    pub value: String,
    /// Media carrying it.
    pub count: usize,
    /// Every spelling seen (more than one = inconsistent).
    pub spellings: Vec<String>,
}

/// The values of one origin (`""` = the file's genre).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OriginInventory {
    pub origin: String,
    /// Sorted by folded key.
    pub values: Vec<TagValueCount>,
    /// Available media with no value for this origin.
    pub without: usize,
}

/// Values per origin among the AVAILABLE media: `origins` first (the file's
/// genre, then the declared sources, even when empty), then any other origin
/// still recorded.
pub async fn tag_inventory(pool: &SqlitePool, origins: &[String]) -> Result<Vec<OriginInventory>, sqlx::Error> {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    let mut all: Vec<String> = origins.to_vec();
    let recorded: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT origin FROM media_tag ORDER BY origin").fetch_all(pool).await?;
    for (o,) in recorded {
        if !all.iter().any(|x| x.eq_ignore_ascii_case(&o)) {
            all.push(o);
        }
    }
    let mut out = Vec::with_capacity(all.len());
    for origin in all {
        let pairs: Vec<(String, String)> = sqlx::query_as(
            "SELECT t.value, t.rel_path FROM media_tag t JOIN media m ON m.rel_path = t.rel_path
             WHERE m.available = 1 AND t.origin = ?1 COLLATE NOCASE",
        )
        .bind(&origin)
        .fetch_all(pool)
        .await?;
        let (without,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM media m WHERE m.available = 1
             AND NOT EXISTS (SELECT 1 FROM media_tag t WHERE t.rel_path = m.rel_path AND t.origin = ?1 COLLATE NOCASE)",
        )
        .bind(&origin)
        .fetch_one(pool)
        .await?;
        let mut buckets: BTreeMap<String, (BTreeSet<String>, HashMap<String, usize>)> = BTreeMap::new();
        for (value, rel_path) in pairs {
            let e = buckets.entry(genre_key(&value)).or_default();
            e.0.insert(rel_path);
            *e.1.entry(value).or_default() += 1;
        }
        let values = buckets
            .into_values()
            .map(|(media, spellings)| {
                let value = spellings
                    .iter()
                    .max_by(|(a, na), (b, nb)| na.cmp(nb).then_with(|| b.cmp(a)))
                    .map(|(s, _)| s.clone())
                    .unwrap_or_default();
                let mut all: Vec<String> = spellings.into_keys().collect();
                all.sort();
                TagValueCount { value, count: media.len(), spellings: all }
            })
            .collect();
        out.push(OriginInventory { origin, values, without: without as usize });
    }
    Ok(out)
}

/// Available media carrying `value` (case folded) from `origin`.
pub async fn files_with_value(pool: &SqlitePool, origin: &str, value: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT t.rel_path FROM media_tag t JOIN media m ON m.rel_path = t.rel_path
         WHERE m.available = 1 AND t.origin = ?1 COLLATE NOCASE AND t.value_key = ?2 ORDER BY t.rel_path",
    )
    .bind(origin)
    .bind(genre_key(value))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(p,)| p).collect())
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
    async fn search_filters_on_the_age_of_the_creation_date() {
        let (_d, pool) = searchable().await;
        let now = 1_790_683_200; // 2026-09-29T12:00:00Z
        for (p, c) in [("Rock/b.mp3", "2026-09-27T12:00:00.000000000Z"), ("Électro/d.mp3", "2026-08-01T00:00:00.000000000Z")] {
            sqlx::query("INSERT INTO media_meta (rel_path, key, value) VALUES (?1, 'creation', ?2)")
                .bind(p)
                .bind(c)
                .execute(&pool)
                .await
                .unwrap();
        }
        let q = |ages: &[(&str, &str)]| SearchQuery {
            creation: ages.iter().map(|(op, v)| age_filter(op, v, now).unwrap()).collect(),
            ..Default::default()
        };
        let paths = |p: SearchPage| p.media.into_iter().map(|m| m.rel_path).collect::<Vec<_>>();
        assert_eq!(paths(search(&pool, &q(&[("<", "10d")])).await.unwrap()), ["Rock/b.mp3"]);
        assert_eq!(paths(search(&pool, &q(&[(">", "10d")])).await.unwrap()), ["Électro/d.mp3"]);
        assert!(search(&pool, &q(&[(">", "10d"), ("<", "30d")])).await.unwrap().media.is_empty());
        assert_eq!(age_filter("=", "10d", now), Err(AgeFilterError::Op));
        assert!(matches!(age_filter("<", "10", now), Err(AgeFilterError::Value(_))));
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

/// Strict RFC3339 shape, calendar validation by jiff, and one sortable UTC form.
/// Fixed nanosecond precision prevents prefix ordering mistakes at whole seconds.
/// The creation instant `age` (a duration: `10d`, `12h`) before `now`, in the
/// stored form of `normalize_creation` (UTC, 9 fractional digits: the
/// strings order like the instants).
fn creation_age_bound(age: &str, now: i64) -> Result<String, String> {
    let secs = crate::playlist::parse_duration_secs(age)?;
    let at = now.checked_sub(i64::try_from(secs).map_err(|_| format!("duration {age:?} is too large"))?);
    let t = at
        .and_then(|s| jiff::Timestamp::from_second(s).ok())
        .ok_or_else(|| format!("duration {age:?} is too large"))?;
    Ok(format!("{t:.9}"))
}

/// An age comparison as a comparison of creation instants: younger = later.
/// `=` / `!=` are refused (an age to the second is never what is meant).
fn age_as_creation_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        _ => return None,
    })
}

/// Why an age filter is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgeFilterError {
    Op,
    Value(String),
}

/// `age <op> <duration>` at `now` → the same test on the stored creation
/// date: `(op, instant)`. Shared by the playlist filter and the search.
pub(crate) fn age_filter(op: &str, value: &str, now: i64) -> Result<(&'static str, String), AgeFilterError> {
    let op = age_as_creation_op(op).ok_or(AgeFilterError::Op)?;
    let bound = creation_age_bound(value, now).map_err(AgeFilterError::Value)?;
    Ok((op, bound))
}

pub(crate) fn normalize_creation(value: &str) -> Result<String, String> {
    let bad = || "expected an RFC3339 timestamp with offset".to_string();
    let b = value.as_bytes();
    if b.len() < 20 || !value.is_ascii()
        || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't')
        || b[13] != b':' || b[16] != b':'
        || [0..4, 5..7, 8..10, 11..13, 14..16, 17..19].iter()
            .any(|r| !b[r.clone()].iter().all(u8::is_ascii_digit))
    { return Err(bad()); }
    let mut offset = 19;
    if b.get(offset) == Some(&b'.') {
        offset += 1;
        let start = offset;
        while b.get(offset).is_some_and(u8::is_ascii_digit) { offset += 1; }
        if start == offset { return Err(bad()); }
    }
    let zone = &b[offset..];
    if !(zone == b"Z" || zone == b"z" || (zone.len() == 6
        && matches!(zone[0], b'+' | b'-') && zone[3] == b':'
        && zone[1..3].iter().chain(zone[4..6].iter()).all(u8::is_ascii_digit)))
    { return Err(bad()); }
    let timestamp = value.parse::<jiff::Timestamp>().map_err(|_| bad())?;
    Ok(format!("{timestamp:.9}"))
}

pub use crate::media_folders::{folders, Folder, FolderPage};
