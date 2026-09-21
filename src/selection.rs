//! Selection stage: turn a grid `playlist_ref` into a concrete media file.
//!
//! Stage two of resolution — stage one (`resolver`/`GridEngine`) answers
//! *which playlist ref is active*; here we load that playlist's definition
//! (its stored TOML is the source the view keeps), read its `selection`, build
//! a candidate pool from the media index, optionally let plugins filter it,
//! then pick one file.
//!
//! Pipeline: **materialize → filter_pool → choose.** The pool is a
//! `Vec<Candidate>` (available media matching the selection); plugins may
//! remove candidates (`filter_pool`, A2); the remaining pool is then chosen
//! from by the order — `shuffle` (rand), `sequential`/`oldest` (persisted
//! cursor), `newest` (head, stateless).
//!
//! SCOPE:
//!   * modes: `static`, `dynamic`, `group`(sequence/shuffle/rotate/weighted).
//!     `queue`/`remote` → error.
//!   * NOT YET honoured: `limit`, `constraints`, `unplayed_only`,
//!     `order_by = published` (loud errors or tolerated-not-applied).
//!
//! No-silent-failure: unsupported mode/order/feature, unknown field/op, bad
//! filter value, unknown ref, empty pool — all loud errors. Media paths keep
//! their original case, unlike playlist refs (lower-cased).

use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;
use sqlx::SqlitePool;

use crate::playlist::{
    Filter, Match, Member, MemberUnavailable, Mode, Order, OrderBy, Playlist, PlaylistError,
    Selection, Strategy,
};
use crate::playlist_cursor;
use crate::plugin::{Candidate, PluginHandle};
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

/// Resolve a grid `playlist_ref` to one concrete media `rel_path`, without any
/// plugin filtering, at the current wall clock (used by tests and simple
/// callers). For a time-budget group member, prefer [`resolve_ref_at`].
pub async fn resolve_ref(pool: &SqlitePool, playlist_ref: &str) -> Result<String, SelectionError> {
    resolve_inner(pool, None, wall_now(), playlist_ref).await
}

/// Same as [`resolve_ref`] but at an explicit instant `now` (epoch seconds), so
/// a caller can drive time-budget (`runtime`) group members deterministically
/// on the controllable clock instead of the wall clock.
pub async fn resolve_ref_at(
    pool: &SqlitePool,
    now: i64,
    playlist_ref: &str,
) -> Result<String, SelectionError> {
    resolve_inner(pool, None, now, playlist_ref).await
}

/// Same, but let the plugin system filter the candidate pool, at instant `now`.
/// Used by the live engine, which holds both the `PluginHandle` and the
/// station clock — `now` is the same instant the grid resolved at, so a
/// `runtime` budget rides the exact clock as everything else.
pub async fn resolve_ref_with_plugins(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    now: i64,
    playlist_ref: &str,
) -> Result<String, SelectionError> {
    resolve_inner(pool, plugins, now, playlist_ref).await
}

async fn resolve_inner(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    now: i64,
    playlist_ref: &str,
) -> Result<String, SelectionError> {
    // Resolve against the canonical key. The store lookup is itself
    // case-insensitive on the ref (it normalizes), but we normalize here too so
    // the canonical key flows DOWNSTREAM as the cursor / group-state key —
    // family-B state must not fork on how the ref happened to be spelled
    // ("Filler" vs "filler"). Mirrors the member-ref normalization below.
    let key = crate::playlist::normalize_ref(playlist_ref)
        .map_err(|_| SelectionError::PlaylistNotFound(playlist_ref.to_string()))?;
    let toml = store::playlist_toml_by_ref(pool, &key)
        .await?
        .ok_or_else(|| SelectionError::PlaylistNotFound(playlist_ref.to_string()))?;
    let playlist = Playlist::parse(&toml)?;
    resolve_media(pool, plugins, now, &key, &playlist).await
}

/// Wall-clock now in epoch seconds, for callers that don't supply an instant.
fn wall_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn resolve_media(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    now: i64,
    reference: &str,
    playlist: &Playlist,
) -> Result<String, SelectionError> {
    let sel = &playlist.selection;
    match sel.mode {
        Mode::Static | Mode::Dynamic => resolve_leaf(pool, plugins, reference, sel).await,
        Mode::Group => match sel.strategy {
            Some(Strategy::Sequence) => {
                resolve_group_rotation(pool, plugins, now, reference, sel, false).await
            }
            Some(Strategy::Shuffle) => {
                resolve_group_rotation(pool, plugins, now, reference, sel, true).await
            }
            // Rotate = plain round-robin, one track per member per turn. Its
            // members are bare (validation forbids take/runtime/weight), so the
            // sequence walk with the default take = 1 already IS a rotation;
            // position persists across turns via group_state.
            Some(Strategy::Rotate) => {
                resolve_group_rotation(pool, plugins, now, reference, sel, false).await
            }
            Some(Strategy::Weighted) => resolve_group_weighted(pool, plugins, sel).await,
            None => Err(SelectionError::Unsupported("group without strategy".into())),
        },
        m @ (Mode::Queue | Mode::Remote) => Err(SelectionError::UnsupportedMode(m)),
    }
}

/// Resolve one file for a LEAF playlist (static or dynamic): materialize the
/// pool, run plugin filtering, then choose. Groups call back here per member —
/// never into `resolve_media` — so there is no async recursion.
async fn resolve_leaf(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    reference: &str,
    sel: &Selection,
) -> Result<String, SelectionError> {
    let order = effective_order(sel);

    // 1. Materialize the base pool (base order: static = declared, dynamic =
    //    lexical on rel_path).
    let mut candidates = match sel.mode {
        Mode::Dynamic => materialize_dynamic(pool, sel).await?,
        Mode::Static => materialize_static(pool, &sel.files).await?,
        m => return Err(SelectionError::UnsupportedMode(m)),
    };

    // 2. Plugin filtering (if wired). A plugin may remove candidates — even
    //    all of them. We do NOT fail-open here (choice (b)): an emptied pool
    //    propagates as PoolEmpty so the caller gets a direct signal and the
    //    grid fallback takes over. But emptying a non-empty pool is worth a
    //    loud warning — a filter causing a potential gap must be visible.
    if let Some(handle) = plugins {
        let before = candidates.len();
        candidates = handle.filter_pool(candidates).await;
        if before > 0 && candidates.is_empty() {
            tracing::warn!(
                playlist = %reference,
                before,
                "plugin filter_pool emptied a non-empty pool; grid fallback should cover"
            );
        }
    }
    if candidates.is_empty() {
        return Err(SelectionError::PoolEmpty);
    }

    // 3. Choose from the filtered pool.
    match order {
        Order::Shuffle => Ok(candidates
            .choose(&mut rand::thread_rng())
            .expect("non-empty pool")
            .rel_path
            .clone()),
        Order::Sequential => cursor_pick(pool, reference, &candidates).await,
        Order::Newest => {
            reject_unplayed_only(sel)?;
            let key = order_key(sel)?;
            candidates.sort_by(|a, b| cmp_by(a, b, key));
            // Ascending sort → the newest is the last element.
            Ok(candidates.last().expect("non-empty pool").rel_path.clone())
        }
        Order::Oldest => {
            reject_unplayed_only(sel)?;
            let key = order_key(sel)?;
            candidates.sort_by(|a, b| cmp_by(a, b, key));
            cursor_pick(pool, reference, &candidates).await
        }
        other => Err(SelectionError::UnsupportedOrder(other)),
    }
}

