//! Selection stage: turn a grid `playlist_ref` into a concrete media file.
//!
//! Stage two of resolution — stage one (`resolver`/`GridEngine`) answers
//! *which playlist ref is active*; here we load that playlist's definition
//! (its stored TOML is the source the view keeps), read its `selection`, and
//! pick one playable file from the media index (`media`, available only).
//!
//! SCOPE (milestones 1 + 2):
//!   * modes: `static`, `dynamic`. `group`/`queue`/`remote` → explicit error.
//!   * orders:
//!       - `shuffle` — SQLite `ORDER BY random()`, stateless (a draw may repeat
//!         until anti-repetition exists; see below).
//!       - `sequential`/`newest`/`oldest` — a persisted traversal CURSOR
//!         (`playlist_cursor`, family B) hands out the file after the last one,
//!         wrapping. `static/sequential` follows the declared `files` order;
//!         `dynamic/sequential` is lexical on the full path; `newest`/`oldest`
//!         sort by `order_by` (`filename` or `mtime`).
//!       - `fifo`/`lifo` (queue) — not reached (queue mode is rejected).
//!   * NOT YET honoured: `limit`, `constraints` (no_same_artist/track_within),
//!     `unplayed_only`, `order_by = published`. The first two need the play
//!     history (family B); the last two are loud errors, not silent pretence.
//!
//! No-silent-failure: unsupported mode/order/feature, unknown field/op, bad
//! filter value, unknown ref, empty pool — all loud errors, never a
//! wrong-but-quiet track. Media paths keep their original case (the media index
//! does), unlike playlist refs which are lower-cased.

use std::collections::HashSet;

use sqlx::SqlitePool;