/// A rotation group — `sequence` or `shuffle` — hands out one track per turn,
/// walking its members and honouring each member's per-member quota, then
/// wrapping (a new activation). `sequence` walks the declared order; `shuffle`
/// walks a random permutation, re-drawn each cycle and persisted so a restart
/// mid-cycle does not repeat a member. State in `group_state` (family B).
/// Members must be leaves; nested groups are a loud error for now.
///
/// Per-member quota (mutually exclusive, validated upstream):
/// - **`take`** (tracks): emit, count, advance once the count reaches `take`.
/// - **`runtime`** (time budget): stamp the member's start on its first track,
///   keep emitting while `now - started < budget`, and advance at the next
///   track boundary once the budget has elapsed. Soft — the member's last track
///   may overrun. `now` is epoch seconds on the controllable station clock, so
///   the budget is testable and `--at`-drivable, and a downtime longer than the
///   budget simply expires the member at restart (catch-up).
async fn resolve_group_rotation(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    now: i64,
    group_ref: &str,
    sel: &Selection,
    shuffle: bool,
) -> Result<String, SelectionError> {
    let n = sel.members.len();
    if n == 0 {
        return Err(SelectionError::PoolEmpty);
    }
    let policy = sel
        .on_member_unavailable
        .unwrap_or(MemberUnavailable::Abort);

    let mut st = crate::group_state::get(pool, group_ref).await?;

    // Traversal order for this cycle. `sequence` = declared order; `shuffle` =
    // the persisted permutation, re-drawn when absent or stale (e.g. the member
    // count changed after a TOML edit) — a fresh cycle from the top.
    let mut order: Vec<usize> = if shuffle {
        match &st.permutation {
            Some(p) if p.len() == n => p.clone(),
            _ => {
                st.member_idx = 0;
                st.take_count = 0;
                st.member_started_at = None;
                new_permutation(n)
            }
        }
    } else {
        (0..n).collect()
    };

    // Try members from the current position. With `skip`, an unavailable member
    // (empty pool) is dropped and we advance to the next this turn; with
    // `abort` (default) it fails the whole group → the grid falls through to a
    // lower-priority source. Bounded to one full pass so we never loop forever.
    for _ in 0..n {
        // Wrap: past the last member → a fresh cycle (shuffle re-draws).
        if st.member_idx >= n {
            st.member_idx = 0;
            st.take_count = 0;
            st.member_started_at = None;
            if shuffle {
                order = new_permutation(n);
            }
        }

        let member = &sel.members[order[st.member_idx]];
        let member_key = crate::playlist::normalize_ref(&member.r#ref)
            .map_err(|_| SelectionError::PlaylistNotFound(member.r#ref.clone()))?;

        // Time-budget expiry is checked BEFORE emitting: if the current
        // member's budget has already elapsed, advance now and let the next
        // member produce this turn (soft switch at a track boundary). Only the
        // current member can be expired — the next has no start stamp yet — so
        // this fires at most once per call: no runaway skipping.
        if let Some(budget) = member_runtime_secs(member)? {
            if let Some(started) = st.member_started_at {
                if now.saturating_sub(started) >= budget {
                    st.member_idx += 1;
                    st.take_count = 0;
                    st.member_started_at = None;
                    continue;
                }
            }
        }

        match resolve_member(pool, plugins, &member_key).await {
            Ok(track) => {
                if member.runtime.is_some() {
                    // Time budget: stamp the slot start on the first track and
                    // stay — the expiry check above is what advances later.
                    if st.member_started_at.is_none() {
                        st.member_started_at = Some(now);
                    }
                } else {
                    // Track budget: count this track, advance once `take` met.
                    let take = member.take.unwrap_or(1).max(1);
                    st.take_count += 1;
                    if st.take_count >= take {
                        st.member_idx += 1;
                        st.take_count = 0;
                        st.member_started_at = None;
                    }
                }
                // Fold a wrap so the NEXT call starts a fresh cycle cleanly.
                if st.member_idx >= n {
                    st.member_idx = 0;
                    st.take_count = 0;
                    st.member_started_at = None;
                    if shuffle {
                        order = new_permutation(n);
                    }
                }
                st.permutation = shuffle.then(|| order.clone());
                crate::group_state::set(pool, group_ref, &st).await?;
                return Ok(track);
            }
            // `skip`: drop this member and try the next one this turn.
            Err(SelectionError::PoolEmpty) if policy == MemberUnavailable::Skip => {
                st.member_idx += 1;
                st.take_count = 0;
                st.member_started_at = None;
                continue;
            }
            // `abort` (PoolEmpty) or a config error: propagate. An aborted group
            // bubbles PoolEmpty to the grid, which falls through to the floor.
            Err(e) => return Err(e),
        }
    }

    // `skip` exhausted every member → the group produces nothing.
    Err(SelectionError::PoolEmpty)
}

/// A group weight given to a member that declares none. Neutral middle of the
/// documented 0–50 range (see the playlist grammar proposal): with weights all
/// omitted every member is equally likely; it only matters when some members
/// set a weight and others don't. `0` is an explicit exclusion from the draw.
const DEFAULT_WEIGHT: u32 = 15;

/// A `weighted` group: draw one available member at random, weighted by its
/// `weight`, and emit one track from it. Independent draws — nothing is
/// persisted (no position, unlike `rotate`/`sequence`). A member with no
/// explicit `weight` gets [`DEFAULT_WEIGHT`]; `weight = 0` excludes it from the
/// draw (not a global disable). If the drawn member yields nothing, the group's
/// `on_member_unavailable` decides: `skip` re-draws among the rest, `abort`
/// (default) bubbles `PoolEmpty` so the grid falls through to a lower-priority
/// source. Members must be leaves; a nested group is a loud error (via
/// `resolve_member`).
async fn resolve_group_weighted(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    sel: &Selection,
) -> Result<String, SelectionError> {
    let policy = sel
        .on_member_unavailable
        .unwrap_or(MemberUnavailable::Abort);

    // Eligible members = weight > 0, paired with their member index so a
    // skipped (empty) member can be removed and the draw retried.
    let mut eligible: Vec<(usize, u32)> = sel
        .members
        .iter()
        .enumerate()
        .map(|(i, m)| (i, m.weight.unwrap_or(DEFAULT_WEIGHT)))
        .filter(|(_, w)| *w > 0)
        .collect();

    // Bounded: each failed draw under `skip` removes one member, so at most
    // `members.len()` iterations before the pool is empty.
    while !eligible.is_empty() {
        let &(member_i, _) = eligible
            .choose_weighted(&mut rand::thread_rng(), |&(_, w)| w)
            .map_err(|_| SelectionError::PoolEmpty)?;
        let member = &sel.members[member_i];
        let member_key = crate::playlist::normalize_ref(&member.r#ref)
            .map_err(|_| SelectionError::PlaylistNotFound(member.r#ref.clone()))?;
        match resolve_member(pool, plugins, &member_key).await {
            Ok(track) => return Ok(track),
            // `skip`: drop this empty member and re-draw among the rest.
            Err(SelectionError::PoolEmpty) if policy == MemberUnavailable::Skip => {
                eligible.retain(|&(i, _)| i != member_i);
                continue;
            }
            // `abort` (default) or a config error: propagate.
            Err(e) => return Err(e),
        }
    }
    Err(SelectionError::PoolEmpty)
}

/// A fresh random permutation of member indices `0..n` (for `shuffle`).
fn new_permutation(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    order.shuffle(&mut rand::thread_rng());
    order
}

/// A member's time budget in seconds, if it carries a `runtime` quota. The
/// stored duration is re-parsed here; a malformed value is a loud error
/// (validation should have caught it on the way in, but selection never trusts
/// silently).
fn member_runtime_secs(member: &Member) -> Result<Option<i64>, SelectionError> {
    match &member.runtime {
        None => Ok(None),
        Some(r) => crate::playlist::parse_duration_secs(r)
            .map(|s| Some(s as i64))
            .map_err(|e| {
                SelectionError::Unsupported(format!("member `{}` runtime `{r}`: {e}", member.r#ref))
            }),
    }
}

async fn resolve_member(
    pool: &SqlitePool,
    plugins: Option<&PluginHandle>,
    member_key: &str,
) -> Result<String, SelectionError> {
    let toml = store::playlist_toml_by_ref(pool, member_key)
        .await?
        .ok_or_else(|| SelectionError::PlaylistNotFound(member_key.to_string()))?;
    let playlist = Playlist::parse(&toml)?;
    match playlist.selection.mode {
        Mode::Static | Mode::Dynamic => {
            resolve_leaf(pool, plugins, member_key, &playlist.selection).await
        }
        m => Err(SelectionError::Unsupported(format!(
            "group member with mode {m:?} (nested groups / queue / remote not supported yet)"
        ))),
    }
}

// --- ordering helpers ------------------------------------------------------

/// The order actually in force: explicit `order`, else the per-mode default
/// (dynamic → shuffle, static → sequential, queue → fifo).
fn effective_order(sel: &Selection) -> Order {
    sel.order.unwrap_or(match sel.mode {
        Mode::Dynamic => Order::Shuffle,
        Mode::Static => Order::Sequential,
        Mode::Queue => Order::Fifo,
        Mode::Remote | Mode::Group => Order::Shuffle, // unreachable: rejected earlier
    })
}

#[derive(Clone, Copy)]
enum SortKey {
    Filename,
    Mtime,
}

/// Sort key for `newest`/`oldest`. `published` and a missing `order_by` are
/// loud errors (not yet / required).
fn order_key(sel: &Selection) -> Result<SortKey, SelectionError> {
    match sel.order_by {
        Some(OrderBy::Filename) => Ok(SortKey::Filename),
        Some(OrderBy::Mtime) => Ok(SortKey::Mtime),
        Some(OrderBy::Published) => Err(SelectionError::Unsupported("order_by = published".into())),
        None => Err(SelectionError::Unsupported("newest/oldest without order_by".into())),
    }
}

/// Ascending comparison by the key, ties broken by rel_path.
fn cmp_by(a: &Candidate, b: &Candidate, key: SortKey) -> std::cmp::Ordering {
    match key {
        SortKey::Filename => basename(&a.rel_path)
            .cmp(basename(&b.rel_path))
            .then_with(|| a.rel_path.cmp(&b.rel_path)),
        SortKey::Mtime => a
            .mtime_ns
            .cmp(&b.mtime_ns)
            .then_with(|| a.rel_path.cmp(&b.rel_path)),
    }
}

fn reject_unplayed_only(sel: &Selection) -> Result<(), SelectionError> {
    if sel.unplayed_only == Some(true) {
        return Err(SelectionError::Unsupported(
            "unplayed_only (needs the play history)".into(),
        ));
    }
    Ok(())
}

/// Advance the playlist cursor over an ordered pool: the file after the last
/// handed out (wrapping), or the first if the cursor is unset or its last file
/// has left the pool.
async fn cursor_pick(
    pool: &SqlitePool,
    reference: &str,
    ordered: &[Candidate],
) -> Result<String, SelectionError> {
    if ordered.is_empty() {
        return Err(SelectionError::PoolEmpty);
    }
    let last = playlist_cursor::get(pool, reference).await?;
    let idx = match last.and_then(|l| ordered.iter().position(|c| c.rel_path == l)) {
        Some(i) => (i + 1) % ordered.len(),
        None => 0,
    };
    let chosen = ordered[idx].rel_path.clone();
    playlist_cursor::set(pool, reference, &chosen).await?;
    Ok(chosen)
}

// --- materialization -------------------------------------------------------

/// Row shape read from `media` for a candidate (genres fetched separately).
type CandRow = (
    String,         // rel_path
    Option<String>, // artist
    Option<String>, // title
    Option<String>, // album
    Option<i64>,    // year
    i64,            // duration_ms
    i64,            // mtime_ns
);

pub(crate) async fn materialize_dynamic(
    pool: &SqlitePool,
    sel: &Selection,
) -> Result<Vec<Candidate>, SelectionError> {
    let m = sel.r#match.unwrap_or(Match::All);
    let w = combine_where(&sel.filter, m)?;
    let sql = format!(
        "SELECT rel_path, artist, title, album, year, duration_ms, mtime_ns \
         FROM media WHERE available = 1 AND ({}) ORDER BY rel_path",
        w.sql
    );
    let mut q = sqlx::query_as::<_, CandRow>(&sql);
    for b in &w.binds {
        q = match b {
            Bind::Text(s) => q.bind(s.clone()),
            Bind::Int(i) => q.bind(*i),
        };
    }
    let rows = q.fetch_all(pool).await?;
    with_genres(pool, rows).await
}

pub(crate) async fn materialize_static(
    pool: &SqlitePool,
    files: &[String],
) -> Result<Vec<Candidate>, SelectionError> {
    let declared: Vec<String> = files.iter().map(|f| normalize_media_path(f)).collect();
    if declared.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT rel_path, artist, title, album, year, duration_ms, mtime_ns \
         FROM media WHERE available = 1 AND rel_path IN ({})",
        placeholders(declared.len())
    );
    let mut q = sqlx::query_as::<_, CandRow>(&sql);
    for p in &declared {
        q = q.bind(p.clone());
    }
    let rows = q.fetch_all(pool).await?;
    let cands = with_genres(pool, rows).await?;

    // Reorder to the declared order, present-only, de-duped.
    let by_path: HashMap<String, Candidate> =
        cands.into_iter().map(|c| (c.rel_path.clone(), c)).collect();
    let mut seen = HashSet::new();
    Ok(declared
        .into_iter()
        .filter_map(|p| {
            if seen.insert(p.clone()) {
                by_path.get(&p).cloned()
            } else {
                None
            }
        })
        .collect())
}

/// Attach the genre set to each row (one extra query for the whole pool).
async fn with_genres(
    pool: &SqlitePool,
    rows: Vec<CandRow>,
) -> Result<Vec<Candidate>, SelectionError> {
    let paths: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
    let genres = fetch_genres(pool, &paths).await?;
    Ok(rows
        .into_iter()
        .map(|(rel_path, artist, title, album, year, duration_ms, mtime_ns)| {
            let g = genres.get(&rel_path).cloned().unwrap_or_default();
            Candidate {
                rel_path,
                artist,
                title,
                album,
                year: year.map(|y| y as u32),
                duration_ms: duration_ms as u64,
                genres: g,
                mtime_ns,
            }
        })
        .collect())
}

async fn fetch_genres(
    pool: &SqlitePool,
    paths: &[String],
) -> Result<HashMap<String, Vec<String>>, sqlx::Error> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    if paths.is_empty() {
        return Ok(map);
    }
    let sql = format!(
        "SELECT rel_path, genre FROM media_genre WHERE rel_path IN ({}) ORDER BY genre",
        placeholders(paths.len())
    );
    let mut q = sqlx::query_as::<_, (String, String)>(&sql);
    for p in paths {
        q = q.bind(p.clone());
    }
    for (rel, genre) in q.fetch_all(pool).await? {
        map.entry(rel).or_default().push(genre);
    }
    Ok(map)
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
    for (index, f) in filters.iter().enumerate() {
        let w = filter_sql(f).map_err(|error| match error {
            SelectionError::BadFilterValue { field, reason } => SelectionError::BadFilterValue {
                field,
                reason: format!(
                    "selection.filter[{}]: op = {:?}, value = {:?}: {reason}",
                    index + 1, f.op, f.value,
                ),
            },
            other => other,
        })?;
        parts.push(format!("({})", w.sql));
        binds.extend(w.binds);
    }
    Ok(Where {
        sql: parts.join(sep),
        binds,
    })
}

/// Translate one `field`/`op`/`value` filter into a parameterised predicate.
/// The `field` is a closed whitelist, so the column names interpolated below
/// are constant, never user input. Unknown field/op or a mistyped value is a
/// loud error.
fn filter_sql(f: &Filter) -> Result<Where, SelectionError> {
    let unsupported = || SelectionError::UnsupportedFilter {
        field: f.field.clone(),
        op: f.op.clone(),
    };
    match f.field.as_str() {
        "path" => {
            let v = as_text(f)?;
            match f.op.as_str() {
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
            // Validate the op before the value type, so an op that doesn't apply
            // to a scalar field (e.g. `has_any`) is a clear UnsupportedFilter
            // rather than a misleading "expected a string".
            let sql = match f.op.as_str() {
                "eq" => format!("{field} = ?"),
                "ne" => format!("{field} <> ?"),
                "contains" => format!("instr({field}, ?) > 0"),
                "prefix" => format!("instr({field}, ?) = 1"),
                _ => return Err(unsupported()),
            };
            Ok(Where {
                sql,
                binds: vec![Bind::Text(as_text(f)?)],
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
            let op = num_op(&f.op).ok_or_else(unsupported)?;
            Ok(Where {
                sql: format!("duration_ms {op} ?"),
                binds: vec![Bind::Int(as_int(f)? * 1000)],
            })
        }
        // Set-valued fields (genre today, others slot into `set_field`): the
        // `has` / `has_any` operators live in `set_field_sql`, field-agnostic —
        // never hardcoded to genre.
        other => match set_field(other) {
            Some(sf) => set_field_sql(&sf, f),
            None => Err(unsupported()),
        },
    }
}

/// A set-valued (multi-valued) media attribute, stored in a detail table keyed
/// on `rel_path`. The `has`/`has_any`/`has_all`/`has_none` operators run against this mapping, so
/// they are field-agnostic: `genre` is the first entry, another set field is
/// one line here and inherits the operators unchanged. `table`/`column` are a
/// closed whitelist — constant SQL identifiers, never user input.
struct SetField {
    table: &'static str,
    column: &'static str,
}

fn set_field(field: &str) -> Option<SetField> {
    match field {
        "genre" => Some(SetField {
            table: "media_genre",
            column: "genre",
        }),
        _ => None,
    }
}

/// The `has` family for a set-valued field, compiled against its detail table:
/// - `has <s>`          — the set contains `s` (one string) → `EXISTS (… = ?)`
/// - `has_any [a, b…]`  — the set intersects the list → `EXISTS (… IN (…))`
/// - `has_all [a, b…]`  — the set contains every listed value → `AND` of one
///                        `EXISTS (… = ?)` per value
/// - `has_none [a, b…]` — the set has none of them → `NOT EXISTS (… IN (…))`
///
/// Fully parameterised: table/column are whitelist constants, the values are
/// binds. `has_none` is true for a media with no rows at all (it has none of
/// the listed values). Any other op on a set field is a loud `UnsupportedFilter`.
fn set_field_sql(sf: &SetField, f: &Filter) -> Result<Where, SelectionError> {
    let unsupported = || SelectionError::UnsupportedFilter {
        field: f.field.clone(),
        op: f.op.clone(),
    };
    let (table, column) = (sf.table, sf.column);
    // One `EXISTS (… d.<col> = ?)` fragment (one bind).
    let exists_eq = || {
        format!("EXISTS (SELECT 1 FROM {table} d WHERE d.rel_path = media.rel_path AND d.{column} = ?)")
    };
    // `[NOT] EXISTS (… d.<col> IN (?, …))` over a list (N binds).
    let exists_in = |neg: bool, n: usize| {
        format!(
            "{}EXISTS (SELECT 1 FROM {table} d WHERE d.rel_path = media.rel_path AND d.{column} IN ({}))",
            if neg { "NOT " } else { "" },
            placeholders(n)
        )
    };
    match f.op.as_str() {
        "has" => Ok(Where {
            sql: exists_eq(),
            binds: vec![Bind::Text(as_text(f)?)],
        }),
        "has_any" => {
            let values = as_text_list(f)?;
            let sql = exists_in(false, values.len());
            Ok(Where {
                sql,
                binds: values.into_iter().map(Bind::Text).collect(),
            })
        }
        "has_all" => {
            let values = as_text_list(f)?;
            // AND of one EXISTS per listed value: the set contains every one.
            let sql = vec![exists_eq(); values.len()].join(" AND ");
            Ok(Where {
                sql,
                binds: values.into_iter().map(Bind::Text).collect(),
            })
        }
        "has_none" => {
            let values = as_text_list(f)?;
            let sql = exists_in(true, values.len());
            Ok(Where {
                sql,
                binds: values.into_iter().map(Bind::Text).collect(),
            })
        }
        _ => Err(unsupported()),
    }
}

/// Validate one dynamic filter's field/op/value *shape* against the closed
/// catalogue, without a database — the very check `filter_sql` performs, with
/// the SQL discarded. Exposed so `Playlist::validate` can reject a malformed
/// filter at apply/validate time instead of it only surfacing on air. Single
/// source of truth: defers to `filter_sql`, never a second catalogue.
pub(crate) fn validate_filter(f: &Filter) -> Result<(), SelectionError> {
    filter_sql(f).map(|_| ())
}

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

/// Read a filter value as a non-empty list of non-empty strings (for `has_any`
/// and other list ops). Loud errors, per no-silent-failure: not an array, an
/// empty array, a non-string element, or an empty string — a mistyped list must
/// never quietly match nothing. Duplicates are left as-is (harmless in an
/// `IN (...)`), not rejected.
fn as_text_list(f: &Filter) -> Result<Vec<String>, SelectionError> {
    let bad = |reason: &str| SelectionError::BadFilterValue {
        field: f.field.clone(),
        reason: reason.to_string(),
    };
    let arr = f
        .value
        .as_array()
        .ok_or_else(|| bad("expected an array of strings"))?;
    if arr.is_empty() {
        return Err(bad("expected a non-empty array"));
    }
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let s = v
            .as_str()
            .ok_or_else(|| bad("every array element must be a string"))?;
        if s.is_empty() {
            return Err(bad("array elements must be non-empty strings"));
        }
        out.push(s.to_string());
    }
    Ok(out)
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
    fn genre_has_builds_an_exists() {
        let w = filter_sql(&filt("genre", "has", toml::Value::String("jazz".into()))).unwrap();
        assert_eq!(
            w.sql,
            "EXISTS (SELECT 1 FROM media_genre d WHERE d.rel_path = media.rel_path AND d.genre = ?)"
        );
        assert_eq!(w.binds, vec![Bind::Text("jazz".into())]);
    }

    #[test]
    fn genre_has_any_builds_an_in_exists() {
        let w = filter_sql(&filt(
            "genre",
            "has_any",
            toml::Value::Array(vec![
                toml::Value::String("jazz".into()),
                toml::Value::String("blues".into()),
            ]),
        ))
        .unwrap();
        assert_eq!(
            w.sql,
            "EXISTS (SELECT 1 FROM media_genre d WHERE d.rel_path = media.rel_path AND d.genre IN (?, ?))"
        );
        assert_eq!(
            w.binds,
            vec![Bind::Text("jazz".into()), Bind::Text("blues".into())]
        );
    }

    #[test]
    fn has_any_on_a_scalar_field_is_unsupported() {
        // `has`/`has_any` are for set-valued fields; on a scalar field they are
        // a loud error, never a silent no-match.
        assert!(matches!(
            filter_sql(&filt(
                "artist",
                "has_any",
                toml::Value::Array(vec![toml::Value::String("A".into())])
            )),
            Err(SelectionError::UnsupportedFilter { .. })
        ));
    }

    #[test]
    fn has_any_rejects_non_list_empty_or_non_string() {
        // not an array
        assert!(matches!(
            filter_sql(&filt("genre", "has_any", toml::Value::String("jazz".into()))),
            Err(SelectionError::BadFilterValue { .. })
        ));
        // empty array
        assert!(matches!(
            filter_sql(&filt("genre", "has_any", toml::Value::Array(vec![]))),
            Err(SelectionError::BadFilterValue { .. })
        ));
        // non-string element
        assert!(matches!(
            filter_sql(&filt(
                "genre",
                "has_any",
                toml::Value::Array(vec![toml::Value::Integer(3)])
            )),
            Err(SelectionError::BadFilterValue { .. })
        ));
    }

    #[test]
    fn genre_has_all_builds_an_and_of_exists() {
        let w = filter_sql(&filt(
            "genre",
            "has_all",
            toml::Value::Array(vec![
                toml::Value::String("jazz".into()),
                toml::Value::String("funk".into()),
            ]),
        ))
        .unwrap();
        assert_eq!(
            w.sql,
            "EXISTS (SELECT 1 FROM media_genre d WHERE d.rel_path = media.rel_path AND d.genre = ?) \
             AND EXISTS (SELECT 1 FROM media_genre d WHERE d.rel_path = media.rel_path AND d.genre = ?)"
        );
        assert_eq!(
            w.binds,
            vec![Bind::Text("jazz".into()), Bind::Text("funk".into())]
        );
    }

    #[test]
    fn genre_has_none_builds_a_not_exists_in() {
        let w = filter_sql(&filt(
            "genre",
            "has_none",
            toml::Value::Array(vec![
                toml::Value::String("rock".into()),
                toml::Value::String("metal".into()),
            ]),
        ))
        .unwrap();
        assert_eq!(
            w.sql,
            "NOT EXISTS (SELECT 1 FROM media_genre d WHERE d.rel_path = media.rel_path AND d.genre IN (?, ?))"
        );
        assert_eq!(
            w.binds,
            vec![Bind::Text("rock".into()), Bind::Text("metal".into())]
        );
    }

    #[test]
    fn has_all_and_has_none_reject_bad_values() {
        // Same list-value contract as has_any: non-array / empty → loud error.
        for op in ["has_all", "has_none"] {
            assert!(matches!(
                filter_sql(&filt("genre", op, toml::Value::String("jazz".into()))),
                Err(SelectionError::BadFilterValue { .. })
            ));
            assert!(matches!(
                filter_sql(&filt("genre", op, toml::Value::Array(vec![]))),
                Err(SelectionError::BadFilterValue { .. })
            ));
        }
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
        "#;
        add_playlist(&pool, "rot/none", toml).await;
        assert!(matches!(
            resolve_ref(&pool, "rot/none").await,
            Err(SelectionError::PoolEmpty)
        ));
    }

    #[tokio::test]
    async fn genre_filter_uses_the_detail_table() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("a.mp3", "A", 2000, &["jazz"]), media("b.mp3", "B", 2000, &["rock"])],
            1000,
        )
        .await
        .unwrap();
        let toml = r#"
            name = "Jazz"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "genre"
            op = "has"
            value = "jazz"
        "#;
        add_playlist(&pool, "rot/jazz", toml).await;
        assert_eq!(resolve_ref(&pool, "rot/jazz").await.unwrap(), "a.mp3");
    }

    #[tokio::test]
    async fn genre_has_any_matches_any_of_the_listed_genres() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("a.mp3", "A", 2000, &["jazz", "funk"]),
                media("b.mp3", "B", 2000, &["rock"]),
                media("c.mp3", "C", 2000, &["blues"]),
            ],
            1000,
        )
        .await
        .unwrap();
        // has_any [rock, blues] → b or c, never a (jazz/funk).
        let toml = r#"
            name = "RockOrBlues"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "genre"
            op = "has_any"
            value = ["rock", "blues"]
        "#;
        add_playlist(&pool, "rot/rb", toml).await;
        let got = resolve_ref(&pool, "rot/rb").await.unwrap();
        assert!(got == "b.mp3" || got == "c.mp3", "must match rock or blues, got {got}");
    }

    #[tokio::test]
    async fn genre_has_all_requires_every_listed_genre() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("a.mp3", "A", 2000, &["jazz", "funk"]),
                media("b.mp3", "B", 2000, &["jazz"]),
                media("c.mp3", "C", 2000, &["jazz", "funk", "soul"]),
            ],
            1000,
        )
        .await
        .unwrap();
        // has_all [jazz, funk] → a or c, never b (missing funk).
        let toml = r#"
            name = "JazzAndFunk"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "genre"
            op = "has_all"
            value = ["jazz", "funk"]
        "#;
        add_playlist(&pool, "rot/jf", toml).await;
        let got = resolve_ref(&pool, "rot/jf").await.unwrap();
        assert!(got == "a.mp3" || got == "c.mp3", "must have both jazz and funk, got {got}");
    }

    #[tokio::test]
    async fn genre_has_none_excludes_any_listed_genre() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("a.mp3", "A", 2000, &["jazz"]),
                media("b.mp3", "B", 2000, &["rock"]),
                media("c.mp3", "C", 2000, &["jazz", "rock"]),
            ],
            1000,
        )
        .await
        .unwrap();
        // has_none [rock] → only a (b and c carry rock; the empty-set case would
        // also pass, none here).
        let toml = r#"
            name = "NoRock"
            [selection]
            mode = "dynamic"
            order = "shuffle"
            [[selection.filter]]
            field = "genre"
            op = "has_none"
            value = ["rock"]
        "#;
        add_playlist(&pool, "rot/norock", toml).await;
        assert_eq!(resolve_ref(&pool, "rot/norock").await.unwrap(), "a.mp3");
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
        "#;
        add_playlist(&pool, "jingles", toml).await;
        assert_eq!(resolve_ref(&pool, "jingles").await.unwrap(), "jingles/id-01.wav");
    }

    #[tokio::test]
    async fn group_weighted_draws_from_its_members() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("a/x.mp3", "", 0, &[]), media("b/x.mp3", "", 0, &[])],
            1000,
        )
        .await
        .unwrap();
        add_letter_leaves(&pool, &["a", "b"]).await;
        add_playlist(
            &pool,
            "w",
            r#"
                name = "W"
                [selection]
                mode = "group"
                strategy = "weighted"
                members = [{ ref = "a", weight = 1 }, { ref = "b", weight = 1 }]
            "#,
        )
        .await;
        let got = resolve_ref(&pool, "w").await.unwrap();
        assert!(got == "a/x.mp3" || got == "b/x.mp3", "weighted picks a member, got {got}");
    }

    #[tokio::test]
    async fn group_weighted_excludes_zero_weight() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("a/x.mp3", "", 0, &[]), media("b/x.mp3", "", 0, &[])],
            1000,
        )
        .await
        .unwrap();
        add_letter_leaves(&pool, &["a", "b"]).await;
        // a has weight 0 → excluded from the draw; every pick must be b.
        add_playlist(
            &pool,
            "w0",
            r#"
                name = "W0"
                [selection]
                mode = "group"
                strategy = "weighted"
                members = [{ ref = "a", weight = 0 }, { ref = "b", weight = 3 }]
            "#,
        )
        .await;
        for _ in 0..8 {
            assert_eq!(resolve_ref(&pool, "w0").await.unwrap(), "b/x.mp3");
        }
    }

    #[tokio::test]
    async fn group_weighted_skip_redraws_past_an_empty_member() {
        let (_d, pool) = fresh_db().await;
        // Only b has media; a's pool is empty.
        media_index::replace_library(&pool, &[media("b/x.mp3", "", 0, &[])], 1000)
            .await
            .unwrap();
        add_letter_leaves(&pool, &["a", "b"]).await;
        // a is heavier but empty; with skip the draw must fall back to b.
        add_playlist(
            &pool,
            "wskip",
            r#"
                name = "WSkip"
                [selection]
                mode = "group"
                strategy = "weighted"
                on_member_unavailable = "skip"
                members = [{ ref = "a", weight = 5 }, { ref = "b", weight = 1 }]
            "#,
        )
        .await;
        for _ in 0..8 {
            assert_eq!(resolve_ref(&pool, "wskip").await.unwrap(), "b/x.mp3");
        }
    }

    #[tokio::test]
    async fn group_weighted_abort_bubbles_when_drawn_member_is_empty() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[media("b/x.mp3", "", 0, &[])], 1000)
            .await
            .unwrap();
        add_letter_leaves(&pool, &["a", "b"]).await;
        // b excluded (weight 0); the sole eligible draw is the empty a → abort
        // (default) → PoolEmpty bubbles up (the grid would fall through).
        add_playlist(
            &pool,
            "wabort",
            r#"
                name = "WAbort"
                [selection]
                mode = "group"
                strategy = "weighted"
                members = [{ ref = "a", weight = 5 }, { ref = "b", weight = 0 }]
            "#,
        )
        .await;
        assert!(matches!(
            resolve_ref(&pool, "wabort").await,
            Err(SelectionError::PoolEmpty)
        ));
    }

    #[tokio::test]
    async fn group_rotate_round_robins_one_per_member() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("a/x.mp3", "", 0, &[]),
                media("b/x.mp3", "", 0, &[]),
                media("c/x.mp3", "", 0, &[]),
            ],
            1000,
        )
        .await
        .unwrap();
        add_letter_leaves(&pool, &["a", "b", "c"]).await;
        add_playlist(
            &pool,
            "rot",
            r#"
                name = "Rot"
                [selection]
                mode = "group"
                strategy = "rotate"
                members = [{ ref = "a" }, { ref = "b" }, { ref = "c" }]
            "#,
        )
        .await;
        // One track per member, in declared order, wrapping. Each leaf has a
        // single file, so the pick within a member is deterministic.
        assert_eq!(resolve_ref(&pool, "rot").await.unwrap(), "a/x.mp3");
        assert_eq!(resolve_ref(&pool, "rot").await.unwrap(), "b/x.mp3");
        assert_eq!(resolve_ref(&pool, "rot").await.unwrap(), "c/x.mp3");
        assert_eq!(resolve_ref(&pool, "rot").await.unwrap(), "a/x.mp3");
    }

    #[tokio::test]
    async fn unknown_ref_is_a_loud_error() {
        let (_d, pool) = fresh_db().await;
        assert!(matches!(
            resolve_ref(&pool, "ghost").await,
            Err(SelectionError::PlaylistNotFound(_))
        ));
    }

    #[tokio::test]
    async fn resolves_a_top_level_ref_case_insensitively() {
        // Regression: a grid ref written "Filler" must resolve to the view key
        // "filler" (the view is keyed by the lowercased `normalize_ref`). Before
        // the fix the lookup was case-sensitive and missed — while the apply
        // check, which normalizes, had passed.
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[media("pop/a.mp3", "A", 2020, &["pop"])], 1000)
            .await
            .unwrap();
        add_playlist(
            &pool,
            "filler", // stored lowercased, as `sync` keys it
            r#"
                name = "Filler"
                [selection]
                mode = "dynamic"
                order = "shuffle"
                [[selection.filter]]
                field = "path"
                op = "prefix"
                value = "pop/"
            "#,
        )
        .await;
        assert_eq!(resolve_ref(&pool, "Filler").await.unwrap(), "pop/a.mp3");
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
        "#;
        add_playlist(&pool, "rot/seq", toml).await;

        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "a.mp3");
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "b.mp3");
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "c.mp3");
        assert_eq!(resolve_ref(&pool, "rot/seq").await.unwrap(), "a.mp3");
    }

    #[tokio::test]
    async fn static_sequential_follows_declared_order_not_lexical() {
        let (_d, pool) = fresh_db().await;
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
        "#;
        add_playlist(&pool, "show", toml).await;

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
        "#;
        add_playlist(&pool, "pod/latest", toml).await;

        assert_eq!(resolve_ref(&pool, "pod/latest").await.unwrap(), "pod/ep003.mp3");
        assert_eq!(resolve_ref(&pool, "pod/latest").await.unwrap(), "pod/ep003.mp3");

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
        "#;
        add_playlist(&pool, "p", published).await;
        assert!(matches!(
            resolve_ref(&pool, "p").await,
            Err(SelectionError::Unsupported(_))
        ));
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
            "#,
        )
        .await;

        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "intros/i1.mp3");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "pod/ep002.mp3");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "outros/o1.mp3");
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
            "#,
        )
        .await;

        assert!(resolve_ref(&pool, "seqshow").await.unwrap().starts_with("rock/"));
        assert!(resolve_ref(&pool, "seqshow").await.unwrap().starts_with("rock/"));
        assert_eq!(resolve_ref(&pool, "seqshow").await.unwrap(), "jingles/j.wav");
        assert!(resolve_ref(&pool, "seqshow").await.unwrap().starts_with("rock/"));
    }

    #[tokio::test]
    async fn nested_group_is_rejected_for_now() {
        let (_d, pool) = fresh_db().await;
        add_playlist(
            &pool,
            "inner",
            r#"
                name = "Inner"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "leaf" }]
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
            "#,
        )
        .await;
        assert!(matches!(
            resolve_ref(&pool, "outer").await,
            Err(SelectionError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn group_sequence_skip_drops_an_empty_member() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[media("intros/i1.mp3", "", 0, &[]), media("outros/o1.mp3", "", 0, &[])],
            1000,
        )
        .await
        .unwrap();
        // podcast pool (pod/) is empty on purpose.
        for (r, prefix) in [("intro", "intros/"), ("podcast", "pod/"), ("outro", "outros/")] {
            add_playlist(
                &pool,
                r,
                &format!(
                    "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                     [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{prefix}\"\n"
                ),
            )
            .await;
        }
        add_playlist(
            &pool,
            "show",
            r#"
                name = "Show"
                [selection]
                mode = "group"
                strategy = "sequence"
                on_member_unavailable = "skip"
                members = [{ ref = "intro" }, { ref = "podcast" }, { ref = "outro" }]
            "#,
        )
        .await;

        // intro, then the empty podcast is skipped → outro, then wrap to intro.
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "intros/i1.mp3");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "outros/o1.mp3");
        assert_eq!(resolve_ref(&pool, "show").await.unwrap(), "intros/i1.mp3");
    }

    #[tokio::test]
    async fn group_sequence_abort_bubbles_pool_empty() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(&pool, &[media("intros/i1.mp3", "", 0, &[])], 1000)
            .await
            .unwrap();
        for (r, prefix) in [("intro", "intros/"), ("podcast", "pod/")] {
            add_playlist(
                &pool,
                r,
                &format!(
                    "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                     [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{prefix}\"\n"
                ),
            )
            .await;
        }
        // Default (abort): no on_member_unavailable field.
        add_playlist(
            &pool,
            "showabort",
            r#"
                name = "Abort"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "intro" }, { ref = "podcast" }]
            "#,
        )
        .await;

        // intro first turn; then the empty podcast aborts → PoolEmpty bubbles up
        // (the grid would fall through to the floor).
        assert_eq!(resolve_ref(&pool, "showabort").await.unwrap(), "intros/i1.mp3");
        assert!(matches!(
            resolve_ref(&pool, "showabort").await,
            Err(SelectionError::PoolEmpty)
        ));
    }

    // ----- group shuffle ----------------------------------------------

    /// Register N single-file leaf playlists `a`, `b`, … each keyed to a
    /// distinct top-level directory, and a group over them.
    async fn add_letter_leaves(pool: &SqlitePool, letters: &[&str]) {
        for r in letters {
            add_playlist(
                pool,
                r,
                &format!(
                    "name = \"{r}\"\n[selection]\nmode = \"dynamic\"\norder = \"shuffle\"\n\
                     [[selection.filter]]\nfield = \"path\"\nop = \"prefix\"\nvalue = \"{r}/\"\n"
                ),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn group_shuffle_visits_each_member_once_per_cycle() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("a/x.mp3", "", 0, &[]),
                media("b/x.mp3", "", 0, &[]),
                media("c/x.mp3", "", 0, &[]),
            ],
            1000,
        )
        .await
        .unwrap();
        add_letter_leaves(&pool, &["a", "b", "c"]).await;
        add_playlist(
            &pool,
            "shuf",
            r#"
                name = "Shuf"
                [selection]
                mode = "group"
                strategy = "shuffle"
                members = [{ ref = "a" }, { ref = "b" }, { ref = "c" }]
            "#,
        )
        .await;

        // Two full cycles: each is a permutation → every member exactly once,
        // no repeat within a cycle (whatever the random order).
        for _ in 0..2 {
            let mut seen = HashSet::new();
            for _ in 0..3 {
                let got = resolve_ref(&pool, "shuf").await.unwrap();
                seen.insert(got.chars().next().unwrap()); // 'a' | 'b' | 'c'
            }
            assert_eq!(seen.len(), 3, "a cycle visits all three members once");
        }
    }

    // ----- per-member time budget (runtime) ---------------------------

    #[tokio::test]
    async fn group_sequence_runtime_budget_switches_after_elapsed() {
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
            "#,
        )
        .await;
        add_playlist(
            &pool,
            "budget",
            r#"
                name = "Budget"
                [selection]
                mode = "group"
                strategy = "sequence"
                members = [{ ref = "rock", runtime = "1m" }, { ref = "jingle", take = 1 }]
            "#,
        )
        .await;

        // t=1000: first rock track, the member's start is stamped.
        assert!(resolve_ref_at(&pool, 1000, "budget").await.unwrap().starts_with("rock/"));
        // t=1030 (30s < 60s budget): still rock.
        assert!(resolve_ref_at(&pool, 1030, "budget").await.unwrap().starts_with("rock/"));
        // t=1061 (61s ≥ 60s): budget elapsed → switch to the jingle this turn.
        assert_eq!(resolve_ref_at(&pool, 1061, "budget").await.unwrap(), "jingles/j.wav");
        // Jingle (take = 1) hands back → wrap → rock again on a fresh budget.
        assert!(resolve_ref_at(&pool, 1062, "budget").await.unwrap().starts_with("rock/"));
    }

    #[tokio::test]
    async fn group_shuffle_runtime_holds_a_member_then_moves_on() {
        let (_d, pool) = fresh_db().await;
        media_index::replace_library(
            &pool,
            &[
                media("a/1.mp3", "", 0, &[]),
                media("a/2.mp3", "", 0, &[]),
                media("b/1.mp3", "", 0, &[]),
                media("b/2.mp3", "", 0, &[]),
            ],
            1000,
        )
        .await
        .unwrap();
        add_letter_leaves(&pool, &["a", "b"]).await;
        add_playlist(
            &pool,
            "shufbud",
            r#"
                name = "ShufBud"
                [selection]
                mode = "group"
                strategy = "shuffle"
                members = [{ ref = "a", runtime = "1m" }, { ref = "b", runtime = "1m" }]
            "#,
        )
        .await;

        // First member of the drawn permutation, held within its 60s budget.
        let first = resolve_ref_at(&pool, 1000, "shufbud").await.unwrap();
        let first_letter = first.chars().next().unwrap();
        let again = resolve_ref_at(&pool, 1030, "shufbud").await.unwrap();
        assert_eq!(again.chars().next().unwrap(), first_letter, "same member within budget");
        // Budget elapsed → the other member takes over (only two in the cycle).
        let next = resolve_ref_at(&pool, 1061, "shufbud").await.unwrap();
        assert_ne!(next.chars().next().unwrap(), first_letter, "switches member after budget");
    }
}