use crate::group_state;
use crate::playlist::{
    Filter, Match, Mode, Order, OrderBy, Playlist, PlaylistError, Selection, Strategy,
};
use crate::playlist_cursor;
use crate::store;

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("grid references unknown playlist `{0}`")]
    PlaylistNotFound(String),
    #[error(transparent)]
    Parse(#[from] PlaylistError),
    #[error("selection mode `{0:?}` is not supported yet")]
    UnsupportedMode(Mode),
    #[error("order `{0:?}` is not supported yet")]
    UnsupportedOrder(Order),
    #[error("{0} is not supported yet")]
    Unsupported(String),
    #[error("unsupported filter: field `{field}` op `{op}`")]
    UnsupportedFilter { field: String, op: String },
    #[error("bad value for filter `{field}`: {reason}")]
    BadFilterValue { field: String, reason: String },
    #[error("no available media matches the selection")]
    PoolEmpty,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

/// Resolve a grid `playlist_ref` to one concrete media `rel_path`. Loads the
/// playlist's TOML from the view, parses it, and runs its selection. The ref
/// doubles as the traversal cursor's key.
pub async fn resolve_ref(pool: &SqlitePool, playlist_ref: &str) -> Result<String, SelectionError> {
    let toml = store::playlist_toml_by_ref(pool, playlist_ref)
        .await?
        .ok_or_else(|| SelectionError::PlaylistNotFound(playlist_ref.to_string()))?;
    let playlist = Playlist::parse(&toml)?;
    resolve_media(pool, playlist_ref, &playlist).await
}

/// Pick one media file for an already-parsed playlist. `reference` keys the
/// traversal cursor / group state (ignored by stateless orders).
pub async fn resolve_media(
    pool: &SqlitePool,
    reference: &str,
    playlist: &Playlist,
) -> Result<String, SelectionError> {
    let sel = &playlist.selection;
    match sel.mode {
        Mode::Static | Mode::Dynamic => resolve_leaf(pool, reference, sel).await,
        Mode::Group => match sel.strategy {
            Some(Strategy::Sequence) => resolve_group_sequence(pool, reference, sel).await,
            Some(other) => Err(SelectionError::Unsupported(format!("group strategy {other:?}"))),
            None => Err(SelectionError::Unsupported("group without strategy".into())),
        },
        m @ (Mode::Queue | Mode::Remote) => Err(SelectionError::UnsupportedMode(m)),
    }
}

/// Resolve one file for a LEAF playlist (static or dynamic). Groups are
/// handled by `resolve_group_sequence`, which calls back into this for each
/// member — never into `resolve_media`, so there is no async recursion.
async fn resolve_leaf(
    pool: &SqlitePool,
    reference: &str,
    sel: &Selection,
) -> Result<String, SelectionError> {
    let order = effective_order(sel);
    match sel.mode {
        Mode::Dynamic => {
            let m = sel.r#match.unwrap_or(Match::All);
            match order {
                Order::Shuffle => {
                    let w = combine_where(&sel.filter, m)?;
                    resolve_dynamic_shuffle(pool, &w).await
                }
                // "Latest": always the most recent, re-aired until a newer one
                // appears (grammar §3.2, unplayed_only = false). Stateless — the
                // head of the descending pool, no cursor advance.
                Order::Newest => {
                    let ordered = ordered_dynamic_pool(pool, sel, Order::Newest).await?;
                    ordered.into_iter().next().ok_or(SelectionError::PoolEmpty)
                }
                // oldest/false loops the pool, sequential walks it: both advance
                // the persisted cursor.
                Order::Oldest | Order::Sequential => {
                    let ordered = ordered_dynamic_pool(pool, sel, order).await?;
                    cursor_pick(pool, reference, ordered).await
                }
                other => Err(SelectionError::UnsupportedOrder(other)),
            }
        }
        Mode::Static => match order {
            Order::Shuffle => resolve_static_shuffle(pool, &sel.files).await,
            Order::Sequential => {
                let ordered = ordered_static_pool(pool, &sel.files).await?;
                cursor_pick(pool, reference, ordered).await
            }
            other => Err(SelectionError::UnsupportedOrder(other)),
        },
        // resolve_leaf is only ever called for static/dynamic.
        m => Err(SelectionError::UnsupportedMode(m)),
    }
}

/// A `sequence` group: hand out one track per turn, walking members in order,
/// `take` tracks each, wrapping at the end (a new activation of the show — a
/// freshly drawn intro, the current latest episode, a fresh outro). The
/// position is persisted (`group_state`, family B) so it survives restarts.
/// Members must be leaves; a nested group is a loud error for now.
/// `on_member_unavailable` is not modelled yet — a member with an empty pool
/// aborts by propagating the error.
async fn resolve_group_sequence(
    pool: &SqlitePool,
    group_ref: &str,
    sel: &Selection,
) -> Result<String, SelectionError> {
    if sel.members.is_empty() {
        return Err(SelectionError::PoolEmpty); // validation should prevent this
    }
    let (mut idx, mut count) = group_state::get(pool, group_ref).await?;
    if idx >= sel.members.len() {
        idx = 0;
        count = 0;
    }

    let member = &sel.members[idx];
    let take = member.take.unwrap_or(1).max(1);

    let member_key = crate::playlist::normalize_ref(&member.r#ref)
        .map_err(|_| SelectionError::PlaylistNotFound(member.r#ref.clone()))?;
    let track = resolve_member(pool, &member_key).await?;

    // Advance: another track of this member, or on to the next (wrapping).
    count += 1;
    if count >= take {
        idx += 1;
        count = 0;
    }
    if idx >= sel.members.len() {
        idx = 0;
        count = 0;
    }
    group_state::set(pool, group_ref, idx, count).await?;

    Ok(track)
}

/// Resolve one track from a group member: load the member playlist from the
/// view and resolve it as a leaf. A member that is itself a group (or queue/
/// remote) is not supported yet.
async fn resolve_member(pool: &SqlitePool, member_key: &str) -> Result<String, SelectionError> {
    let toml = store::playlist_toml_by_ref(pool, member_key)
        .await?
        .ok_or_else(|| SelectionError::PlaylistNotFound(member_key.to_string()))?;
    let playlist = Playlist::parse(&toml)?;
    match playlist.selection.mode {
        Mode::Static | Mode::Dynamic => {
            resolve_leaf(pool, member_key, &playlist.selection).await
        }
        m => Err(SelectionError::Unsupported(format!(
            "group member with mode {m:?} (nested groups / queue / remote not supported yet)"
        ))),
    }
}

/// The order actually in force: the explicit `order`, else the per-mode
/// default from the grammar (dynamic → shuffle, static → sequential, queue →
/// fifo). Remote/group have no order and are rejected before this is read.
fn effective_order(sel: &Selection) -> Order {
    sel.order.unwrap_or(match sel.mode {
        Mode::Dynamic => Order::Shuffle,
        Mode::Static => Order::Sequential,
        Mode::Queue => Order::Fifo,
        Mode::Remote | Mode::Group => Order::Shuffle, // unreachable: rejected earlier
    })
}

// --- shuffle (stateless) ---------------------------------------------------

async fn resolve_dynamic_shuffle(pool: &SqlitePool, w: &Where) -> Result<String, SelectionError> {
    // `random()` is SQLite's own PRNG: shuffle without an rng crate or state.
    // Milestone limitation: no anti-repetition memory (needs family B), so a
    // draw can repeat — acceptable for a first pass.
    let sql = format!(
        "SELECT rel_path FROM media WHERE available = 1 AND ({}) ORDER BY random() LIMIT 1",
        w.sql
    );
    let mut q = sqlx::query_as::<_, (String,)>(&sql);
    for b in &w.binds {
        q = match b {
            Bind::Text(s) => q.bind(s.clone()),
            Bind::Int(i) => q.bind(*i),
        };
    }
    q.fetch_optional(pool)
        .await?
        .map(|(p,)| p)
        .ok_or(SelectionError::PoolEmpty)
}

async fn resolve_static_shuffle(
    pool: &SqlitePool,
    files: &[String],
) -> Result<String, SelectionError> {
    if files.is_empty() {
        return Err(SelectionError::PoolEmpty); // validation should prevent this
    }
    let normalised: Vec<String> = files.iter().map(|f| normalize_media_path(f)).collect();
    let placeholders = placeholders(normalised.len());
    let sql = format!(
        "SELECT rel_path FROM media WHERE available = 1 AND rel_path IN ({placeholders}) \
         ORDER BY random() LIMIT 1"
    );
    let mut q = sqlx::query_as::<_, (String,)>(&sql);
    for f in &normalised {
        q = q.bind(f.clone());
    }
    q.fetch_optional(pool)
        .await?
        .map(|(p,)| p)
        .ok_or(SelectionError::PoolEmpty)
}

// --- ordered pools + cursor ------------------------------------------------

/// Build the fully-ordered candidate list for a dynamic playlist under a
/// stateful order. `unplayed_only` and `order_by = published` are not
/// supported yet (both need machinery we don't have) → loud errors.
async fn ordered_dynamic_pool(
    pool: &SqlitePool,
    sel: &Selection,
    order: Order,
) -> Result<Vec<String>, SelectionError> {
    if sel.unplayed_only == Some(true) {
        return Err(SelectionError::Unsupported(
            "unplayed_only (needs the play history)".into(),
        ));
    }
    let m = sel.r#match.unwrap_or(Match::All);
    let w = combine_where(&sel.filter, m)?;
    let mut rows = fetch_candidates(pool, &w).await?;

    match order {
        // dynamic/sequential: full relative path, lexical ascending.
        Order::Sequential => rows.sort_by(|a, b| a.0.cmp(&b.0)),
        Order::Newest | Order::Oldest => {
            let by = sel
                .order_by
                .ok_or_else(|| SelectionError::Unsupported("newest/oldest without order_by".into()))?;
            match by {
                OrderBy::Published => {
                    return Err(SelectionError::Unsupported("order_by = published".into()))
                }
                // Ascending; `newest` reverses below. Ties broken by path.
                OrderBy::Mtime => rows.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0))),
                OrderBy::Filename => {
                    rows.sort_by(|a, b| basename(&a.0).cmp(basename(&b.0)).then(a.0.cmp(&b.0)))
                }
            }
            if order == Order::Newest {
                rows.reverse();
            }
        }
        _ => unreachable!("ordered_dynamic_pool called with a non-ordered order"),
    }
    Ok(rows.into_iter().map(|(p, _)| p).collect())
}

/// Build the ordered candidate list for a static/sequential playlist: the
/// declared `files` order, keeping only those present & available, de-duped.
async fn ordered_static_pool(
    pool: &SqlitePool,
    files: &[String],
) -> Result<Vec<String>, SelectionError> {
    let declared: Vec<String> = files.iter().map(|f| normalize_media_path(f)).collect();
    if declared.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = placeholders(declared.len());
    let sql =
        format!("SELECT rel_path FROM media WHERE available = 1 AND rel_path IN ({placeholders})");
    let mut q = sqlx::query_as::<_, (String,)>(&sql);
    for f in &declared {
        q = q.bind(f.clone());
    }
    let present: HashSet<String> = q.fetch_all(pool).await?.into_iter().map(|(p,)| p).collect();

    let mut seen = HashSet::new();
    Ok(declared
        .into_iter()
        .filter(|p| present.contains(p) && seen.insert(p.clone()))
        .collect())
}

/// Advance the playlist's cursor over an ordered pool and return the chosen
/// file: the one after the last handed out (wrapping), or the first if the
/// cursor is unset or its last file has left the pool.
async fn cursor_pick(
    pool: &SqlitePool,
    reference: &str,
    ordered: Vec<String>,
) -> Result<String, SelectionError> {
    if ordered.is_empty() {
        return Err(SelectionError::PoolEmpty);
    }
    let last = playlist_cursor::get(pool, reference).await?;
    let idx = match last.and_then(|l| ordered.iter().position(|p| *p == l)) {
        Some(i) => (i + 1) % ordered.len(),
        None => 0,
    };
    let chosen = ordered[idx].clone();
    playlist_cursor::set(pool, reference, &chosen).await?;
    Ok(chosen)
}

/// Fetch (rel_path, mtime_ns) for every available media matching the WHERE.
async fn fetch_candidates(
    pool: &SqlitePool,
    w: &Where,
) -> Result<Vec<(String, i64)>, SelectionError> {
    let sql = format!(
        "SELECT rel_path, mtime_ns FROM media WHERE available = 1 AND ({})",
        w.sql
    );
    let mut q = sqlx::query_as::<_, (String, i64)>(&sql);
    for b in &w.binds {
        q = match b {
            Bind::Text(s) => q.bind(s.clone()),
            Bind::Int(i) => q.bind(*i),
        };
    }
    Ok(q.fetch_all(pool).await?)
}

/// The file-name segment of a relative path (after the last '/').
fn basename(rel_path: &str) -> &str {
    rel_path.rsplit('/').next().unwrap_or(rel_path)
}

/// Normalise a static `files` entry to compare against `media.rel_path`:
/// backslash → '/', drop a leading '/', case preserved.
fn normalize_media_path(raw: &str) -> String {
    raw.replace('\\', "/").trim_start_matches('/').to_string()
}

fn placeholders(n: usize) -> String {
    std::iter::repeat("?").take(n).collect::<Vec<_>>().join(", ")
}

// --- dynamic filter → SQL (pure, testable) ---------------------------------

/// A bound parameter for the generated WHERE. Kept typed so the query is fully
/// parameterised (no value is ever interpolated into SQL text).
#[derive(Debug, Clone, PartialEq)]
enum Bind {
    Text(String),
    Int(i64),
}

/// A generated predicate: SQL with `?` placeholders plus the binds, in the
/// order the placeholders appear.
#[derive(Debug, Clone, PartialEq)]
struct Where {
    sql: String,
    binds: Vec<Bind>,
}

/// Combine the filters with AND (`match = all`) or OR (`match = any`). No
/// filters → the always-true predicate (whole available library).
fn combine_where(filters: &[Filter], m: Match) -> Result<Where, SelectionError> {
    if filters.is_empty() {
        return Ok(Where {
            sql: "1 = 1".to_string(),
            binds: Vec::new(),
        });
    }
    let sep = match m {
        Match::All => " AND ",
        Match::Any => " OR ",
    };
    let mut parts = Vec::with_capacity(filters.len());
    let mut binds = Vec::new();
    for f in filters {
        let w = filter_sql(f)?;
        parts.push(format!("({})", w.sql));
        binds.extend(w.binds);
    }
    Ok(Where {
        sql: parts.join(sep),
        binds,
    })
}

/// Translate one `field`/`op`/`value` filter into a parameterised predicate.
/// The `field` is matched against a closed whitelist, so the column names
/// interpolated below are constant, never user input. An unknown field/op or
/// a mistyped value is a loud error (no-silent-failure).
fn filter_sql(f: &Filter) -> Result<Where, SelectionError> {
    let unsupported = || SelectionError::UnsupportedFilter {
        field: f.field.clone(),
        op: f.op.clone(),
    };
    match f.field.as_str() {
        "path" => {
            let v = as_text(f)?;
            match f.op.as_str() {
                // Empty prefix = whole library (grammar §4): match everything
                // rather than rely on instr() semantics for an empty needle.
                "prefix" if v.is_empty() => Ok(Where {
                    sql: "1 = 1".into(),
                    binds: vec![],
                }),
                "prefix" => Ok(Where {
                    sql: "instr(rel_path, ?) = 1".into(),
                    binds: vec![Bind::Text(v)],
                }),
                "eq" => Ok(Where {
                    sql: "rel_path = ?".into(),
                    binds: vec![Bind::Text(v)],
                }),
                "ne" => Ok(Where {
                    sql: "rel_path <> ?".into(),
                    binds: vec![Bind::Text(v)],
                }),
                _ => Err(unsupported()),
            }
        }
        field @ ("title" | "artist" | "album") => {
            let v = as_text(f)?;
            let sql = match f.op.as_str() {
                "eq" => format!("{field} = ?"),
                "ne" => format!("{field} <> ?"),
                "contains" => format!("instr({field}, ?) > 0"),
                "prefix" => format!("instr({field}, ?) = 1"),
                _ => return Err(unsupported()),
            };
            Ok(Where {
                sql,
                binds: vec![Bind::Text(v)],
            })
        }
        "year" => {
            let op = num_op(&f.op).ok_or_else(unsupported)?;
            Ok(Where {
                sql: format!("year {op} ?"),
                binds: vec![Bind::Int(as_int(f)?)],
            })
        }
        "duration" => {
            // Grammar's `duration` is in seconds; the column is ms.
            let op = num_op(&f.op).ok_or_else(unsupported)?;
            Ok(Where {
                sql: format!("duration_ms {op} ?"),
                binds: vec![Bind::Int(as_int(f)? * 1000)],
            })
        }
        "genre" => match f.op.as_str() {
            "has" => Ok(Where {
                sql: "EXISTS (SELECT 1 FROM media_genre mg \
                      WHERE mg.rel_path = media.rel_path AND mg.genre = ?)"
                    .into(),
                binds: vec![Bind::Text(as_text(f)?)],
            }),
            _ => Err(unsupported()),
        },
        _ => Err(unsupported()),
    }
}

/// SQL comparator for a numeric op, or `None` if the op is not a comparator.
fn num_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "=" | "==" => "=",
        "!=" => "<>",
        "<" => "<",
        "<=" => "<=",
        ">" => ">",
        ">=" => ">=",
        _ => return None,
    })
}

fn as_text(f: &Filter) -> Result<String, SelectionError> {
    f.value
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| SelectionError::BadFilterValue {
            field: f.field.clone(),
            reason: "expected a string".into(),
        })
}

fn as_int(f: &Filter) -> Result<i64, SelectionError> {
    f.value
        .as_integer()
        .ok_or_else(|| SelectionError::BadFilterValue {
            field: f.field.clone(),
            reason: "expected an integer".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::media::ScannedMedia;
    use crate::media_index;

    // ----- pure WHERE builder (no DB) ----------------------------------

    fn filt(field: &str, op: &str, value: toml::Value) -> Filter {
        Filter {
            field: field.into(),
            op: op.into(),
            value,
        }
    }

    #[test]
    fn builds_numeric_and_text_predicates() {
        let w = combine_where(
            &[
                filt("year", ">=", toml::Value::Integer(2018)),
                filt("path", "prefix", toml::Value::String("pop/".into())),
            ],
            Match::All,
        )
        .unwrap();
        assert_eq!(w.sql, "(year >= ?) AND (instr(rel_path, ?) = 1)");
        assert_eq!(w.binds, vec![Bind::Int(2018), Bind::Text("pop/".into())]);
    }

    #[test]
    fn match_any_joins_with_or() {
        let w = combine_where(
            &[
                filt("artist", "eq", toml::Value::String("A".into())),
                filt("artist", "eq", toml::Value::String("B".into())),
            ],
            Match::Any,
        )
        .unwrap();
        assert_eq!(w.sql, "(artist = ?) OR (artist = ?)");
    }

    #[test]
    fn empty_prefix_and_no_filter_match_everything() {
        assert_eq!(combine_where(&[], Match::All).unwrap().sql, "1 = 1");
        let w = combine_where(
            &[filt("path", "prefix", toml::Value::String(String::new()))],
            Match::All,
        )
        .unwrap();
        assert_eq!(w.sql, "(1 = 1)");
        assert!(w.binds.is_empty());
    }

    #[test]
    fn unknown_field_or_op_is_a_loud_error() {
        assert!(matches!(
            filter_sql(&filt("rating", ">=", toml::Value::Integer(3))),
            Err(SelectionError::UnsupportedFilter { .. })
        ));
        assert!(matches!(
            filter_sql(&filt("year", "between", toml::Value::Integer(3))),
            Err(SelectionError::UnsupportedFilter { .. })
        ));
    }

    #[test]
    fn wrong_value_type_is_a_loud_error() {
        assert!(matches!(
            filter_sql(&filt("year", ">=", toml::Value::String("nope".into()))),
            Err(SelectionError::BadFilterValue { .. })
        ));
    }

    #[test]
    fn basename_takes_the_last_segment() {
        assert_eq!(basename("a/b/ep003.mp3"), "ep003.mp3");
        assert_eq!(basename("flat.mp3"), "flat.mp3");
    }

    // ----- integration against the media + playlists views -------------

    async fn fresh_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init(&dir.path().join("t.db")).await.unwrap();
        (dir, pool)
    }

    fn media(rel: &str, artist: &str, year: i64, genres: &[&str]) -> ScannedMedia {
        ScannedMedia {
            rel_path: rel.into(),
            title: Some("t".into()),
            artist: Some(artist.into()),
            album: None,
            year: (year != 0).then(|| year as u32),
            genres: genres.iter().map(|s| s.to_string()).collect(),
            duration_ms: 180_000,
            size_bytes: 1,
            mtime_ns: 0,
        }
    }

    /// Media with an explicit mtime, for `order_by = "mtime"` tests.
    fn media_mtime(rel: &str, mtime_ns: i64) -> ScannedMedia {
        ScannedMedia {
            rel_path: rel.into(),
            title: None,
            artist: None,
            album: None,
            year: None,
            genres: vec![],
            duration_ms: 180_000,
            size_bytes: 1,
            mtime_ns,
        }
    }

    async fn add_playlist(pool: &SqlitePool, reference: &str, toml: &str) {
        let pl = Playlist::parse(toml).expect("playlist parses");
        store::upsert(pool, reference, &pl, toml, Some(reference))
            .await
            .expect("upsert playlist");
    }

    #[tokio::test]
    async fn dynamic_shuffle_resolves_from_the_matching_pool() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("pop/a.mp3", "A", 2020, &["pop"]),
                media("pop/b.mp3", "B", 2021, &["pop"]),
                media("rock/c.mp3", "C", 1999, &["rock"]),
            ],
            1000,
        )
        .await
        .unwrap();

        let toml = r#"
            name = "Pop"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = "pop/"
            [broadcast]
            type = "general"
        "#;
        add_playlist(&pool, "rot/pop", toml).await;

        let got = resolve_ref(&pool, "rot/pop").await.unwrap();
        assert!(got.starts_with("pop/"), "must pick from the pop/ pool, got {got}");
    }

    #[tokio::test]
    async fn year_filter_narrows_the_pool() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("old.mp3", "A", 2000, &[]), media("new.mp3", "B", 2020, &[])],
            1000,
        )
        .await
        .unwrap();
        let toml = r#"
            name = "Recent"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "year"
            op = ">="
            value = 2010
            [broadcast]
            type = "general"
        "#;
        add_playlist(&pool, "rot/recent", toml).await;
        assert_eq!(resolve_ref(&pool, "rot/recent").await.unwrap(), "new.mp3");
    }

    #[tokio::test]
    async fn empty_pool_is_a_loud_error() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[media("x.mp3", "A", 2000, &[])], 1000)
            .await
            .unwrap();
        let toml = r#"
            name = "None"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "year"
            op = ">="
            value = 3000
            [broadcast]
            type = "general"
        "#;
        add_playlist(&pool, "rot/none", toml).await;
        assert!(matches!(
            resolve_ref(&pool, "rot/none").await,
            Err(SelectionError::PoolEmpty)
        ));
    }

    #[tokio::test]
    async fn static_shuffle_resolves_a_listed_file() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[media("jingles/id-01.wav", "", 0, &[])], 1000)
            .await
            .unwrap();
        let toml = r#"
            name = "Jingles"
            [selection]
            mode = "static"
            order = "shuffle"
            files = ["jingles/id-01.wav"]
            [broadcast]
            type = "interval"
            every_tracks = 4
        "#;
        add_playlist(&pool, "jingles", toml).await;
        assert_eq!(resolve_ref(&pool, "jingles").await.unwrap(), "jingles/id-01.wav");
    }

    #[tokio::test]
    async fn group_mode_is_rejected_for_now() {
        let (_d, pool) = fresh_db().await;
        let toml = r#"
            name = "Grp"
            [selection]
            mode = "group"
            strategy = "weighted"
            members = [{ ref = "a", weight = 1 }]
            [broadcast]
            type = "scheduled"
        "#;
        add_playlist(&pool, "grp", toml).await;
        assert!(matches!(
            resolve_ref(&pool, "grp").await,
            Err(SelectionError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn unknown_ref_is_a_loud_error() {
        let (_d, pool) = fresh_db().await;
        assert!(matches!(
            resolve_ref(&pool, "ghost").await,
            Err(SelectionError::PlaylistNotFound(_))
        ));
    }

    // ----- cursor: sequential / newest / oldest ------------------------

    #[tokio::test]
    async fn dynamic_sequential_advances_and_wraps() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("a.mp3", "", 0, &[]), media("b.mp3", "", 0, &[]), media("c.mp3", "", 0, &[])],
            1000,
        )
        .await
        .unwrap();
        let toml = r#"
            name = "Seq"
            [selection]
            mode = "dynamic"
            order = "sequential"
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = ""
            [broadcast]
            type = "general"
        "#;
        add_playlist(&pool, "rot/seq", toml).await;

        // Lexical: a, b, c, then wrap to a.
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "a.mp3");
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "b.mp3");
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "c.mp3");
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "a.mp3");
    }

    #[tokio::test]
    async fn static_sequential_follows_declared_order_not_lexical() {
        let (_d, pool) = fresh_db().await;
        // Present out of lexical order on purpose.
        media_index::replace_library(
            &pool,
            &[media("z.wav", "", 0, &[]), media("a.wav", "", 0, &[])],
            1000,
        )
        .await
        .unwrap();
        let toml = r#"
            name = "Ordered"
            [selection]
            mode = "static"
            order = "sequential"
            files = ["z.wav", "a.wav"]
            [broadcast]
            type = "scheduled"
        "#;
        add_playlist(&pool, "show", toml).await;

        // Declared order wins over lexical: z before a.
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "z.wav");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "a.wav");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "z.wav");
    }

    #[tokio::test]
    async fn newest_is_always_the_latest_until_a_newer_arrives() {
        let (_d, pool) = fresh_db().await;
        let three = [
            media("pod/ep001.mp3", "", 0, &[]),
            media("pod/ep002.mp3", "", 0, &[]),
            media("pod/ep003.mp3", "", 0, &[]),
        ];
        media_index::replace_library(&pool, &three, 1000).await.unwrap();
        let toml = r#"
            name = "Podcast"
            [selection]
            mode = "dynamic"
            order = "newest"
            order_by = "filename"
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = "pod/"
            [broadcast]
            type = "scheduled"
        "#;
        add_playlist(&pool, "pod/latest", toml).await;

        // "Latest": always the most recent, re-aired (no cursor advance).
        assert_eq!(resolve_ref(&pool, "pod/latest").await.unwrap(), "pod/ep003.mp3");
        assert_eq!(resolve_ref(&pool, "pod/latest").await.unwrap(), "pod/ep003.mp3");

        // A newer episode arrives → it becomes the pick.
        media_index::replace_library(
            &pool,
            &[
                media("pod/ep001.mp3", "", 0, &[]),
                media("pod/ep002.mp3", "", 0, &[]),
                media("pod/ep003.mp3", "", 0, &[]),
                media("pod/ep004.mp3", "", 0, &[]),
            ],
            2000,
        )
        .await
        .unwrap();
        assert_eq!(resolve_ref(&pool, "pod/latest").await.unwrap(), "pod/ep004.mp3");
    }

    #[tokio::test]
    async fn oldest_by_mtime_starts_with_the_earliest() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media_mtime("late.mp3", 3_000),
                media_mtime("early.mp3", 1_000),
                media_mtime("mid.mp3", 2_000),
            ],
            1000,
        )
        .await
        .unwrap();
        let toml = r#"
            name = "Chrono"
            [selection]
            mode = "dynamic"
            order = "oldest"
            order_by = "mtime"
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = ""
            [broadcast]
            type = "general"
        "#;
        add_playlist(&pool, "rot/chrono", toml).await;

        assert_eq!(resolve_ref(&pool, "rot/chrono").await.unwrap(), "early.mp3");
        assert_eq!(resolve_ref(&pool, "rot/chrono").await.unwrap(), "mid.mp3");
        assert_eq!(resolve_ref(&pool, "rot/chrono").await.unwrap(), "late.mp3");
    }

    #[tokio::test]
    async fn unplayed_only_and_published_are_rejected_for_now() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[media("pod/ep001.mp3", "", 0, &[])], 1000)
            .await
            .unwrap();

        let unplayed = r#"
            name = "U"
            [selection]
            mode = "dynamic"
            order = "oldest"
            order_by = "filename"
            unplayed_only = true
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = "pod/"
            [broadcast]
            type = "scheduled"
        "#;
        add_playlist(&pool, "u", unplayed).await;
        assert!(matches!(
            resolve_ref(&pool, "u").await,
            Err(SelectionError::Unsupported(_))
        ));

        let published = r#"
            name = "P"
            [selection]
            mode = "dynamic"
            order = "newest"
            order_by = "published"
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = "pod/"
            [broadcast]
            type = "scheduled"
        "#;
        add_playlist(&pool, "p", published).await;
        assert!(matches!(
            resolve_ref(&pool, "p").await,
            Err(SelectionError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn cursor_restarts_when_last_file_left_the_pool() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("a.mp3", "", 0, &[]), media("b.mp3", "", 0, &[])],
            1000,
        )
        .await
        .unwrap();
        let toml = r#"
            name = "Seq"
            [selection]
            mode = "dynamic"
            order = "sequential"
            [[selection.filter]]
            field = "path"
            op = "prefix"
            value = ""
            [broadcast]
            type = "general"
        "#;
        add_playlist(&pool, "rot/seq", toml).await;

        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "a.mp3");
        // a.mp3 vanishes; cursor pointed at it → restart from the new head.
        media_index::replace_library(&pool, &[media("b.mp3", "", 0, &[])], 2000)
            .await
            .unwrap();
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "b.mp3");
    }

    // ----- group sequence ---------------------------------------------

    #[tokio::test]
    async fn group_sequence_intro_content_outro_then_wraps() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("intros/i1.mp3", "", 0, &[]),
                media("pod/ep001.mp3", "", 0, &[]),
                media("pod/ep002.mp3", "", 0, &[]),
                media("outros/o1.mp3", "", 0, &[]),
            ],
            1000,
        )
        .await
        .unwrap();

        add_playlist(
            &pool,
            "intro",
            r#"
                name = "Intro"
                [selection]
                mode = "dynamic"
                order = "shuffle"
                [[selection.filter]]
                field = "path"
                op = "prefix"
                value = "intros/"
                [broadcast]
                type = "general"
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "podcast",
            r#"
                name = "Pod"
                [selection]
                mode = "dynamic"
                order = "newest"
                order_by = "filename"
                [[selection.filter]]
                field = "path"
                op = "prefix"
                value = "pod/"
                [broadcast]
                type = "scheduled"
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "outro",
            r#"
                name = "Outro"
                [selection]
                mode = "dynamic"
                order = "shuffle"
                [[selection.filter]]
                field = "path"
                op = "prefix"
                value = "outros/"
                [broadcast]
                type = "general"
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "show",
            r#"
                name = "Show"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "intro" }, { ref = "podcast" }, { ref = "outro" }]
                [broadcast]
                type = "scheduled"
            "#,
        )
        .await;

        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "intros/i1.mp3");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "pod/ep002.mp3"); // newest
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "outros/o1.mp3");
        // Wrap: a new activation of the show starts again at the intro.
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "intros/i1.mp3");
    }

    #[tokio::test]
    async fn group_sequence_honours_take() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("rock/r1.mp3", "", 0, &[]),
                media("rock/r2.mp3", "", 0, &[]),
                media("jingles/j.wav", "", 0, &[]),
            ],
            1000,
        )
        .await
        .unwrap();
        add_playlist(
            &pool,
            "rock",
            r#"
                name = "Rock"
                [selection]
                mode = "dynamic"
                order = "shuffle"
                [[selection.filter]]
                field = "path"
                op = "prefix"
                value = "rock/"
                [broadcast]
                type = "general"
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "jingle",
            r#"
                name = "J"
                [selection]
                mode = "static"
                order = "shuffle"
                files = ["jingles/j.wav"]
                [broadcast]
                type = "interval"
                every_tracks = 4
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "seqshow",
            r#"
                name = "Seq"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "rock", take = 2 }, { ref = "jingle", take = 1 }]
                [broadcast]
                type = "scheduled"
            "#,
        )
        .await;

        // Two rock tracks (take = 2), then the jingle, then rock again (wrap).
        assert!(resolve_ref(&pool, "seqshow").await.unwrap().starts_with("rock/"));
        assert!(resolve_ref(&pool, "seqshow").await.unwrap().starts_with("rock/"));
        assert_eq!(resolve_ref(&pool, "seqshow").await.unwrap(), "jingles/j.wav");
        assert!(resolve_ref(&pool, "seqshow").await.unwrap().starts_with("rock/"));
    }

    #[tokio::test]
    async fn group_weighted_and_nested_are_rejected_for_now() {
        let (_d, pool) = fresh_db().await;

        add_playlist(
            &pool,
            "w",
            r#"
                name = "W"
                [selection]
                mode = "group"
                strategy = "weighted"
                members = [{ ref = "a", weight = 1 }]
                [broadcast]
                type = "scheduled"
            "#,
        )
        .await;
        assert!(matches!(
            resolve_ref(&pool, "w").await,
            Err(SelectionError::Unsupported(_))
        ));

        // A sequence group whose member is itself a group: nested → rejected.
        add_playlist(
            &pool,
            "inner",
            r#"
                name = "Inner"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "leaf" }]
                [broadcast]
                type = "scheduled"
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "outer",
            r#"
                name = "Outer"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "inner" }]
                [broadcast]
                type = "scheduled"
            "#,
        )
        .await;
        assert!(matches!(
            resolve_ref(&pool, "outer").await,
            Err(SelectionError::Unsupported(_))
        ));
    }
}
